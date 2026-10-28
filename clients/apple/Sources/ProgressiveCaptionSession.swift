import AVFoundation
import Combine
import Foundation

/// Bounded-lifetime ownership for caption HTTP tasks. Completed handles are
/// removed; cancellation retains each handle until its operation has settled.
/// Cleanup keeps its captured principal and gets the existing HTTP/TTL bounds.
@MainActor
final class CaptionNetworkTasks {
    private struct Entry { let task: Task<Void,Never>;let cleanup:Bool }
    private var active:[UUID:Entry]=[:]
    var pendingCount:Int { active.count }
    @discardableResult
    func run(cleanup:Bool=false,_ operation:@escaping @MainActor () async -> Void) -> Task<Void,Never> {
        let identity=UUID()
        let task=Task { [weak self] in
            defer { self?.active[identity]=nil }
            guard cleanup || !Task.isCancelled else { return }
            await operation()
        }
        active[identity]=Entry(task:task,cleanup:cleanup)
        return task
    }
    func cancelWork() {
        for entry in active.values where !entry.cleanup { entry.task.cancel() }
    }
    func cancelAll() {
        for entry in active.values { entry.task.cancel() }
    }
    func drain() async {
        // Snapshot the retired owner's work; a later reconfiguration has its
        // own lifetime and cannot be removed or awaited as the old owner.
        let pending=active.values.map(\.task)
        for task in pending { await task.value }
    }
}

/// One playback's caption work and bounded source-clock read window. It owns
/// no AVPlayer, session replacement, media recovery or presentation timer.
@MainActor
final class ProgressiveCaptionSession: ObservableObject {
    @Published private(set) var options: CaptionOptions?
    @Published private(set) var status = "Choose a spoken language and caption action."
    @Published private(set) var coverageStatus = ""
    @Published private(set) var operation: CaptionOperation?
    @Published private(set) var window: CaptionRenderWindow?
    @Published private(set) var selected = false
    @Published private(set) var busy = false
    @Published private(set) var stopRequested = false
    @Published var profileId = ""
    @Published var spokenLanguage = ""
    @Published var subtitleOffsetMs = 0 { didSet { publishWindow() } }
    private(set) var assetId: String?
    @Published private var assetPage: [CaptionAsset] = []
    @Published private(set) var nextAssetCursor: String?
    @Published private var failurePage: CaptionFailurePage?
    var failedRanges: [CaptionFailedRange] {
        if let page = failurePage, page.operationId == operation?.operationId,
           page.operationEpoch == operation?.operationEpoch, page.viewRevision == operation?.viewRevision { return page.entries }
        return operation?.failedRanges ?? []
    }
    var nextFailureCursor: String? {
        if let page = failurePage, page.operationId == operation?.operationId,
           page.operationEpoch == operation?.operationEpoch, page.viewRevision == operation?.viewRevision { return page.nextCursor }
        return operation?.failedRangesNextCursor
    }
    private var api: PlurxAPI?
    private var fileId = 0
    private var sourceAudioIndex: Int?
    private var audioOrdinal: Int?
    private var generation = 0
    private var authGeneration: UInt64 = 0
    private var authObserver: UUID?
    private var loop: Task<Void, Never>?
    private var action: Task<Void, Never>?
    private var actionSerial = 0
    private let networkTasks=CaptionNetworkTasks()
    private var now: (() -> (position: Int, base: Int, rate: Double, paused: Bool))?
    private var isPlaybackCurrent: (() -> Bool)?
    private var playbackId = UUID().uuidString.lowercased()
    // Traversal authority is independent of the per-heartbeat CAS generation.
    private var continuityId = UUID().uuidString.lowercased()
    private var operationId: String?
    private var epoch = "0", view = "0"
    private var demandId: String?, demandGeneration: String?
    private var serverClock: (ms: Int, uptime: TimeInterval)?
    private var intents: [String: [String: CaptionValue]] = [:]
    private var lease: (id: String, revision: String, deadline: TimeInterval)?
    private var cues: [CaptionCue] = []
    private var ranges: [CaptionRange] = []
    private var renderRevision = 0
    private var lastHeartbeat: TimeInterval = 0
    private var windowGeneration = 0
    private var polling = false
    private var lastObserved: (rate: Double, paused: Bool)?
    private var seekDemandPosition: Int?
    private var chosenProfile: CaptionProfile?
    private var chosenLanguage: String?
    private var contentRevision: String?

    var profiles: [CaptionProfile] { options?.models ?? [] }
    var profile: CaptionProfile? { profiles.first { $0.profileId == profileId } }
    var languages: [String] { profile?.inputLanguages ?? [] }
    var stopped: Bool { stopRequested || operation?.state == "stopped" }
    var canStop: Bool { options?.permissions.stop == true && operationId != nil && !stopped }
    var canRetry: Bool { options?.permissions.resume == true && (stopped || operation?.retryable == true) }
    var compatibleAssets: [CaptionAsset] { assetPage.filter { $0.audioIndex == audioOrdinal } }

    func configure(api: PlurxAPI, fileId: Int, sourceAudioIndex: Int?,
                   now: @escaping () -> (Int, Int, Double, Bool), current: @escaping () -> Bool) {
        retire()
        self.authGeneration = Session.shared.playbackAuthorization.generation
        self.api = api.captionOwner(generation: authGeneration)
        self.fileId = fileId; self.sourceAudioIndex = sourceAudioIndex
        self.now = now; self.isPlaybackCurrent = current
        let observation = Session.shared.observeAuthorizationChanges { [weak self] generation in
            Task { @MainActor in
                guard let self, self.authGeneration != generation else { return }
                self.retire()
            }
        }
        authObserver = observation.id
        guard observation.generation == authGeneration else { retire(); return }
        let captured = generation
        action = networkTasks.run { [weak self] in
            guard let self else { return }
            do { try await self.loadOptions(captured) }
            catch { self.fail(error, captured) }
        }
    }

    private func current(_ captured: Int) -> Bool {
        captured == generation && authGeneration == Session.shared.playbackAuthorization.generation &&
            isPlaybackCurrent?() == true && !Task.isCancelled
    }
    /// Mutation busy state belongs to an action, independently of Task cancellation.
    private func replaceAction() {
        action?.cancel(); action = nil; actionSerial += 1; busy = false
    }
    private func currentAction(_ captured: Int, _ serial: Int) -> Bool {
        serial == actionSerial && current(captured)
    }
    private func finishAction(_ captured: Int, _ serial: Int) {
        guard generation == captured, actionSerial == serial else { return }
        busy = false; action = nil
    }
    private func clock(_ ms: Int) { serverClock = (ms, ProcessInfo.processInfo.systemUptime) }
    private func envelope() throws -> [String: CaptionValue] {
        guard let clock = serverClock else { throw CaptionProtocolError.expired }
        let elapsed = max(0, ProcessInfo.processInfo.systemUptime - clock.uptime)
        guard elapsed < 240 else { throw CaptionProtocolError.expired }
        return ["version": .integer(1), "request_id": .string(UUID().uuidString.lowercased()),
                "intent_expires_at_ms": .integer(clock.ms + Int(elapsed * 1000) + 60_000)]
    }
    private func playbackFields() -> [String: CaptionValue] {
        var sample = now?() ?? (position: 0, base: 0, rate: 1.0, paused: true)
        sample.position = seekDemandPosition ?? sample.position
        return ["playback_id": .string(playbackId), "continuity_id": .string(continuityId), "position_ms": .integer(max(0, sample.position)),
                "playback_rate": .number(sample.rate > 0 && sample.rate.isFinite ? sample.rate : 1), "paused": .bool(sample.paused)]
    }
    private func sentContinuity(_ body: [String: CaptionValue]) -> String? {
        if case let .string(value)? = body["continuity_id"] { return value }
        return nil
    }
    private func discontinueTraversal() {
        continuityId = UUID().uuidString.lowercased()
        lastHeartbeat = 0
        // Pending request bodies remain immutable until their CAS resolves.
    }
    private func loadOptions(_ captured: Int) async throws {
        guard let api else { return }
        let reply: CaptionHTTPReply<CaptionOptions> = try await api.caption("files/\(fileId)/subtitle-translation/options")
        guard current(captured) else { return }
        try reply.value.validate(file: fileId)
        options = reply.value; clock(reply.value.serverNowMs)
        assetPage = reply.value.assets; nextAssetCursor = reply.value.assetsNextCursor
        let rows = reply.value.audioTracks
        // Playback Decision AudioTrack.index is the audio ordinal, as is the
        // options audio_ordinal/index. source_index is FFprobe's global index.
        let audio = rows.first { $0.ordinal == sourceAudioIndex }
            ?? (rows.count == 1 ? rows.first : nil)
        audioOrdinal = audio?.ordinal
        if !profiles.contains(where: { $0.profileId == profileId }) {
            profileId = profiles.first(where: { $0.profileId == options?.defaultProfileId })?.profileId
                ?? profiles.first(where: { $0.modelId == "base" })?.profileId ?? profiles.first?.profileId ?? ""
        }
        if spokenLanguage.isEmpty { spokenLanguage = audio?.normalizedLanguage ?? "" }
        if !reply.value.capability.supported { status = CaptionServiceError.message(reply.value.capability.reason) }
        else if audio == nil { status = "Choose the actual spoken audio track before requesting captions." }
        else { status = "Choose a spoken language. Captions appear progressively while the picture continues." }
        if reply.value.permissions.demand {
            let initialLanguage = spokenLanguage
            let saved: CaptionHTTPReply<CaptionSavedSettings>? = try? await api.caption("subtitle-translation")
            guard current(captured), !selected else { return }
            if let saved = saved?.value, saved.version == 1 {
                if spokenLanguage == initialLanguage, let preferred = saved.preferredSpokenLanguage, languages.contains(preferred) { spokenLanguage = preferred }
                clock(saved.serverNowMs)
            }
        }
    }

    @discardableResult func request(_ task: CaptionTask) -> Bool {
        guard !busy else { return false }
        guard options?.capability.supported == true else { status = CaptionServiceError.message(options?.capability.reason); return false }
        let candidates = profiles.filter { $0.task == task }
        if profile?.task != task {
            let chosenModel = profile?.modelId
            let hintedModel = profiles.first(where: { $0.profileId == options?.defaultProfileId })?.modelId
            profileId = candidates.first(where: { $0.modelId == chosenModel })?.profileId
                ?? candidates.first(where: { $0.modelId == hintedModel })?.profileId
                ?? candidates.first(where: { $0.modelId == "base" })?.profileId ?? candidates.first?.profileId ?? ""
        }
        guard let profile, profile.task == task, profile.inputLanguages.contains(spokenLanguage),
              options?.permissions.demand == true,
              let audioOrdinal else { status = "Choose a compatible model and the actual spoken language. An administrator can request work."; return false }
        releaseDemand(); replaceAction(); loop?.cancel()
        intents = intents.filter { $0.key.hasPrefix("create:") }
        windowGeneration += 1; cues = []; ranges = []; lease = nil; window = nil
        operationId = nil; operation = nil; assetId = nil; contentRevision = nil; epoch = "0"; view = "0"
        chosenProfile = profile; chosenLanguage = spokenLanguage
        let language = spokenLanguage
        selected = true; busy = true; stopRequested = false
        let captured = generation, serial = actionSerial
        action = networkTasks.run { [weak self] in
            guard let self, let api = self.api else { return }
            defer { self.finishAction(captured, serial) }
            do {
                if self.serverClock.map({ ProcessInfo.processInfo.systemUptime - $0.uptime >= 240 }) ?? true {
                    try await self.loadOptions(captured)
                }
                guard self.currentAction(captured, serial), let options = self.options else { return }
                let key = "create:\(profile.profileId):\(audioOrdinal):\(language)"
                var body = try self.intents[key] ?? self.envelope()
                if self.intents[key] == nil {
                    body.merge(self.playbackFields()) { _, next in next }
                    body.merge(["source_size": .string(options.sourceSize), "source_mtime": .string(options.sourceMtime),
                                "audio_index": .integer(audioOrdinal), "spoken_language": .string(language),
                                "output_language": .string(task == .translate ? "en" : language), "task": .string(task.rawValue),
                                "model_id": .string(profile.modelId), "profile_id": .string(profile.profileId)]) { _, next in next }
                    self.intents[key] = body
                }
                self.status = "Preparing captions. Playback continues."
                let reply: CaptionHTTPReply<CaptionReceipt> = try await api.caption("files/\(self.fileId)/subtitle-translation", method: "POST", body: body)
                guard self.currentAction(captured, serial), self.selected else { return }
                try self.adopt(reply.value, continuity: self.sentContinuity(body))
                self.intents[key] = nil; self.chosenProfile = profile; self.chosenLanguage = language
                await self.poll(captured)
                self.startLoop(captured)
            } catch { self.fail(error, captured) }
        }
        return true
    }

    func read(_ asset: CaptionAsset) {
        guard CaptionValidation.digest(asset.assetId), CaptionValidation.uuid(asset.operationId),
              let profile = profiles.first(where: { $0.profileId == asset.profileId }), asset.audioIndex == audioOrdinal else { return }
        releaseDemand(); discontinueTraversal()
        intents = intents.filter { $0.key.hasPrefix("create:") }
        replaceAction(); loop?.cancel(); windowGeneration += 1; cues = []; ranges = []; lease = nil; window = nil
        selected = true; stopRequested = false; assetId = asset.assetId; operationId = asset.operationId
        chosenProfile = profile; chosenLanguage = asset.spokenLanguage; epoch = "0"; view = "0"; contentRevision = nil
        let captured = generation
        action = networkTasks.run { [weak self] in
            guard let self else { return }
            await self.poll(captured); self.startLoop(captured)
        }
    }

    func deselect() {
        networkTasks.cancelWork()
        discontinueTraversal()
        selected = false; window = nil; seekDemandPosition = nil
        loop?.cancel(); loop = nil; replaceAction()
        windowGeneration += 1; releaseDemand()
        busy = false
    }

    func retire() {
        // Invalidate continuations before releasing the captured demand.
        if authGeneration != Session.shared.playbackAuthorization.generation { networkTasks.cancelAll() }
        generation += 1; deselect()
        if let authObserver { Session.shared.removeAuthorizationObserver(authObserver) }
        authObserver = nil; options = nil; operation = nil; api = nil; now = nil; isPlaybackCurrent = nil
        operationId = nil; assetId = nil; demandId = nil; demandGeneration = nil; cues = []; ranges = []; lease = nil
        assetPage = []; nextAssetCursor = nil; failurePage = nil
        intents = [:]; chosenProfile = nil; chosenLanguage = nil; contentRevision = nil; serverClock = nil
        playbackId = UUID().uuidString.lowercased(); epoch = "0"; view = "0"; stopRequested = false; polling = false
        seekDemandPosition = nil; lastObserved = nil
    }

    /// Retire remains synchronous for UI callers. A transport owner may also
    /// wait for all cancelled requests and captured demand releases to settle.
    func retireAndDrain() async {
        retire()
        await networkTasks.drain()
    }

    private func adopt(_ receipt: CaptionReceipt, continuity: String? = nil) throws {
        try receipt.validate(continuity: continuity)
        if let operationId, operationId == receipt.operationId,
           CaptionValidation.compare(receipt.operationEpoch, epoch) == .orderedAscending { throw CaptionProtocolError.invalid }
        let advanceView = operationId != receipt.operationId || epoch != receipt.operationEpoch ||
            CaptionValidation.compare(receipt.viewRevision, view) != .orderedAscending
        if operationId != receipt.operationId || epoch != receipt.operationEpoch {
            demandId = nil; demandGeneration = nil; intents["heartbeat"] = nil
        }
        operationId = receipt.operationId; epoch = receipt.operationEpoch
        if advanceView { view = receipt.viewRevision }
        if let asset = receipt.assetId { assetId = asset }
        if let demand = receipt.demandId { demandId = demand; demandGeneration = receipt.demandGeneration }
        if receipt.state == "stopped" { stopRequested = true; demandId = nil; demandGeneration = nil }
        clock(receipt.serverNowMs)
    }

    /// Called by the existing owner's observer and urgent seek path.
    func mediaChanged(seek: Bool = false, positionMs: Int? = nil) {
        guard selected else { return }
        if seek {
            discontinueTraversal()
            seekDemandPosition = positionMs
            windowGeneration += 1; lease = nil; cues = []; ranges = []; publishWindow()
        }
        let sample = now?() ?? (position: 0, base: 0, rate: 1.0, paused: true)
        if let destination = seekDemandPosition, abs(sample.position - destination) < 1000 { seekDemandPosition = nil }
        let changed = lastObserved.map { $0.rate != sample.rate || $0.paused != sample.paused } ?? true
        lastObserved = (sample.rate, sample.paused)
        publishWindow()
        if seek || changed { lastHeartbeat = 0 }
        // Network updates are coalesced in the one loop, never awaited by a seek.
    }

    private func startLoop(_ captured: Int) {
        loop?.cancel()
        loop = networkTasks.run { [weak self] in
            while let self, self.current(captured), self.selected {
                await self.heartbeat(captured)
                await self.poll(captured)
                let delay = self.chosenProfile?.limits.pollMinMs ?? 1000
                do { try await Task.sleep(for: .milliseconds(delay)) } catch { return }
            }
        }
    }
    private func heartbeat(_ captured: Int) async {
        guard current(captured), selected, !stopRequested, operation?.state != "stopped", !busy,
              let api, let operationId, let demandId, let demandGeneration,
              ProcessInfo.processInfo.systemUptime - lastHeartbeat >= Double(chosenProfile?.limits.heartbeatIntervalMs ?? 5000) / 1000 else { return }
        do {
            var body = try intents["heartbeat"] ?? envelope()
            if intents["heartbeat"] == nil {
                body.merge(playbackFields()) { _, next in next }
                body["expected_operation_epoch"] = .string(epoch)
                body["demand_generation"] = .string(try CaptionValidation.next(demandGeneration))
                intents["heartbeat"] = body
            }
            let reply: CaptionHTTPReply<CaptionReceipt> = try await api.caption("subtitle-translation/operations/\(operationId)/demands/\(demandId)", method: "PUT", body: body)
            guard current(captured), !stopRequested, self.demandId == demandId else { return }
            let sent = sentContinuity(body)
            try adopt(reply.value, continuity: sent); intents["heartbeat"] = nil
            // A seek during this await must still consume the acknowledged CAS,
            // then promptly replace the old traversal on the next owned loop.
            lastHeartbeat = sent == continuityId ? ProcessInfo.processInfo.systemUptime : 0
        } catch {
            if let error = error as? CaptionServiceError, ["operation_stopped", "epoch_conflict", "demand_conflict"].contains(error.code) {
                self.demandId = nil; self.demandGeneration = nil
                if error.code == "operation_stopped" { stopRequested = true }
            }
            fail(error, captured, intent: "heartbeat")
        }
    }
    private func releaseDemand() {
        guard authGeneration == Session.shared.playbackAuthorization.generation,
              let api, let operationId, let demandId, let demandGeneration, let bodyBase = try? envelope() else { return }
        var body = bodyBase
        body["expected_operation_epoch"] = .string(epoch)
        body["demand_generation"] = .string((try? CaptionValidation.next(demandGeneration)) ?? demandGeneration)
        body["playback_id"] = .string(playbackId)
        self.demandId = nil; self.demandGeneration = nil; intents["heartbeat"] = nil
        // Best effort only; the server demand TTL is the authority after loss.
        networkTasks.run(cleanup:true) { let _: CaptionHTTPReply<CaptionReceipt>? = try? await api.caption("subtitle-translation/operations/\(operationId)/demands/\(demandId)", method: "DELETE", body: body) }
    }

    func stop() {
        guard !busy, let operationId, let api else { return }
        discontinueTraversal()
        replaceAction(); stopRequested = true; busy = true
        let captured = generation, serial = actionSerial
        action = networkTasks.run { [weak self] in
            guard let self else { return }
            defer { self.finishAction(captured, serial) }
            do {
                var body = try self.intents["stop"] ?? self.envelope()
                if self.intents["stop"] == nil { body["expected_operation_epoch"] = .string(self.epoch) }
                self.intents["stop"] = body
                let reply: CaptionHTTPReply<CaptionReceipt> = try await api.caption("subtitle-translation/operations/\(operationId)/stop", method: "POST", body: body)
                guard self.currentAction(captured, serial) else { return }
                try self.adopt(reply.value); self.intents["stop"] = nil
                self.demandId = nil; self.demandGeneration = nil
                self.status = "Caption work stopped. Published captions remain available."
                await self.poll(captured)
            } catch {
                guard self.currentAction(captured, serial), !(error is CancellationError),
                      (error as NSError).code != NSURLErrorCancelled else { return }
                self.fail(error, captured, intent: "stop")
                guard self.currentAction(captured, serial) else { return }
                self.status = "Stop is unconfirmed. Retry Stop; playback continues."
            }
        }
    }

    func retry(_ range: CaptionFailedRange? = nil) {
        guard !busy, let api, let operationId, operation != nil else { return }
        let key = range.map { "retry:\($0.coreIndex)" } ?? "retry"
        replaceAction(); busy = true
        let captured = generation, serial = actionSerial
        action = networkTasks.run { [weak self] in
            guard let self else { return }
            defer { self.finishAction(captured, serial) }
            do {
                if self.intents[key] == nil { await self.poll(captured) }
                guard self.currentAction(captured, serial), let operation = self.operation else { return }
                var body = try self.intents[key] ?? self.envelope()
                if self.intents[key] == nil {
                    body.merge(self.playbackFields()) { _, next in next }
                    body["expected_operation_epoch"] = .string(self.epoch)
                    if let range {
                        body["scope"] = .string("range"); body["expected_operation_state"] = .string("partial")
                        body["core_index"] = .integer(range.coreIndex); body["expected_range_revision"] = .string(range.rangeRevision)
                        body["expected_range_state"] = .string("failed")
                    } else {
                        body["scope"] = .string("operation"); body["expected_view_revision"] = .string(operation.viewRevision)
                        body["expected_state"] = .string(operation.state)
                    }
                    self.intents[key] = body
                }
                let reply: CaptionHTTPReply<CaptionReceipt> = try await api.caption("subtitle-translation/operations/\(operationId)/retry", method: "POST", body: body)
                guard self.currentAction(captured, serial) else { return }
                try self.adopt(reply.value, continuity: self.sentContinuity(body)); self.intents[key] = nil; self.stopRequested = false
                self.startLoop(captured)
            } catch { self.fail(error, captured, intent: key) }
        }
    }

    func moreAssets() {
        guard let cursor = nextAssetCursor, cursor.utf8.count <= 1024, let api else { return }
        let captured = generation
        networkTasks.run { [self] in
            do {
                let reply: CaptionHTTPReply<CaptionAssetPage> = try await api.caption("files/\(fileId)/subtitle-translation/assets", query: [URLQueryItem(name: "cursor", value: cursor)])
                guard current(captured), reply.value.version == 1, reply.value.fileId == String(fileId), reply.value.entries.count <= 16 else { return }
                assetPage = reply.value.entries; nextAssetCursor = reply.value.nextCursor; clock(reply.value.serverNowMs)
            } catch { fail(error, captured) }
        }
    }

    func moreFailures() {
        guard let cursor = nextFailureCursor, cursor.utf8.count <= 1024,
              let operation, let lease, let api, ProcessInfo.processInfo.systemUptime < lease.deadline else { return }
        let captured = generation
        networkTasks.run { [self] in
            do {
                let reply: CaptionHTTPReply<CaptionFailurePage> = try await api.caption("subtitle-translation/operations/\(operation.operationId)/failed-ranges", query: [URLQueryItem(name: "cursor", value: cursor)], headers: ["X-Translation-Read-Lease": lease.id])
                let page = reply.value
                guard current(captured), self.operation?.operationId == operation.operationId,
                      self.operation?.operationEpoch == operation.operationEpoch, self.operation?.viewRevision == operation.viewRevision,
                      page.version == 1, page.operationId == operation.operationId, page.operationEpoch == operation.operationEpoch,
                      page.viewRevision == operation.viewRevision, page.entries.count <= 16,
                      page.entries.allSatisfy({ (0...2047).contains($0.coreIndex) && CaptionValidation.counter($0.rangeRevision) }) else { return }
                failurePage = page
            } catch { fail(error, captured) }
        }
    }

    private func poll(_ captured: Int) async {
        guard current(captured), selected, !polling, let api else { return }
        polling = true
        defer { if generation == captured { polling = false } }
        do {
            if let operationId {
                let reply: CaptionHTTPReply<CaptionOperation> = try await api.caption("subtitle-translation/operations/\(operationId)")
                guard current(captured), selected else { return }
                let row = reply.value
                guard row.version == 1, row.operationId == operationId, row.fileId == String(fileId),
                      row.audioIndex == audioOrdinal, row.profileId == chosenProfile?.profileId,
                      row.sourceSize == options?.sourceSize, row.sourceMtime == options?.sourceMtime,
                      row.spokenLanguage == chosenLanguage, row.task == chosenProfile?.task,
                      row.outputLanguage == (chosenProfile?.task == .translate ? "en" : chosenLanguage),
                      CaptionValidation.counter(row.operationEpoch), CaptionValidation.counter(row.viewRevision),
                      row.failedRanges.count <= 64,
                      row.failedRanges.allSatisfy({ (0...2047).contains($0.coreIndex) && CaptionValidation.counter($0.rangeRevision) }) else { throw CaptionProtocolError.invalid }
                if CaptionValidation.compare(row.operationEpoch, epoch) != .orderedAscending &&
                    (row.operationEpoch != epoch || CaptionValidation.compare(row.viewRevision, view) != .orderedAscending) {
                    if epoch != row.operationEpoch { demandId = nil; demandGeneration = nil; intents["heartbeat"] = nil }
                    operation = row; epoch = row.operationEpoch; view = row.viewRevision; clock(row.serverNowMs)
                    assetId = row.assetId
                    if row.state == "stopped" { stopRequested = true; demandId = nil; demandGeneration = nil }
                }
                status = row.reason.map { CaptionServiceError.message($0) } ?? (row.state == "checking_source" ? "Checking the source before generating captions…" : "Preparing captions. Playback continues.")
            }
            if assetId != nil { try await readWindow(captured) }
        } catch { fail(error, captured) }
    }

    private func pages<T: Decodable>(_ initial: [T], cursor: String?, kind: String,
                                      asset: String, snapshot: CaptionSnapshot, captured: Int,
                                      windowOwner: Int, maximum: Int) async throws -> [T] {
        guard let api, let profile = chosenProfile, initial.count <= profile.limits.pageEntries, initial.count <= maximum else { throw CaptionProtocolError.budget }
        var entries = initial, next = cursor, seen: Set<String> = []
        while let cursor = next {
            guard current(captured), windowGeneration == windowOwner, lease?.id == snapshot.leaseId,
                  ProcessInfo.processInfo.systemUptime < (lease?.deadline ?? 0),
                  cursor.utf8.count <= 1024, seen.insert(cursor).inserted, entries.count < maximum else { throw CaptionProtocolError.expired }
            let reply: CaptionHTTPReply<CaptionPage<T>> = try await api.caption("subtitle-translation/assets/\(asset)/\(kind)",
                query: [URLQueryItem(name: "cursor", value: cursor)], headers: ["X-Translation-Read-Lease": snapshot.leaseId], maximumBytes: profile.limits.pageBytes)
            let page = reply.value
            guard page.version == 1, page.assetId == asset, page.snapshotRevision == snapshot.revision,
                  page.entries.count <= profile.limits.pageEntries else { throw CaptionProtocolError.invalid }
            entries += page.entries
            guard entries.count <= maximum else { throw CaptionProtocolError.budget }
            next = page.nextCursor
        }
        return entries
    }

    private func readWindow(_ captured: Int) async throws {
        guard let api, let asset = assetId, let profile = chosenProfile, let options,
              CaptionValidation.digest(asset) else { throw CaptionProtocolError.invalid }
        let owner = windowGeneration
        var sample = now?() ?? (position: 0, base: 0, rate: 1.0, paused: true)
        sample.position = seekDemandPosition ?? sample.position
        let reply: CaptionHTTPReply<CaptionManifest> = try await api.caption("subtitle-translation/assets/\(asset)/manifest",
            query: [URLQueryItem(name: "position_ms", value: String(max(0, sample.position))), URLQueryItem(name: "renew_lease", value: "1")], maximumBytes: profile.limits.pageBytes)
        guard current(captured), owner == windowGeneration, selected else { return }
        let m = reply.value, snapshot = m.snapshot
        guard m.version == 1, m.assetId == asset, m.fileId == String(fileId), m.profileId == profile.profileId,
              m.audioIndex == audioOrdinal, m.spokenLanguage == chosenLanguage,
              m.outputLanguage == (profile.task == .translate ? "en" : chosenLanguage),
              m.timelineContract == profile.timelineContract, CaptionValidation.digest(m.contentRevision),
              contentRevision == nil || contentRevision == m.contentRevision,
              m.operation.operationId == operationId, CaptionValidation.counter(m.operation.epoch),
              CaptionValidation.counter(m.manifestRevision), CaptionValidation.counter(m.viewRevision),
              CaptionValidation.uuid(snapshot.leaseId), snapshot.revision == m.manifestRevision,
              snapshot.expiresInMs > reply.elapsedMs,
              CaptionValidation.compare(m.operation.epoch, epoch) != .orderedAscending,
              m.operation.epoch != epoch || CaptionValidation.compare(m.viewRevision, view) != .orderedAscending else { throw CaptionProtocolError.invalid }
        let expiresHeader = reply.response.value(forHTTPHeaderField: "X-Translation-Read-Lease-Expires-At-Ms").flatMap(Int.init)
        let nowHeader = reply.response.value(forHTTPHeaderField: "X-Translation-Server-Now-Ms").flatMap(Int.init)
        let budget = min(snapshot.expiresInMs, expiresHeader.flatMap { expires in nowHeader.map { expires - $0 } } ?? snapshot.expiresInMs) - reply.elapsedMs
        guard budget > 0, budget <= 300_000 else { throw CaptionProtocolError.expired }
        lease = (snapshot.leaseId, snapshot.revision, ProcessInfo.processInfo.systemUptime + Double(budget) / 1000)
        let references: [CaptionSegmentReference] = try await pages(m.segments, cursor: m.segmentNextCursor, kind: "segments", asset: asset, snapshot: snapshot, captured: captured, windowOwner: owner, maximum: profile.limits.cueCount)
        let coverage: [CaptionRange] = try await pages(m.coverage, cursor: m.coverageNextCursor, kind: "coverage", asset: asset, snapshot: snapshot, captured: captured, windowOwner: owner, maximum: profile.limits.coverageIntervals)
        guard coverage.allSatisfy({ CaptionValidation.range($0.startMs, $0.endMs, duration: options.durationMs) }) else { throw CaptionProtocolError.invalid }
        var result: [CaptionCue] = [], byteCount = 0, ids: Set<String> = []
        for ref in references {
            guard current(captured), owner == windowGeneration, lease?.id == snapshot.leaseId,
                  ProcessInfo.processInfo.systemUptime < (lease?.deadline ?? 0) else { throw CaptionProtocolError.expired }
            guard CaptionValidation.digest(ref.segmentId), CaptionValidation.digest(ref.digest),
                  CaptionValidation.range(ref.startMs, ref.endMs, duration: options.durationMs) else { throw CaptionProtocolError.invalid }
            guard ref.endMs > sample.position - profile.limits.cueWindowBeforeMs,
                  ref.startMs < sample.position + profile.limits.cueWindowAfterMs else { continue }
            let bytes = try await api.captionBytes("subtitle-translation/assets/\(asset)/segments/\(ref.segmentId)",
                headers: ["X-Translation-Read-Lease": snapshot.leaseId], maximumBytes: profile.limits.segmentBytes)
            guard CaptionValidation.sha256(bytes.value) == ref.digest else { throw CaptionProtocolError.invalid }
            let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
            let segment = try decoder.decode(CaptionSegment.self, from: bytes.value)
            try segment.validate(reference: ref, asset: asset, source: m.contentRevision, profile: profile, duration: options.durationMs)
            for cue in segment.cues where ids.insert(cue.cueId).inserted {
                byteCount += cue.text.utf8.count; result.append(cue)
                guard byteCount <= profile.limits.cueMemoryBytes, result.count <= profile.limits.cueCount else { throw CaptionProtocolError.budget }
            }
        }
        guard current(captured), owner == windowGeneration, lease?.id == snapshot.leaseId,
              ProcessInfo.processInfo.systemUptime < (lease?.deadline ?? 0),
              CaptionValidation.compare(m.operation.epoch, epoch) != .orderedAscending,
              m.operation.epoch != epoch || CaptionValidation.compare(m.viewRevision, view) != .orderedAscending else { return }
        if epoch != m.operation.epoch { demandId = nil; demandGeneration = nil; intents["heartbeat"] = nil }
        clock(m.serverNowMs); contentRevision = m.contentRevision; epoch = m.operation.epoch; view = m.viewRevision
        cues = result.sorted { $0.startMs == $1.startMs ? $0.cueId < $1.cueId : $0.startMs < $1.startMs }; ranges = coverage
        if m.operation.state == "stopped" { stopRequested = true; demandId = nil; demandGeneration = nil }
        status = stopped ? "Caption work stopped. Published captions remain available." : operation?.state == "complete" ? "Captions complete." : "Partial captions available. Playback continues."
        publishWindow()
    }

    private func publishWindow() {
        guard selected, let sample = now?() else { window = nil; return }
        if window?.baseMs != sample.base || window?.offsetMs != subtitleOffsetMs || window?.cues != cues {
            renderRevision += 1
            window = CaptionRenderWindow(revision: renderRevision, baseMs: sample.base, offsetMs: subtitleOffsetMs, cues: cues)
        }
        var end: Int?
        for range in ranges.sorted(by: { $0.startMs < $1.startMs }) {
            if range.startMs <= sample.position && range.endMs > sample.position { end = max(end ?? 0, range.endMs) }
            else if let currentEnd = end, range.startMs <= currentEnd { end = max(currentEnd, range.endMs) }
        }
        coverageStatus = end.map { "Coverage ahead: \(max(0, $0 - sample.position) / 1000) seconds." }
            ?? (options?.permissions.demand == true && demandId != nil ? "Not captioned here yet. Preparing this part." : "Not captioned here yet. Ask an administrator to generate this part.")
    }
    private func fail(_ error: Error, _ captured: Int, intent: String? = nil) {
        guard current(captured), !(error is CancellationError), (error as NSError).code != NSURLErrorCancelled else { return }
        if let service = error as? CaptionServiceError, service.status == 401 || service.status == 403 {
            retire()
            status = "Caption access ended. Sign in or reopen the title to refresh permissions."
            return
        }
        if let intent, let service = error as? CaptionServiceError, !service.ambiguous { intents[intent] = nil }
        status = error.localizedDescription
    }
}
