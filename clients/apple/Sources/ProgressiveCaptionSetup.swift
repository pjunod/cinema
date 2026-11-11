import Combine
import SwiftUI

struct CaptionSavedSettings: Decodable {
    let version: Int, revision: String, enabled: Bool
    let modelId: String?, profileId: String?, preferredSpokenLanguage: String?
    let serverNowMs: Int
}
struct CaptionWorkerCapability: Decodable {
    let modelId: String, profileId: String, status: String, reason: String?
}
struct CaptionWorker: Decodable, Identifiable {
    let nodeId: String, label: String
    let current: Bool, eligible: Bool
    let capabilities: [CaptionWorkerCapability]
    var id: String { nodeId }
}
struct CaptionModelCatalog: Decodable {
    let version: Int, profiles: [CaptionProfile], workers: [CaptionWorker], serverNowMs: Int
}
struct CaptionModelOperation: Decodable, Identifiable {
    let nodeId: String, outcome: String, jobId: String?, reason: String?
    var id: String { nodeId }
}
struct CaptionModelReceipt: Decodable {
    let version: Int, purpose: String, profileId: String, modelId: String
    let operations: [CaptionModelOperation]
    let serverNowMs: Int?
}
struct CaptionModelJob: Decodable {
    let state: String, phase: String?, reason: String?
}

@MainActor
final class ProgressiveCaptionSetup: ObservableObject {
    @Published var enabled = false
    @Published var profileId = ""
    @Published var language = ""
    @Published var workers: Set<String> = []
    @Published private(set) var catalog: CaptionModelCatalog?
    @Published private(set) var status = "Loading caption setup…"
    @Published private(set) var modelStatus = ""
    @Published private(set) var operations: [CaptionModelOperation] = []
    @Published private(set) var jobs: [String: CaptionModelJob] = [:]
    @Published private(set) var modelBusy = false
    @Published private(set) var saving = false
    private var api: PlurxAPI?
    private var saved: CaptionSavedSettings?
    private var clock: (Int, TimeInterval)?
    private var auth: UInt64 = 0
    private var generation = 0
    private var poll: Task<Void, Never>?
    private var intents: [String: [String: CaptionValue]] = [:]
    private var intentKeys: [String: String] = [:]
    var profiles: [CaptionProfile] { catalog?.profiles ?? [] }
    var profile: CaptionProfile? { profiles.first { $0.profileId == profileId } }
    var languages: [String] { profile?.inputLanguages ?? [] }

    func load(_ api: PlurxAPI) async {
        auth = Session.shared.playbackAuthorization.generation
        let api = api.captionOwner(generation: auth)
        self.api = api
        let captured = generation
        do {
            let reply: CaptionHTTPReply<CaptionSavedSettings> = try await api.caption("subtitle-translation")
            guard current(captured) else { return }
            guard reply.value.version == 1, CaptionValidation.digest(reply.value.revision) else { throw CaptionProtocolError.invalid }
            saved = reply.value; clock = (reply.value.serverNowMs, ProcessInfo.processInfo.systemUptime)
            enabled = reply.value.enabled; profileId = reply.value.profileId ?? ""; language = reply.value.preferredSpokenLanguage ?? ""
            status = "Saved choices are independent of model readiness."
            await refresh()
        } catch { if current(captured) { status = error.localizedDescription } }
    }
    private func current(_ captured: Int) -> Bool { captured == generation && auth == Session.shared.playbackAuthorization.generation && !Task.isCancelled }
    private func envelope() throws -> [String: CaptionValue] {
        guard let clock else { throw CaptionProtocolError.expired }
        let elapsed = max(0, ProcessInfo.processInfo.systemUptime - clock.1)
        guard elapsed < 240 else { throw CaptionProtocolError.expired }
        return ["version": .integer(1), "request_id": .string(UUID().uuidString.lowercased()),
                "intent_expires_at_ms": .integer(clock.0 + Int(elapsed * 1000) + 60_000)]
    }
    func refresh() async {
        guard let api else { return }
        let captured = generation
        do {
            let reply: CaptionHTTPReply<CaptionModelCatalog> = try await api.caption("subtitle-translation/models")
            guard current(captured) else { return }
            guard reply.value.version == 1, reply.value.profiles.count <= 16, reply.value.workers.count <= 32,
                  reply.value.workers.allSatisfy({ !$0.nodeId.isEmpty && $0.nodeId.utf8.count <= 128 && $0.label.utf8.count <= 256 && $0.capabilities.count <= 16 }) else { throw CaptionProtocolError.invalid }
            for profile in reply.value.profiles { try profile.validate() }
            catalog = reply.value; clock = (reply.value.serverNowMs, ProcessInfo.processInfo.systemUptime)
            if profileId.isEmpty { profileId = profiles.first?.profileId ?? "" }
            // An explicit choice survives refresh; stale worker facts remain advisory.
            if workers.isEmpty { workers = Set(reply.value.workers.filter { $0.current && $0.eligible }.prefix(1).map(\.nodeId)) }
            modelStatus = "Download and test a model, then Save your enable choice."
        } catch { if current(captured) { modelStatus = error.localizedDescription } }
    }
    func save() async {
        guard !saving, let api, let saved else { return }
        saving = true; defer { saving = false }
        let captured = generation
        let desiredProfile = profile, desiredProfileId = profileId, desiredLanguage = language, desiredEnabled = enabled
        let key = "\(desiredEnabled):\(desiredProfileId):\(desiredLanguage)"
        if intentKeys["save"] != key { intents["save"] = nil; intentKeys["save"] = key }
        do {
            if intents["save"] == nil, clock.map({ ProcessInfo.processInfo.systemUptime - $0.1 >= 240 }) ?? true { await refresh() }
            guard current(captured) else { return }
            var body = try intents["save"] ?? envelope()
            if intents["save"] == nil {
                body["expected_revision"] = .string(saved.revision); body["enabled"] = .bool(desiredEnabled)
                body["profile_id"] = desiredProfileId.isEmpty ? .null : .string(desiredProfileId)
                body["model_id"] = desiredProfile.map { .string($0.modelId) } ?? saved.modelId.map { .string($0) } ?? .null
                body["preferred_spoken_language"] = desiredLanguage.isEmpty ? .null : .string(desiredLanguage)
                intents["save"] = body
            }
            let reply: CaptionHTTPReply<CaptionSavedSettings> = try await api.caption("subtitle-translation", method: "PUT", body: body)
            guard current(captured) else { return }
            guard reply.value.version == 1, CaptionValidation.digest(reply.value.revision) else { throw CaptionProtocolError.invalid }
            self.saved = reply.value; intents["save"] = nil
            clock = (reply.value.serverNowMs, ProcessInfo.processInfo.systemUptime)
            status = "Saved. Your current controls were kept."
        } catch {
            if current(captured) {
                clearDefinite(error, key: "save")
                status = (error as? CaptionServiceError)?.status == 409 ? "Settings changed elsewhere. Reload the saved revision before saving; your controls were kept." : error.localizedDescription
            }
        }
    }
    func reloadRevision() async {
        guard let api else { return }
        let captured = generation
        do {
            let reply: CaptionHTTPReply<CaptionSavedSettings> = try await api.caption("subtitle-translation")
            guard current(captured), CaptionValidation.digest(reply.value.revision) else { return }
            saved = reply.value; clock = (reply.value.serverNowMs, ProcessInfo.processInfo.systemUptime)
            intents["save"] = nil; status = "Saved revision reloaded. Your unsaved controls were kept."
        } catch { if current(captured) { status = error.localizedDescription } }
    }
    func model(_ action: String) async {
        guard !modelBusy, let api, let profile, ["install", "verify"].contains(action), !workers.isEmpty, workers.count <= 16 else { return }
        modelBusy = true
        let captured = generation, nodeIds = workers.sorted()
        let key = "\(action):\(profile.profileId):\(nodeIds.joined(separator: ","))"
        if intentKeys["model"] != key { intents["model"] = nil; intentKeys["model"] = key }
        do {
            if intents["model"] == nil, clock.map({ ProcessInfo.processInfo.systemUptime - $0.1 >= 240 }) ?? true { await refresh() }
            guard current(captured) else { return }
            var body = try intents["model"] ?? envelope()
            body["version"] = .integer(2); body["purpose"] = .string("translation")
            body["profile_id"] = .string(profile.profileId)
            // The wire uses an array; an explicit request type preserves it.
            body["node_ids"] = .array(nodeIds)
            self.intents["model"] = body
            let reply: CaptionHTTPReply<CaptionModelReceipt> = try await api.caption("subtitle-transcription/models/\(profile.modelId)/\(action)", method: "POST", body: body)
            guard current(captured) else { return }
            guard reply.value.version == 2, reply.value.purpose == "translation", reply.value.profileId == profile.profileId,
                  reply.value.modelId == profile.modelId, reply.value.operations.count <= 16,
                  reply.value.operations.allSatisfy({ $0.jobId.map(CaptionValidation.uuid) ?? true }) else { throw CaptionProtocolError.invalid }
            operations = reply.value.operations; jobs = [:]; intents["model"] = nil
            modelStatus = "Request accepted. Follow each worker's actual job state below."
            startPoll(captured)
        } catch { if current(captured) { clearDefinite(error, key: "model"); modelBusy = false; modelStatus = error.localizedDescription } }
    }
    private func startPoll(_ captured: Int) {
        poll?.cancel()
        poll = Task { [weak self] in
            while let self, self.current(captured), let api = self.api {
                do {
                    for row in self.operations where ["accepted", "existing"].contains(row.outcome) {
                        guard let id = row.jobId else { continue }
                        let reply: CaptionHTTPReply<CaptionModelJob> = try await api.caption("subtitle-transcription/jobs/\(id)")
                        guard self.current(captured) else { return }
                        self.jobs[id] = reply.value
                    }
                    let unfinished = self.operations.contains { row in
                        guard let id = row.jobId, ["accepted", "existing"].contains(row.outcome) else { return false }
                        return !["succeeded", "failed", "cancelled"].contains(self.jobs[id]?.state ?? "queued")
                    }
                    self.modelBusy = unfinished
                    if !unfinished { await self.refresh(); return }
                } catch { self.modelStatus = "Job status unavailable. Refresh status or follow the existing job in Activity."; return }
                do { try await Task.sleep(for: .seconds(3)) } catch { return }
            }
        }
    }
    func refreshJobs() { startPoll(generation) }
    func close() { generation += 1; poll?.cancel(); poll = nil }
    private func clearDefinite(_ error: Error, key: String) {
        if let error = error as? CaptionServiceError, !error.ambiguous { intents[key] = nil }
    }
}

struct ProgressiveCaptionSetupView: View {
    @EnvironmentObject var model: AppModel
    @StateObject private var setup = ProgressiveCaptionSetup()
    @StateObject private var textTranslation = SubtitleTextTranslation()
    var body: some View {
        Form {
            Section { TextCaptionModelSetup(owner: textTranslation) } header: { Text("Existing subtitle translation") }
            Section {
                Toggle("Enable progressive captions", isOn: $setup.enabled)
                Picker("Model and task", selection: $setup.profileId) {
                    Text("Choose a model").tag("")
                    if !setup.profileId.isEmpty, !setup.profiles.contains(where: { $0.profileId == setup.profileId }) { Text("Saved model · catalog unavailable").tag(setup.profileId) }
                    ForEach(setup.profiles) { Text("\($0.modelId) · \($0.task.label)").tag($0.profileId) }
                }
                Picker("Preferred spoken language", selection: $setup.language) {
                    Text("Choose for each movie").tag("")
                    if !setup.language.isEmpty, !setup.languages.contains(setup.language) { Text("Saved language: " + setup.language).tag(setup.language) }
                    ForEach(setup.languages, id: \.self) { Text(Locale.current.localizedString(forLanguageCode: $0) ?? $0).tag($0) }
                }
                Button("Save") { Task { await setup.save() } }.disabled(setup.saving)
                Text(setup.status)
                Button("Reload saved revision, keep my controls") { Task { await setup.reloadRevision() } }
            } footer: { Text("Enablement is saved even when a model is missing or a worker is busy. In the player, choose the actual audio and spoken language. Captions become available progressively; a slow worker can lag behind.") }
            Section {
                ForEach(setup.catalog?.workers ?? []) { worker in
                    Toggle(isOn: Binding(get: { setup.workers.contains(worker.nodeId) }, set: { if $0 { setup.workers.insert(worker.nodeId) } else { setup.workers.remove(worker.nodeId) } })) {
                        VStack(alignment: .leading) {
                            Text(worker.label)
                            Text(worker.current ? worker.eligible ? "Current worker" : "Currently unavailable" : "Status expired").font(.caption)
                            ForEach(worker.capabilities.filter { $0.profileId == setup.profileId }, id: \.profileId) { capability in
                                Text(capability.status.replacingOccurrences(of: "_", with: " ")).font(.caption)
                                if let reason = capability.reason { Text(CaptionServiceError.message(reason)).font(.caption) }
                            }
                        }
                    }
                }
                Button("Download and test") { Task { await setup.model("install") } }.disabled(setup.modelBusy)
                Button("Test selected model") { Task { await setup.model("verify") } }.disabled(setup.modelBusy)
                Button("Refresh worker status") { Task { await setup.refresh() } }
                Text(setup.modelStatus)
                ForEach(setup.operations) { row in
                    VStack(alignment: .leading) {
                        Text(setup.catalog?.workers.first { $0.nodeId == row.nodeId }?.label ?? row.nodeId)
                        if let job = row.jobId, let state = setup.jobs[job] {
                            Text((state.phase ?? state.state).replacingOccurrences(of: "_", with: " "))
                            if let reason = state.reason { Text(CaptionServiceError.message(reason)) }
                        } else { Text(row.outcome.replacingOccurrences(of: "_", with: " ")) }
                        if let id = row.jobId, let url = URL(string: model.requireAPI().origin + "/#/activity/jobs/" + id) { Link("Follow job in Activity", destination: url) }
                    }
                }
                if !setup.operations.isEmpty { Button("Refresh existing jobs") { setup.refreshJobs() } }
            } header: { Text("Model setup") }
        }
        .navigationTitle("Caption tools")
        .task { textTranslation.configure(api: model.requireAPI()); await setup.load(model.requireAPI()) }
        .onDisappear { setup.close(); textTranslation.retire() }
    }
}
