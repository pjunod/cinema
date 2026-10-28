import Combine
import SwiftUI

struct TextCaptionConfiguration: Decodable {
    let version: Int, revision: String, enabled: Bool, modelId: String, targetLanguage: String
}
struct TextCaptionSource: Decodable, Identifiable {
    let index: Int, language: String?, title: String?, textEligible: Bool
    let downloadedSubtitleId: String?
    var id: Int { index }
    var label: String { title ?? language ?? "Subtitle \(index + 1)" }
}
struct TextCaptionWorker: Decodable, Identifiable {
    let nodeId: String, label: String, bootId: String, modelId: String, status: String, reason: String?
    var id: String { nodeId }
}
struct TextCaptionPermissions: Decodable { let read: Bool, create: Bool }
struct TextCaptionOptions: Decodable {
    let version: Int, fileId: String, sourceSize: String, sourceMtime: String
    let subtitles: [TextCaptionSource], configuration: TextCaptionConfiguration
    let modelId: String, workers: [TextCaptionWorker], permissions: TextCaptionPermissions
    let inputLanguages: [String]?
}
struct TextCaptionCatalog: Decodable { let inputLanguages: [String]? }
struct TextCaptionModels: Decodable {
    let version: Int, workers: [TextCaptionWorker]
    let inputLanguages: [String]?
    let catalog: TextCaptionCatalog?
}
struct TextCaptionReceipt: Decodable { let version: Int, jobId: String, state: String }
struct TextCaptionResult: Decodable {
    let fileId: String, subtitleIndex: Int, downloadedSubtitleId: String, language: String
}
struct TextCaptionProgress: Decodable { let completed: Int, total: Int }
struct TextCaptionJob: Decodable {
    let version: Int, jobId: String, state: String, phase: String?, reason: String?, retryable: Bool
    let progress: TextCaptionProgress?
    let result: TextCaptionResult?
}

/// Complete existing-text translation is a durable job, independent of the
/// progressive speech demand and its Stop epoch. Completion never selects it.
@MainActor
final class SubtitleTextTranslation: ObservableObject {
    @Published private(set) var options: TextCaptionOptions?
    @Published private(set) var workers: [TextCaptionWorker] = []
    @Published private(set) var inputLanguages: [String] = []
    @Published private(set) var job: TextCaptionJob?
    @Published private(set) var status = "Translate an existing text subtitle to English."
    @Published private(set) var busy = false
    @Published var enabled = false
    @Published var sourceIndex = -1
    @Published var sourceLanguage = ""
    @Published var workerId = ""
    private var configuration: TextCaptionConfiguration?
    private var api: PlurxAPI?
    private var fileId: Int?
    private var generation = 0
    private var auth: UInt64 = 0
    private var authObserver: UUID?
    private var loop: Task<Void, Never>?
    private let networkTasks=CaptionNetworkTasks()
    private var pending: [String: CaptionValue]?
    private var pendingKey: String?
    private var jobId: String?
    var active: Bool { jobId != nil && !["succeeded", "failed", "cancelled"].contains(job?.state ?? "queued") }
    var origin: String { api?.origin ?? "" }
    var activeJobId: String? { jobId }
    var sources: [TextCaptionSource] { options?.subtitles ?? [] }

    func configure(api: PlurxAPI, fileId: Int? = nil) {
        let authorization = Session.shared.playbackAuthorization.generation
        if self.api != nil, self.fileId == fileId, auth == authorization { return }
        retire()
        self.api = api.captionOwner(generation: authorization); self.fileId = fileId; auth = authorization
        let observation = Session.shared.observeAuthorizationChanges { [weak self] next in
            Task { @MainActor in if let self, self.auth != next { self.retire() } }
        }
        authObserver = observation.id
        guard observation.generation == auth else { retire(); return }
        let captured = generation
        networkTasks.run { [self] in await refresh(captured, initialize: true) }
    }
    private func current(_ captured: Int) -> Bool {
        generation == captured && auth == Session.shared.playbackAuthorization.generation && !Task.isCancelled
    }
    private func validate(_ config: TextCaptionConfiguration) throws {
        guard config.version == 1, CaptionValidation.counter(config.revision), config.modelId == "opus-mt-mul-en", config.targetLanguage == "en" else { throw CaptionProtocolError.invalid }
    }
    private func refresh(_ captured: Int, initialize: Bool = false) async {
        guard let api else { return }
        do {
            if let fileId {
                let reply: CaptionHTTPReply<TextCaptionOptions> = try await api.caption("files/\(fileId)/subtitle-text-translation/options")
                guard current(captured) else { return }
                let value = reply.value
                guard value.version == 1, value.fileId == String(fileId), CaptionValidation.counter(value.sourceSize),
                      Int64(value.sourceMtime) != nil, value.subtitles.count <= 4096, value.subtitles.allSatisfy({ (0...999999).contains($0.index) }), value.workers.count <= 32 else { throw CaptionProtocolError.invalid }
                try validate(value.configuration)
                options = value; configuration = value.configuration; workers = value.workers
                if initialize {
                    enabled = value.configuration.enabled
                    sourceIndex = value.subtitles.first(where: \.textEligible)?.index ?? -1
                    let language = PlayerController.subtitleLanguageTag(value.subtitles.first(where: { $0.index == sourceIndex })?.language)
                    sourceLanguage = language == "und" ? "" : language
                }
            } else {
                let saved: CaptionHTTPReply<TextCaptionConfiguration> = try await api.caption("subtitle-text-translation/config")
                guard current(captured) else { return }
                try validate(saved.value); configuration = saved.value
                if initialize { enabled = saved.value.enabled }
            }
            let models: CaptionHTTPReply<TextCaptionModels> = try await api.caption("subtitle-text-translation/models")
            guard current(captured), models.value.version == 1, models.value.workers.count <= 32 else { return }
            workers = models.value.workers
            inputLanguages = models.value.inputLanguages ?? models.value.catalog?.inputLanguages ?? options?.inputLanguages ?? []
            guard inputLanguages.count <= 256, inputLanguages.allSatisfy({ !$0.isEmpty && $0.utf8.count <= 16 }) else { throw CaptionProtocolError.invalid }
            guard workers.allSatisfy({ !$0.nodeId.isEmpty && $0.nodeId.utf8.count <= 128 && !$0.bootId.isEmpty && $0.bootId.utf8.count <= 128 && $0.label.utf8.count <= 256 }) else { throw CaptionProtocolError.invalid }
            if workerId.isEmpty { workerId = workers.first?.nodeId ?? "" }
            status = "Model readiness is advisory to your saved enable choice."
        } catch { fail(error, captured) }
    }
    func refresh() { networkTasks.run { [self] in await refresh(generation) } }
    func save() {
        guard !busy, let configuration else { return }
        send(key: "save:\(enabled):\(configuration.revision)", path: "subtitle-text-translation/config", method: "PUT",
             body: ["version": .integer(1), "expected_revision": .string(configuration.revision), "enabled": .bool(enabled), "model_id": .string("opus-mt-mul-en")], save: true)
    }
    func install(_ action: String) {
        guard !active, !busy, ["install", "verify"].contains(action), let worker = workers.first(where: { $0.nodeId == workerId }) else { return }
        send(key: "model:\(action):\(worker.nodeId):\(worker.bootId)", path: "subtitle-text-translation/models/\(action)", body: ["version": .integer(1), "request_id": .string(UUID().uuidString.lowercased()), "node_id": .string(worker.nodeId), "boot_id": .string(worker.bootId), "model_id": .string("opus-mt-mul-en")])
    }
    func translate() {
        guard !active, !busy, let fileId, let options, options.permissions.create,
              options.subtitles.contains(where: { $0.index == sourceIndex && $0.textEligible }), !sourceLanguage.isEmpty else {
            status = "Choose an existing text subtitle and its actual source language. Bitmap subtitles can use speech generation instead."
            return
        }
        send(key: "translate:\(sourceIndex):\(sourceLanguage):\(options.sourceSize):\(options.sourceMtime)",
             path: "files/\(fileId)/subtitles/\(sourceIndex)/translation",
             body: ["version": .integer(1), "request_id": .string(UUID().uuidString.lowercased()),
                    "source_size": .string(options.sourceSize), "source_mtime": .string(options.sourceMtime),
                    "source_language": .string(sourceLanguage), "target_language": .string("en"), "model_id": .string("opus-mt-mul-en")])
    }
    private func send(key: String, path: String, method: String = "POST", body: [String: CaptionValue], save: Bool = false) {
        guard let api else { return }
        if pendingKey != key { pending = nil; pendingKey = key }
        if pending == nil { pending = body }
        let request = pending
        busy = true
        let captured = generation
        networkTasks.run { [self] in
            defer { if current(captured) { busy = false } }
            do {
                if save {
                    let reply: CaptionHTTPReply<TextCaptionConfiguration> = try await api.caption(path, method: method, body: request)
                    guard current(captured) else { return }
                    try validate(reply.value); configuration = reply.value; pending = nil
                    status = "Saved. Current controls were kept."
                } else {
                    let reply: CaptionHTTPReply<TextCaptionReceipt> = try await api.caption(path, method: method, body: request)
                    guard current(captured), reply.value.version == 1, CaptionValidation.uuid(reply.value.jobId) else { return }
                    jobId = reply.value.jobId; job = nil; pending = nil
                    status = "Queued. Follow the actual job state below."
                    startLoop(captured)
                }
            } catch { fail(error, captured) }
        }
    }
    private func startLoop(_ captured: Int) {
        loop?.cancel()
        loop = networkTasks.run { [weak self] in
            while let self, self.current(captured), let api = self.api, let jobId = self.jobId {
                do {
                    let reply: CaptionHTTPReply<TextCaptionJob> = try await api.caption("subtitle-text-translation/jobs/\(jobId)")
                    guard self.current(captured), reply.value.version == 1, reply.value.jobId == jobId else { return }
                    if let result = reply.value.result {
                        guard result.fileId == self.fileId.map(String.init), result.language == "en", (0...999999).contains(result.subtitleIndex),
                              CaptionValidation.digest(result.downloadedSubtitleId) else { throw CaptionProtocolError.invalid }
                    }
                    self.job = reply.value
                    self.status = reply.value.reason.map { CaptionServiceError.message($0) }
                        ?? (reply.value.state == "succeeded" ? reply.value.result == nil ? "Model job complete. Refresh worker verification status." : "Translation complete. Choose Use translated captions when you want to switch." : (reply.value.phase ?? reply.value.state).replacingOccurrences(of: "_", with: " "))
                    if ["succeeded", "failed", "cancelled"].contains(reply.value.state) { return }
                } catch { self.fail(error, captured); return }
                do { try await Task.sleep(for: .seconds(2)) } catch { return }
            }
        }
    }
    func refreshJob() { if jobId != nil { startLoop(generation) } }
    func cancel() {
        guard let api, let jobId else { return }
        let captured = generation
        networkTasks.run { [self] in
            do {
                let _: CaptionHTTPReply<Data> = try await api.captionBytes("cluster/jobs/\(jobId)/cancel", method: "POST")
                guard current(captured) else { return }; refreshJob()
            } catch { fail(error, captured) }
        }
    }
    func retry() {
        guard !busy, let jobId, job?.retryable == true else { return }
        send(key: "retry:\(jobId)", path: "subtitle-text-translation/jobs/\(jobId)/retry", body: ["version": .integer(1), "request_id": .string(UUID().uuidString.lowercased())])
    }
    func retire() {
        generation += 1; networkTasks.cancelWork();loop?.cancel(); loop = nil
        if let authObserver { Session.shared.removeAuthorizationObserver(authObserver) }
        authObserver = nil; api = nil; fileId = nil; options = nil; configuration = nil; workers = []
        job = nil; jobId = nil; busy = false; pending = nil; pendingKey = nil
    }
    func retireAndDrain() async {
        retire()
        await networkTasks.drain()
    }
    private func fail(_ error: Error, _ captured: Int) {
        guard current(captured), !(error is CancellationError) else { return }
        if let service = error as? CaptionServiceError {
            if !service.ambiguous { pending = nil }
            if service.status == 401 || service.status == 403 { retire() }
        }
        status = error.localizedDescription
    }
}

struct TextCaptionModelSetup: View {
    @ObservedObject var owner: SubtitleTextTranslation
    var body: some View {
        Toggle("Enable existing subtitle translation", isOn: $owner.enabled)
        Text("Marian translates existing text to English. Speech generation uses its own model above. Model readiness never changes the saved enable choice.").font(.caption)
        Button("Save text translation settings") { owner.save() }.disabled(owner.busy)
        Picker("Text translation worker", selection: $owner.workerId) {
            Text("Choose a worker").tag("")
            ForEach(owner.workers) { Text("\($0.label) · \($0.status.replacingOccurrences(of: "_", with: " "))").tag($0.nodeId) }
        }
        if let reason = owner.workers.first(where: { $0.nodeId == owner.workerId })?.reason { Text(CaptionServiceError.message(reason)).font(.caption) }
        Button("Download and test Marian") { owner.install("install") }.disabled(owner.busy || owner.active)
        Button("Test Marian") { owner.install("verify") }.disabled(owner.busy || owner.active)
        Text(owner.status)
        Button("Refresh text model status") { owner.refresh(); owner.refreshJob() }
        TextCaptionJobControls(owner: owner)
    }
}
struct TextCaptionJobControls: View {
    @ObservedObject var owner: SubtitleTextTranslation
    var body: some View {
        if let id = owner.activeJobId, let url = URL(string: owner.origin + "/#/activity/jobs/" + id) { Link("Follow job in Activity", destination: url) }
        if let job = owner.job {
            Text(job.state.replacingOccurrences(of: "_", with: " "))
            if let phase = job.phase, phase != job.state { Text(phase.replacingOccurrences(of: "_", with: " ")).font(.caption) }
            if let progress = job.progress, progress.total > 0, progress.completed >= 0, progress.completed <= progress.total {
                Text("\(progress.completed) of \(progress.total) cues")
            }
            if owner.active { Button("Stop text translation job") { owner.cancel() } }
            if job.retryable { Button("Retry text translation job") { owner.retry() }.disabled(owner.busy) }
        }
    }
}
struct TextCaptionTranslationControls: View {
    @ObservedObject var owner: SubtitleTextTranslation
    let use: (TextCaptionResult) -> Void
    var body: some View {
        Section {
            Picker("Existing subtitle", selection: $owner.sourceIndex) {
                Text("Choose a subtitle").tag(-1)
                ForEach(owner.sources) { source in
                    Text(source.label + (source.textEligible ? "" : " · bitmap; use speech generation")).tag(source.index)
                }
            }
            if owner.inputLanguages.isEmpty {
                TextField("Source language code, for example de", text: $owner.sourceLanguage)
            } else {
                Picker("Subtitle language", selection: $owner.sourceLanguage) {
                    Text("Choose source language").tag("")
                    if !owner.sourceLanguage.isEmpty, !owner.inputLanguages.contains(owner.sourceLanguage) { Text("Source language: " + owner.sourceLanguage).tag(owner.sourceLanguage) }
                    ForEach(owner.inputLanguages, id: \.self) { Text(Locale.current.localizedString(forLanguageCode: $0) ?? $0).tag($0) }
                }
            }
            Button("Translate existing subtitle to English") { owner.translate() }.disabled(owner.busy || owner.active)
            Text(owner.status)
            Text("Original cue times and captions are retained. Completion adds an ordinary English caption track; it does not change the selected track.").font(.caption)
            TextCaptionJobControls(owner: owner)
            if let result = owner.job?.result { Button("Use translated captions") { use(result) } }
            Button("Refresh existing subtitle translation") { owner.refresh(); owner.refreshJob() }
        } header: { Text("Translate existing text") }
        .onChange(of: owner.sourceIndex) { _, index in
            let language = PlayerController.subtitleLanguageTag(owner.sources.first(where: { $0.index == index })?.language)
            owner.sourceLanguage = language == "und" ? "" : language
        }
    }
}
