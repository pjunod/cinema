import Foundation

/// Authenticated B decisions preserve Source identity and never acquire media authority.
struct SharedDecisionClient {
    private let origin: String
    private let token: String
    private let generation: UInt64
    private let configuration: URLSessionConfiguration
    init() throws { try self.init(configuration: .ephemeral) }
    #if DEBUG
    init(testConfiguration: URLSessionConfiguration) throws { try self.init(configuration: testConfiguration) }
    #endif
    private init(configuration: URLSessionConfiguration) throws {
        let auth = Session.shared.playbackAuthorization
        guard Session.canonicalOrigin(auth.origin) != nil, let token = auth.token, !token.isEmpty else { throw APIError.badURL }
        origin = auth.origin; self.token = token; generation = auth.generation; self.configuration = configuration.copy() as! URLSessionConfiguration
    }
    private func requireCurrent() throws {
        let current = Session.shared.playbackAuthorization
        guard current.origin == origin, current.token == token, current.generation == generation else { throw APIError.badURL }
    }
    func decision(context: PlaybackFileContext, selection: PrePlaySelection = .none,
                  quality: PlaybackQuality = .auto, audioOffsetMs: Int = 0,
                  presentationTarget: PresentationTarget? = nil) async throws -> (decision: SharedDecision, caps: DeviceCaps) {
        var document = Caps.snapshot().document
        document.display.presentationTarget = presentationTarget
        return try await execute(context: context, caps: document,
            query: quality.decisionQueryItems + selection.queryItems + [URLQueryItem(name: "audio_offset_ms", value: String(audioOffsetMs))])
    }
    /// Authenticated initial Start only. Local recovery/control fields are never
    /// erased or forwarded to Source as a guessed predecessor identity.
    func start(context: PlaybackFileContext, request body: CreateSessionRequest) async throws -> SharedStartedPlayback {
        try Task.checkCancellation(); try requireCurrent()
        guard let reference = context.reference, let revision = context.revision,
              context.sessionId == nil, (context.lifecycleGeneration ?? 0) > 0
        else { throw APIError.badURL }
        try context.validateSharedReference(reference, file: context.sourceFileId, revision: revision)
        guard body.intent == nil, body.previousSessionId == nil, body.controlSequence == nil,
              body.reopenReason == nil, body.subtitleBurn == nil, body.hdr10 != true,
              body.preserveDolbyVision != true
        else { throw APIError.transport("This Shared playback change is not available yet.") }
        guard body.caps?.v == 2, body.presentation == "vod", !body.playbackId.isEmpty,
              body.playbackId.utf8.count <= 128,
              !body.playbackId.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }),
              let requestID = body.requestId,
              PlaybackFileContext.matches(requestID, "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        else { throw APIError.badURL }
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let encoded = try encoder.encode(body); guard encoded.count <= 24_576 else { throw APIError.badURL }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let retained = try decoder.decode(CreateSessionRequest.self, from: encoded)
        guard let url = URL(string: origin + (try context.path("hls/sessions"))) else { throw APIError.badURL }
        var request = URLRequest(url: url); request.httpMethod = "POST"; request.httpBody = encoded; request.timeoutInterval = 310
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let data = try await SharedDecisionReadOperation(request: request, configuration: configuration) {
            try requireCurrent(); _ = try context.path("hls/sessions")
        }.read()
        try Task.checkCancellation(); try requireCurrent()
        return try SharedStart.decode(data).bindInitial(context, request: retained)
    }
    func playlistURL(playback: SharedStartedPlayback) throws -> URL {
        try requireCurrent(); _ = try playback.start.validated(playback.context)
        guard let reference = playback.context.reference, let revision = playback.context.revision,
              playback.context.sessionId == playback.start.response.sessionId else { throw APIError.badURL }
        try playback.context.validateSharedReference(reference, file: playback.context.sourceFileId, revision: revision)
        guard let url = URL(string: origin + playback.start.response.playlistUrl) else { throw APIError.badURL }
        return url
    }
    func status(playback: SharedStartedPlayback) async throws -> SharedPlaybackStatus {
        _ = try playlistURL(playback: playback)
        guard let url = URL(string: origin + "/api/v1/hls/\(playback.start.response.sessionId)/status") else { throw APIError.badURL }
        var request = URLRequest(url: url); request.timeoutInterval = 10
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let data = try await SharedDecisionReadOperation(request: request, configuration: configuration, maxBytes: 65_536) {
            _ = try playlistURL(playback: playback)
        }.read()
        try requireCurrent()
        return try SharedPlaybackStatus.decode(data, playback: playback)
    }
    /// Best-effort B End. A successful HTTP reply is never physical settlement
    /// evidence; the server's actual owner retains that obligation.
    func end(playback: SharedStartedPlayback) async throws {
        _ = try playlistURL(playback: playback)
        guard let url = URL(string: origin + "/api/v1/hls/\(playback.start.response.sessionId)") else { throw APIError.badURL }
        var request = URLRequest(url: url); request.httpMethod = "DELETE"; request.timeoutInterval = 10
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        _ = try await SharedDecisionReadOperation(request: request, configuration: configuration, statuses: [200, 202, 204], maxBytes: 16_384, allowEmpty: true, current: requireCurrent).readResponse()
    }
    /// One bounded account/Source/epoch/item watch order across imports.
    /// Busy callers skip a beat; a conflict always triggers a fresh read before
    /// a later new beat, never a replay of the stale position with a new number.
    @MainActor
    func orderedProgress(playback: SharedStartedPlayback, initialWatchSequence: Int64,
                         positionMs: Int64, durationMs: Int64?, watched: Bool = false) async throws -> SharedProgressResult? {
        try requireCurrent()
        guard let reference = playback.context.reference, let revision = playback.context.revision else { throw APIError.badURL }
        try playback.context.validateSharedReference(reference, file: playback.context.sourceFileId, revision: revision)
        _ = try playback.start.validated(playback.context)
        let account = SharedProgressAccount(origin: origin, token: token, generation: generation)
        let key = SharedProgressKey(server: reference.serverId, epoch: reference.catalogueEpoch, item: reference.itemId)
        let registry = Self.progressRegistry
        if registry.account != account { registry.account = account; registry.entries.removeAll() }
        guard registry.active < 4 else { return nil }
        if registry.entries[key] == nil {
            if registry.entries.count >= 256 {
                guard let victim = registry.entries.filter({ !$0.value.busy && $0.value.order.pending == nil }).min(by: { $0.value.lastUse < $1.value.lastUse })?.key else { return nil }
                registry.entries.removeValue(forKey: victim)
            }
            registry.entries[key] = SharedProgressEntry(order: try SharedProgressOrder(sequence: initialWatchSequence))
        }
        let entry = registry.entries[key]!
        guard !entry.busy else { return nil }
        entry.busy = true; registry.active += 1; registry.serial += 1; entry.lastUse = registry.serial
        defer { entry.busy = false; registry.active -= 1 }
        if entry.order.needsResync || entry.order.pending.map({ $0.sessionId != playback.context.sessionId }) == true {
            let expected = Session.shared.playbackAuthorization
            let data = try await Self.detail(reference: reference, transport: URLSession(configuration: configuration), expected: expected)
            try requireCurrent()
            let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
            let detail = try decoder.decode(SharedLibraryDetail.self, from: data); try detail.validate(expected: reference)
            guard detail.lifecycleGeneration == playback.context.lifecycleGeneration else { throw APIError.badURL }
            let fresh = detail.watch?.sequence ?? 0
            if !entry.order.needsResync { entry.order = try SharedProgressOrder(sequence: max(entry.order.sequence, fresh)) }
            else { try entry.order.resync(freshAuthorizedSequence: fresh) }
        }
        let beat = try entry.order.beat(sessionId: playback.start.response.sessionId, positionMs: positionMs, durationMs: durationMs, watched: watched)
        let result = try await progress(playback: playback, beat: beat)
        try requireCurrent(); try entry.order.complete(beat, result: result)
        if result == .acknowledged && (beat.positionMs != positionMs || beat.durationMs != durationMs || beat.watched != watched) { return .previousBeatAcknowledged }
        return result
    }
    @MainActor private static let progressRegistry = SharedProgressRegistry()
    /// Exact B session beat only. The caller retains it on an uncertain send
    /// and obtains fresh authorized detail/watch state after a typed conflict.
    func progress(playback: SharedStartedPlayback, beat: SharedProgressBeat) async throws -> SharedProgressResult {
        try Task.checkCancellation(); try requireCurrent(); try beat.validate()
        let context = playback.context
        guard let reference = context.reference, context.sessionId == beat.sessionId,
              playback.start.response.sessionId == beat.sessionId,
              let revision = context.revision else { throw APIError.badURL }
        try context.validateSharedReference(reference, file: context.sourceFileId, revision: revision)
        _ = try playback.start.validated(context)
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase; encoder.outputFormatting = [.sortedKeys]
        let bytes = try encoder.encode(beat)
        guard bytes.count <= 1024, let url = URL(string: origin + "/api/v1/shared/imports/\(reference.importId)/items/\(reference.itemId)/progress") else { throw APIError.badURL }
        var request = URLRequest(url: url); request.httpMethod = "POST"; request.httpBody = bytes; request.timeoutInterval = 10
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let response = try await SharedDecisionReadOperation(request: request, configuration: configuration, statuses: [200, 409], maxBytes: 16_384, current: requireCurrent).readResponse()
        try requireCurrent(); try Task.checkCancellation()
        if response.status == 200 {
            guard try JSONDecoder().decode(SharedPlaybackJSON.self, from: response.data).object != nil else { throw APIError.badURL }
            return .acknowledged
        }
        struct Refusal: Decodable { let code: String; let currentSequence: Int64? }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let refusal = try decoder.decode(Refusal.self, from: response.data)
        guard ["sharing_progress_stale", "sharing_progress_conflict"].contains(refusal.code),
              refusal.currentSequence.map({ (0...9_007_199_254_740_991).contains($0) }) ?? true else { throw APIError.badURL }
        return .resyncRequired(currentSequence: refusal.currentSequence)
    }
    #if DEBUG
    func decisionForTest(context: PlaybackFileContext, caps: DeviceCaps, query: [URLQueryItem] = []) async throws -> (decision: SharedDecision, caps: DeviceCaps) {
        try await execute(context: context, caps: caps, query: query)
    }
    #endif
    private func execute(context: PlaybackFileContext, caps: DeviceCaps, query: [URLQueryItem]) async throws -> (decision: SharedDecision, caps: DeviceCaps) {
        try Task.checkCancellation(); try requireCurrent()
        guard let reference = context.reference, let revision = context.revision,
              let lifecycle = context.lifecycleGeneration, lifecycle > 0, caps.v == 2 else { throw APIError.badURL }
        try context.validateSharedReference(reference, file: context.sourceFileId, revision: revision)
        guard Set(query.map(\.name)).count == query.count else { throw APIError.badURL }
        for field in query {
            guard let text = field.value else { throw APIError.badURL }
            let integer = Int(text)
            let canonical = integer.map { String($0) == text } ?? false
            switch field.name {
            case "force": guard ["auto", "original", "transcode"].contains(text) else { throw APIError.badURL }
            case "audio": guard canonical, (0...4095).contains(integer!) else { throw APIError.badURL }
            case "subtitle": guard canonical, (-1...4095).contains(integer!) else { throw APIError.badURL }
            case "audio_offset_ms": guard canonical, (-15000...15000).contains(integer!) else { throw APIError.badURL }
            default: throw APIError.badURL
            }
        }
        struct Body: Encodable { let caps: DeviceCaps }
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let body = try encoder.encode(Body(caps: caps)); guard body.count <= 131_072 else { throw APIError.badURL }
        guard let url = URL(string: origin + (try context.path("decision", query: query))) else { throw APIError.badURL }
        var request = URLRequest(url: url); request.httpMethod = "POST"; request.httpBody = body; request.timeoutInterval = 30
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization"); request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        let data = try await SharedDecisionReadOperation(request: request, configuration: configuration) { try requireCurrent(); _ = try context.path("decision") }.read()
        try Task.checkCancellation(); try requireCurrent()
        let decision = try SharedDecision.decode(data).validated(context); try requireCurrent()
        return (decision, caps)
    }
    /// The context factory still validates every returned field before constructing authority.
    static func detail(reference: SharedPlaybackReference, transport: URLSession, expected: (origin: String, token: String?, generation: UInt64)) async throws -> Data {
        try reference.validate()
        let client = try Self(configuration: transport.configuration); try client.requireCurrent()
        guard client.origin == expected.origin, client.token == expected.token, client.generation == expected.generation else { throw APIError.badURL }
        guard let url = URL(string: client.origin + "/api/v1/shared/imports/\(reference.importId)/items/\(reference.itemId)") else { throw APIError.badURL }
        var request = URLRequest(url: url); request.timeoutInterval = 30
        request.setValue("Bearer \(client.token)", forHTTPHeaderField: "Authorization")
        return try await SharedDecisionReadOperation(request: request, configuration: client.configuration, current: client.requireCurrent).read()
    }
}

private struct SharedDecisionHTTPResponse { let data: Data; let status: Int }

private final class SharedDecisionReadOperation: NSObject, URLSessionDataDelegate {
    private let request: URLRequest
    private let configuration: URLSessionConfiguration
    private let current: () throws -> Void
    private let lock = NSLock()
    private var data = Data()
    private var completed = false
    private var continuation: CheckedContinuation<SharedDecisionHTTPResponse, Error>?
    private let statuses: Set<Int>
    private let maxBytes: Int
    private let allowEmpty: Bool
    private var responseStatus = 0
    private var session: URLSession?
    private var task: URLSessionDataTask?
    private var observer: UUID?
    init(request: URLRequest, configuration: URLSessionConfiguration, statuses: Set<Int> = [200], maxBytes: Int = 4_194_304, allowEmpty: Bool = false, current: @escaping () throws -> Void) {
        self.request = request; self.configuration = configuration; self.current = current
        self.statuses = statuses; self.maxBytes = maxBytes; self.allowEmpty = allowEmpty
        super.init(); data.reserveCapacity(maxBytes)
    }
    func read() async throws -> Data { try await readResponse().data }
    func readResponse() async throws -> SharedDecisionHTTPResponse {
        try await withTaskCancellationHandler(operation: {
            try await withCheckedThrowingContinuation { continuation in start(continuation) }
        }, onCancel: { [weak self] in self?.finish(.failure(CancellationError())) })
    }
    private func start(_ continuation: CheckedContinuation<SharedDecisionHTTPResponse, Error>) {
        lock.lock()
        if completed { lock.unlock(); continuation.resume(throwing: CancellationError()); return }
        self.continuation = continuation; lock.unlock()
        do { try current() } catch { finish(.failure(error)); return }
        let config = configuration.copy() as! URLSessionConfiguration
        config.urlCache = nil; config.urlCredentialStorage = nil; config.httpCookieStorage = nil; config.httpShouldSetCookies = false
        config.requestCachePolicy = .reloadIgnoringLocalCacheData
        let queue = OperationQueue(); queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: config, delegate: self, delegateQueue: queue)
        let task = session.dataTask(with: request)
        let registration = Session.shared.observeAuthorizationChanges { [weak self] _ in self?.finish(.failure(APIError.badURL)) }
        lock.lock()
        if completed { lock.unlock(); Session.shared.removeAuthorizationObserver(registration.id); session.invalidateAndCancel(); return }
        self.session = session; self.task = task; observer = registration.id; lock.unlock()
        do { try current(); task.resume() } catch { finish(.failure(error)) }
    }
    private func finish(_ result: Result<SharedDecisionHTTPResponse, Error>) {
        lock.lock()
        guard !completed else { lock.unlock(); return }
        completed = true
        let continuation = self.continuation; self.continuation = nil
        let task = self.task; self.task = nil
        let session = self.session; self.session = nil
        let observer = self.observer; self.observer = nil
        data = Data(); lock.unlock()
        if let observer { Session.shared.removeAuthorizationObserver(observer) }
        task?.cancel(); session?.invalidateAndCancel(); continuation?.resume(with: result)
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil); finish(.failure(APIError.badURL))
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive response: URLResponse,
                    completionHandler: @escaping (URLSession.ResponseDisposition) -> Void) {
        do {
            try current()
            guard let response = response as? HTTPURLResponse, response.url == request.url else { throw APIError.badURL }
            guard statuses.contains(response.statusCode) else { throw APIError.http(response.statusCode) }
            guard response.expectedContentLength < 0 || response.expectedContentLength <= maxBytes else { throw APIError.badURL }
            lock.lock(); let done = completed; responseStatus = response.statusCode; lock.unlock()
            completionHandler(done ? .cancel : .allow)
        } catch { completionHandler(.cancel); finish(.failure(error)) }
    }
    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive bytes: Data) {
        do { try current() } catch { finish(.failure(error)); return }
        lock.lock(); guard !completed else { lock.unlock(); return }
        guard bytes.count <= maxBytes - data.count else { lock.unlock(); finish(.failure(APIError.badURL)); return }
        data.append(bytes); lock.unlock()
    }
    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error { finish(.failure(error)); return }
        do { try current() } catch { finish(.failure(error)); return }
        lock.lock(); let result = SharedDecisionHTTPResponse(data: data, status: responseStatus); let valid = allowEmpty || !data.isEmpty; lock.unlock()
        if valid { finish(.success(result)) } else { finish(.failure(APIError.badURL)) }
    }
}

private struct SharedProgressAccount: Equatable { let origin: String; let token: String; let generation: UInt64 }
private struct SharedProgressKey: Hashable { let server: String; let epoch: String; let item: String }
@MainActor private final class SharedProgressEntry {
    var order: SharedProgressOrder; var busy = false; var lastUse: UInt64 = 0
    init(order: SharedProgressOrder) { self.order = order }
}
@MainActor private final class SharedProgressRegistry {
    var account: SharedProgressAccount?
    var entries: [SharedProgressKey: SharedProgressEntry] = [:]
    var active = 0
    var serial: UInt64 = 0
}
