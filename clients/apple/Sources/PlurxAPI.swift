import Foundation

enum APIError: Error, LocalizedError {
    case badURL
    /// A non-2xx answer with no legible `{code, message}` body — and every
    /// 401/403, whose match stays status-based (`AppModel.isSessionExpired`).
    case http(Int)
    case conflict(code: String, message: String)
    /// A refusal the server explained. The playback surface adapter classifies
    /// on `code` (PLAYBACK-SURFACE-CONTRACT.md §3.3) and shows `message`; a
    /// `media_owner_lost` also carries the film position to reopen at.
    case refused(status: Int, code: String, message: String, positionMs: Int?)
    case transport(String)

    var errorDescription: String? {
        switch self {
        case .badURL: return "Invalid server address"
        case .http(let code): return "Server returned \(code)"
        case .conflict(let code, let message): return "\(message) (\(code), HTTP 409)"
        // The server's own sentence, unadorned: it is the answer, and the
        // code and status travel in the surface ledger rather than in the
        // line the viewer reads.
        case .refused(_, _, let message, _): return message
        case .transport(let message): return message
        }
    }

    /// The refusal code a typed answer carried, for the callers that classify
    /// on it. `.conflict` keeps its own shape for the matchers that predate
    /// the surface contract.
    var refusalCode: String? {
        switch self {
        case .conflict(let code, _): return code
        case .refused(_, let code, _, _): return code
        case .badURL, .http, .transport: return nil
        }
    }

    /// The HTTP status behind this error, when there was one.
    var httpStatus: Int? {
        switch self {
        case .http(let code): return code
        case .conflict: return 409
        case .refused(let status, _, _, _): return status
        case .badURL, .transport: return nil
        }
    }
}

/// The server's typed refusal body: `{code, message}` plus whatever recovery
/// data the code carries (`ApiError::TypedDetail`).
private struct Refusal: Decodable {
    let code: String
    let message: String
    let filmPositionMs: Int?

    enum CodingKeys: String, CodingKey {
        case code
        case message
        case filmPositionMs = "film_position_ms"
    }
}

/// Async `/api/v1` client over URLSession. The bearer token is added per-request
/// from `Session`; JSON uses snake_case ⇄ camelCase conversion so the Swift
/// models stay idiomatic.
struct PlurxAPI {
    let origin: String
    /// A cold embedded-subtitle extraction can require one full sequential
    /// read of a large MKV before the HLS session exists. Keep ordinary API
    /// calls brisk, but let this explicit playback-preparation action finish.
    static let playbackPreparationTimeout: TimeInterval = 180
    /// A local-network request may be the operation that causes iOS to show
    /// its permission sheet. The first request must wait for that choice,
    /// rather than failing underneath the sheet and making login work only on
    /// the second attempt.
    private static let waitingSession: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.waitsForConnectivity = true
        configuration.timeoutIntervalForRequest = 30
        configuration.timeoutIntervalForResource = 30
        return URLSession(configuration: configuration)
    }()
    private static let playbackPreparationSession: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.waitsForConnectivity = true
        configuration.timeoutIntervalForRequest = playbackPreparationTimeout
        configuration.timeoutIntervalForResource = playbackPreparationTimeout
        return URLSession(configuration: configuration)
    }()
    /// Sign Out is best effort, but never unbounded: a dead server must not
    /// keep the viewer trapped in a session they are trying to leave.
    private static let logoutSession: URLSession = {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = 5
        configuration.timeoutIntervalForResource = 5
        return URLSession(configuration: configuration)
    }()
    private var session: URLSession { Self.waitingSession }

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()
    private static let encoder: JSONEncoder = {
        let e = JSONEncoder()
        e.keyEncodingStrategy = .convertToSnakeCase
        return e
    }()

    private func makeURL(_ path: String, query: [URLQueryItem] = []) -> URL? {
        guard var comps = URLComponents(string: origin + "/api/v1/" + path) else { return nil }
        if !query.isEmpty { comps.queryItems = query }
        return comps.url
    }

    private func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        guard let url = makeURL(path, query: query) else { throw APIError.badURL }
        var req = URLRequest(url: url)
        Session.shared.authorize(&req)
        return try await run(req)
    }

    private func post<B: Encodable, T: Decodable>(
        _ path: String,
        query: [URLQueryItem] = [],
        body: B,
        using session: URLSession? = nil
    ) async throws -> T {
        var req = try jsonRequest(path, query: query, body: body)
        Session.shared.authorize(&req)
        return try await run(req, using: session)
    }

    private func post<T: Decodable>(_ path: String) async throws -> T {
        guard let url = makeURL(path) else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        Session.shared.authorize(&req)
        return try await run(req)
    }

    private func put<B: Encodable, T: Decodable>(_ path: String, body: B) async throws -> T {
        var req = try jsonRequest(path, body: body)
        req.httpMethod = "PUT"
        Session.shared.authorize(&req)
        return try await run(req)
    }

    private func putNoContent<B: Encodable>(_ path: String, body: B) async throws {
        var req = try jsonRequest(path, body: body)
        req.httpMethod = "PUT"
        Session.shared.authorize(&req)
        let (_, resp) = try await session.data(for: req)
        try Self.check(resp)
    }

    private func deleteNoContent(_ path: String, query: [URLQueryItem] = []) async throws {
        guard let url = makeURL(path, query: query) else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "DELETE"
        Session.shared.authorize(&req)
        let (_, resp) = try await session.data(for: req)
        try Self.check(resp)
    }

    private func postNoContent(_ path: String) async throws {
        guard let url = makeURL(path) else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        Session.shared.authorize(&req)
        let (_, resp) = try await session.data(for: req)
        try Self.check(resp)
    }

    /// Revoke one captured bearer at this API's captured origin.
    ///
    /// This deliberately bypasses `Session.authorize`: a late request must not
    /// borrow a newer login's token or send the old token to a newly selected
    /// server.
    func logout(token: String) async throws {
        guard let url = makeURL("auth/logout") else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let (_, resp) = try await Self.logoutSession.data(for: req)
        try Self.check(resp)
    }

    private func postNoContent<B: Encodable>(_ path: String, body: B) async throws {
        var req = try jsonRequest(path, body: body)
        Session.shared.authorize(&req)
        let (_, resp) = try await session.data(for: req)
        try Self.check(resp)
    }

    private func jsonRequest<B: Encodable>(
        _ path: String,
        query: [URLQueryItem] = [],
        body: B
    ) throws -> URLRequest {
        guard let url = makeURL(path, query: query) else { throw APIError.badURL }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        req.httpBody = try Self.encoder.encode(body)
        return req
    }

    private func run<T: Decodable>(
        _ req: URLRequest,
        using session: URLSession? = nil
    ) async throws -> T {
        let data: Data
        let resp: URLResponse
        do { (data, resp) = try await (session ?? self.session).data(for: req) }
        catch { throw Self.transportError(from: error) }
        try Self.check(resp, data: data)
        return try Self.decoder.decode(T.self, from: data)
    }

    /// Cancellation is control flow, not a server failure. Keeping it typed
    /// lets view models distinguish SwiftUI replacing a task from the server
    /// actually becoming unreachable. URLSession reports cancellation as
    /// either Swift's CancellationError or NSURLErrorCancelled depending on
    /// which layer observes it first.
    static func transportError(from error: Error) -> Error {
        if error is CancellationError { return CancellationError() }
        if let urlError = error as? URLError, urlError.code == .cancelled {
            return CancellationError()
        }
        return APIError.transport(error.localizedDescription)
    }

    /// Every refusal the server explained, kept instead of thrown away.
    ///
    /// The playback surface contract classifies on the server's `code`
    /// (PLAYBACK-SURFACE-CONTRACT.md §3.3), so a 503 `startup_timeout` can be
    /// "still building" rather than "Server returned 503". Two shapes are
    /// preserved deliberately:
    ///
    /// * `401`/`403` short-circuit to `.http(status)` before the body is read
    ///   at all, because `AppModel.isSessionExpired` matches on the status and
    ///   a server that starts explaining its 401 must not log the viewer out
    ///   any less reliably.
    /// * `409` keeps `.conflict(code:message:)` for the matchers that predate
    ///   this, so nothing that reads a session-create conflict changes.
    ///
    /// Everything else with a legible `{code, message}` body becomes
    /// `.refused`; anything bodiless or unparseable stays `.http(status)`.
    static func check(_ resp: URLResponse, data: Data? = nil) throws {
        guard let http = resp as? HTTPURLResponse,
              !(200..<300).contains(http.statusCode) else { return }
        if http.statusCode == 401 || http.statusCode == 403 {
            throw APIError.http(http.statusCode)
        }
        if let data, data.count <= 16_384,
           let detail = try? JSONDecoder().decode(Refusal.self, from: data),
           !detail.code.isEmpty, !detail.message.isEmpty {
            let code = String(detail.code.prefix(80))
            let message = String(detail.message.prefix(512))
            if http.statusCode == 409 {
                throw APIError.conflict(code: code, message: message)
            }
            throw APIError.refused(
                status: http.statusCode,
                code: code,
                message: message,
                positionMs: detail.filmPositionMs
            )
        }
        throw APIError.http(http.statusCode)
    }

    // MARK: - Endpoints

    /// Server identity is public and is also used to verify a rediscovered
    /// endpoint. Never attach a saved bearer token while probing candidates.
    func serverInfo() async throws -> ServerInfo {
        guard let url = makeURL("server") else { throw APIError.badURL }
        return try await run(URLRequest(url: url))
    }
    /// The other ingresses this household's media may be retried through.
    /// Signed-in only, and deliberately not part of `serverInfo()`: that call
    /// probes candidates that have not been trusted yet, so it carries no
    /// credential and must not receive the cluster's addresses either.
    func clusterIngress() async throws -> ClusterIngress { try await get("cluster/ingress") }

    func login(_ body: LoginRequest) async throws -> LoginResponse { try await post("auth/login", body: body) }
    func me() async throws -> User { try await get("me") }
    func libraries() async throws -> [Library] { try await get("libraries") }

    func libraryItems(
        _ id: Int,
        sort: LibrarySort = .title,
        offset: Int = 0,
        limit: Int = 200
    ) async throws -> Page {
        try await get("libraries/\(id)/items", query: [
            URLQueryItem(name: "limit", value: String(limit)),
            URLQueryItem(name: "offset", value: String(offset)),
            URLQueryItem(name: "sort", value: sort.rawValue),
        ])
    }

    func hubs() async throws -> Hubs { try await get("hubs") }
    func comingSoon() async throws -> ComingSoonResponse { try await get("coming-soon") }
    func item(_ id: Int) async throws -> ItemDetail { try await get("items/\(id)") }

    func readingState(itemId: Int, fileId: Int) async throws -> ReadingStateResponse {
        try await get("items/\(itemId)/reading-state", query: [
            URLQueryItem(name: "file_id", value: String(fileId)),
        ])
    }

    func openPublication(fileId: Int) async throws -> OpenPublicationResponse {
        try await post("files/\(fileId)/publication")
    }

    func closePublication(sessionId: String) async throws {
        try await deleteNoContent("publication/\(sessionId)")
    }

    func bookContentRequest(
        fileId: Int,
        accept: String = "application/epub+zip"
    ) throws -> URLRequest {
        guard let url = makeURL("files/\(fileId)/content") else { throw APIError.badURL }
        var request = URLRequest(url: url)
        Session.shared.authorize(&request)
        request.setValue(accept, forHTTPHeaderField: "Accept")
        return request
    }

    func publicationResourceRequest(base: String, path: String) throws -> URLRequest {
        guard let root = URL(string: origin),
              let baseURL = URL(string: base, relativeTo: root),
              let url = URL(string: path, relativeTo: baseURL)?.absoluteURL,
              url.scheme == root.scheme, url.host == root.host, url.port == root.port
        else { throw APIError.badURL }
        var request = URLRequest(url: url)
        request.setValue("no-referrer", forHTTPHeaderField: "Referrer-Policy")
        return request
    }

    func putReadingState(itemId: Int, state: PutReadingStateRequest) async throws -> ReadingState {
        try await put("items/\(itemId)/reading-state", body: state)
    }

    func deleteReadingState(itemId: Int, fileId: Int) async throws {
        try await deleteNoContent("items/\(itemId)/reading-state", query: [
            URLQueryItem(name: "file_id", value: String(fileId)),
        ])
    }

    func setWatched(itemId: Int, watched: Bool) async throws -> MutationResponse {
        try await post("items/\(itemId)/\(watched ? "scrobble" : "unscrobble")")
    }

    func search(_ query: String, limit: Int = 200) async throws -> SearchResponse {
        try await get("search", query: [
            URLQueryItem(name: "q", value: query),
            URLQueryItem(name: "limit", value: String(limit)),
        ])
    }

    private struct DecisionBody: Encodable {
        let caps: DeviceCaps
    }

    /// Ask with caps v2, then use the unchanged query only when the peer is
    /// old enough not to know the route or the document version. Other errors
    /// are real failures; hiding a 401 or 500 behind a second request makes the
    /// viewer's error both slower and less truthful.
    func decision(
        fileId: Int,
        caps: DeviceCaps,
        query: [URLQueryItem],
        legacyQuery: () -> [URLQueryItem]
    ) async throws -> Decision {
        do {
            return try await post(
                "files/\(fileId)/decision",
                query: query,
                body: DecisionBody(caps: caps)
            )
        } catch {
            guard Self.shouldFallBackToLegacyDecision(after: error) else { throw error }
            return try await get("files/\(fileId)/decision", query: legacyQuery())
        }
    }

    /// Behaviour-preserving across the `.refused` split: a server that now
    /// explains its 400/404/405 in a typed body answers `.refused` rather than
    /// `.http`, and the legacy decision fallback has to keep firing for it or
    /// an older server becomes unreachable the day it grows a code.
    static func shouldFallBackToLegacyDecision(after error: Error) -> Bool {
        guard let status = (error as? APIError)?.httpStatus else { return false }
        return status == 400 || status == 404 || status == 405
    }

    func pgsOverlayManifest(
        fileId: Int,
        trackIndex: Int
    ) async throws -> PGSOverlayManifestFetch {
        guard let url = makeURL("files/\(fileId)/subs/\(trackIndex)/overlay.json") else {
            throw APIError.badURL
        }
        var request = URLRequest(url: url)
        Session.shared.authorize(&request)
        let data: Data
        let response: URLResponse
        do { (data, response) = try await session.data(for: request) }
        catch { throw Self.transportError(from: error) }
        guard let http = response as? HTTPURLResponse else {
            throw APIError.transport("The PGS overlay response was not HTTP.")
        }
        switch PGSOverlayPolicy.manifestDisposition(http.statusCode) {
        case .ready:
            return .ready(try Self.decoder.decode(PGSOverlayManifest.self, from: data))
        case .preparing where http.statusCode == 202:
            let state = try Self.decoder.decode(PGSOverlayPreparing.self, from: data)
            guard state.state == "preparing" else { throw PGSOverlayError.invalidManifest }
            return .preparing(retryAfterMs: min(max(250, state.retryAfterMs), 5_000))
        case .preparing:
            return .preparing(retryAfterMs: PGSOverlayPolicy.retryAfterMs(
                http.value(forHTTPHeaderField: "Retry-After")
            ))
        case .terminal:
            throw APIError.http(http.statusCode)
        }
    }

    func pgsOverlayObject(
        fileId: Int,
        trackIndex: Int,
        generation: String,
        path: String
    ) async throws -> Data {
        guard PGSOverlayManifest.objectHash(from: path, generation: generation) != nil,
              let url = makeURL("files/\(fileId)/subs/\(trackIndex)/\(path)")
        else { throw PGSOverlayError.invalidManifest }
        var request = URLRequest(url: url)
        Session.shared.authorize(&request)
        let data: Data
        let response: URLResponse
        do { (data, response) = try await session.data(for: request) }
        catch { throw Self.transportError(from: error) }
        try Self.check(response)
        guard let http = response as? HTTPURLResponse,
              http.value(forHTTPHeaderField: "Content-Type")?
                .lowercased().hasPrefix("image/png") == true
        else { throw PGSOverlayError.invalidImage }
        return data
    }

    /// POST rather than the deprecated GET bridge: creating a session spawns a
    /// process and kills its predecessor, and anything entitled to replay a
    /// GET could spawn a second encoder. The body carries this player's
    /// `playback_id` and a per-attempt `request_id` so a replay recovers the
    /// same session instead.
    func createHlsSession(fileId: Int, body: CreateSessionRequest) async throws -> HlsStart {
        let started: HlsStart = try await post(
            "files/\(fileId)/hls/sessions",
            body: body,
            using: Self.playbackPreparationSession
        )
        return Self.acceptHlsSessionPresentation(started)
    }

    // MARK: - Library channels

    func libraryChannels(management: Bool = false) async throws -> [LibraryChannel] {
        var result: [LibraryChannel] = []
        var after: String?
        repeat {
            var query = [URLQueryItem(name: "limit", value: "100")]
            if management { query.append(URLQueryItem(name: "management", value: "true")) }
            if let after { query.append(URLQueryItem(name: "after", value: after)) }
            let page: [LibraryChannel] = try await get("library-channels", query: query)
            result += page
            after = page.count == 100 ? page.last?.id : nil
        } while after != nil
        return result
    }

    func libraryChannel(_ id: String) async throws -> LibraryChannel {
        try await get("library-channels/\(id)")
    }

    func libraryChannelGuide(ids: [String], from: Int64, to: Int64) async throws -> [LibraryChannelProgramme] {
        var programmes: [LibraryChannelProgramme] = []
        for start in stride(from: 0, to: ids.count, by: 20) {
            let group = Array(ids[start..<min(ids.count, start + 20)])
            var cursor: String?
            repeat {
                var query = [
                    URLQueryItem(name: "channel_ids", value: group.joined(separator: ",")),
                    URLQueryItem(name: "start_ms", value: String(from)),
                    URLQueryItem(name: "end_ms", value: String(to)),
                ]
                if let cursor { query.append(URLQueryItem(name: "cursor", value: cursor)) }
                guard let url = makeURL("library-channels/guide", query: query) else { throw APIError.badURL }
                var request = URLRequest(url: url)
                Session.shared.authorize(&request)
                let (data, response) = try await session.data(for: request)
                try Self.check(response)
                programmes += try Self.decoder.decode([LibraryChannelProgramme].self, from: data)
                cursor = (response as? HTTPURLResponse)?.value(forHTTPHeaderField: "X-Plurx-Next-Cursor")
            } while cursor != nil
        }
        return programmes.sorted {
            ($0.startsAtMs, $0.channelId) < ($1.startsAtMs, $1.channelId)
        }
    }

    func previewLibraryChannel(_ recipe: LibraryChannelRecipe, seed: String?) async throws -> LibraryChannelPreview {
        try await post("library-channels/preview", body: LibraryChannelPreviewRequest(recipe: recipe, limit: 50, previewSeed: seed))
    }

    func createLibraryChannel(_ definition: LibraryChannelDefinition) async throws -> LibraryChannelMutation {
        try await post("library-channels", body: definition)
    }

    func updateLibraryChannel(_ channel: LibraryChannel, definition: LibraryChannelDefinition) async throws -> LibraryChannelMutation {
        try await put("library-channels/\(channel.id)", body: LibraryChannelUpdateRequest(
            expectedRevision: channel.revision,
            requestId: definition.requestId,
            name: definition.name,
            description: definition.description,
            visibility: definition.visibility,
            enabled: definition.enabled,
            recipe: definition.recipe,
            previewSeed: definition.previewSeed
        ))
    }

    func deleteLibraryChannel(_ channel: LibraryChannel) async throws {
        try await deleteNoContent("library-channels/\(channel.id)", query: [
            URLQueryItem(name: "expected_revision", value: String(channel.revision)),
            URLQueryItem(name: "request_id", value: UUID().uuidString),
        ])
    }

    func setLibraryChannelFavourite(_ id: String, favourite: Bool) async throws {
        try await putNoContent("library-channels/\(id)/favourite", body: LibraryChannelFavouriteRequest(favourite: favourite))
    }

    func rebuildLibraryChannel(_ channel: LibraryChannel, activation: String, reshuffle: Bool) async throws -> LibraryChannelBuild {
        try await post("library-channels/\(channel.id)/rebuild", body: LibraryChannelRebuildRequest(
            expectedRevision: channel.revision,
            requestId: UUID().uuidString,
            activation: activation,
            reshuffle: reshuffle
        ))
    }

    func resolveLibraryChannel(_ id: String) async throws -> LibraryChannelResolved {
        try await post("library-channels/\(id)/resolve")
    }

    func createLibraryChannelSession(
        channelId: String,
        resolved: LibraryChannelResolved,
        tuneSequence: UInt64,
        playback: CreateSessionRequest
    ) async throws -> LibraryChannelSession {
        try await post(
            "library-channels/\(channelId)/sessions",
            body: LibraryChannelSessionRequest(
                generationId: resolved.generationId,
                occurrence: resolved.occurrence,
                tuneSequence: tuneSequence,
                playback: playback
            ),
            using: Self.playbackPreparationSession
        )
    }

    func developerReadiness() async throws -> DeveloperReadiness {
        try await get("developer/readiness")
    }

    /// `vod` describes the presentation the server selected; it is not a
    /// client compatibility gate. During index recovery the same HLS contract
    /// is served by the retained live engine and arrives as `vod: false`.
    static func acceptHlsSessionPresentation(_ started: HlsStart) -> HlsStart {
        started
    }

    func hlsStatus(sessionId: String) async throws -> PlaybackSessionStatus {
        try await get("hls/\(sessionId)/status")
    }

    /// DELETE the session the moment playback ends. Without it the encoder
    /// lives on for the idle timeout plus a reaper tick — a hardware slot
    /// held for over a minute for nobody. Best-effort by design: the route is
    /// idempotent and capability-authed, and a failure to say goodbye is not
    /// worth surfacing to a viewer who has already left.
    func endHlsSession(_ sessionId: String) async {
        guard let url = makeURL("hls/\(sessionId)") else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "DELETE"
        Session.shared.authorize(&req)
        _ = try? await session.data(for: req)
    }

    func offlineOptions(
        fileId: Int,
        audioLanguage: String,
        subtitleLanguage: String,
        subtitleMode: String = "auto"
    ) async throws -> OfflineOptions {
        try await get("files/\(fileId)/offline-options", query: [
            URLQueryItem(name: "audio_lang", value: audioLanguage),
            URLQueryItem(name: "subtitle_lang", value: subtitleLanguage),
            URLQueryItem(name: "subtitle_mode", value: subtitleMode),
        ])
    }

    func createOfflinePackage(
        fileId: Int,
        body: CreateOfflinePackageRequest
    ) async throws -> OfflinePackageStatus {
        try await post("files/\(fileId)/offline-packages", body: body)
    }

    func offlinePackage(_ packageId: String) async throws -> OfflinePackageStatus {
        try await get("offline/packages/\(packageId)")
    }

    func putOfflineLease(
        packageId: String,
        token: String
    ) async throws -> OfflineLeaseResponse {
        try await put(
            "offline/packages/\(packageId)/lease",
            body: OfflineLeaseRequest(token: token)
        )
    }

    func deleteOfflinePackage(_ packageId: String) async throws {
        try await deleteNoContent("offline/packages/\(packageId)")
    }

    func completeOfflinePackage(_ packageId: String) async throws {
        try await postNoContent("offline/packages/\(packageId)/complete")
    }

    func absoluteOfflineManifest(_ path: String) -> URL? {
        guard let base = URL(string: origin) else { return nil }
        return URL(string: path, relativeTo: base)?.absoluteURL
    }

    func progress(
        itemId: Int,
        positionMs: Int,
        durationMs: Int?,
        recordedAt: Int? = nil
    ) async throws {
        try await postNoContent(
            "items/\(itemId)/progress",
            body: ProgressRequest(
                positionMs: positionMs,
                durationMs: durationMs,
                recordedAt: recordedAt
            )
        )
    }
}
