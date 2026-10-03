import Foundation

/// Source IDs are metadata strings; only B's authenticated opaque locator is a path.
struct SharedPlaybackReference: Codable, Hashable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let libraryId: String
    let itemId: String

    func validate() throws {
        guard [importId, serverId, catalogueEpoch].allSatisfy({ PlaybackFileContext.canonicalUUID($0) && $0 != "00000000-0000-0000-0000-000000000000" }),
              [libraryId, itemId].allSatisfy(PlaybackFileContext.canonicalID) else {
            throw APIError.badURL
        }
    }
}

private final class PlaybackDetailRedirectFence: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest,
                    completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

struct PlaybackFileContext: Hashable {
    let reference: SharedPlaybackReference?
    let sourceFileId: String
    let revision: String?
    let fileBase: String
    let sessionId: String?
    let lifecycleGeneration: Int64?
    private let accountGeneration: UInt64?
    private let accountOrigin: String?

    private init(reference: SharedPlaybackReference?, file: String, revision: String?,
                 base: String, session: String? = nil, generation: UInt64? = nil, origin: String? = nil, lifecycle: Int64? = nil) {
        self.reference = reference; sourceFileId = file; self.revision = revision
        fileBase = base; sessionId = session; accountGeneration = generation; accountOrigin = origin; lifecycleGeneration = lifecycle
    }

    static func local(_ id: Int) throws -> Self { try local(String(id)) }
    static func local(_ id: String) throws -> Self {
        guard canonicalID(id) else { throw APIError.badURL }
        return Self(reference: nil, file: id, revision: nil, base: "/api/v1/files/\(id)")
    }
    static func canonicalID(_ value: String) -> Bool {
        guard let id = Int64(value), id >= 0 else { return false }
        return String(id) == value
    }
    static func canonicalLifecycle(_ value: Any?) -> Int64? {
        guard let number = value as? NSNumber, ["s", "i", "l", "q"].contains(String(cString: number.objCType)), number.int64Value > 0 else { return nil }
        return number.int64Value
    }
    static func canonicalUUID(_ value: String) -> Bool {
        UUID(uuidString: value)?.uuidString.lowercased() == value
    }
    static func matches(_ value: String, _ pattern: String) -> Bool {
        value.range(of: pattern, options: .regularExpression) == value.startIndex..<value.endIndex
    }
    private static let detailSession = URLSession(configuration: .ephemeral,
                                                  delegate: PlaybackDetailRedirectFence(), delegateQueue: nil)

    static func authenticatedDetail(reference: SharedPlaybackReference, fileId: String) async throws -> Self {
        try await fetch(reference: reference, fileId: fileId, transport: detailSession)
    }
    #if DEBUG
    /// Test HTTP transport injection does not expose a raw locator constructor.
    static func authenticatedDetail(reference: SharedPlaybackReference, fileId: String,
                                    testTransport: URLSession) async throws -> Self {
        try await fetch(reference: reference, fileId: fileId, transport: testTransport)
    }
    #endif
    private static func fetch(reference: SharedPlaybackReference, fileId: String,
                              transport: URLSession) async throws -> Self {
        try reference.validate()
        let auth = Session.shared.playbackAuthorization
        guard canonicalID(fileId), let token = auth.token, !token.isEmpty,
              Session.canonicalOrigin(auth.origin) != nil
        else { throw APIError.badURL }
        let data = try await SharedDecisionClient.detail(reference: reference, transport: transport, expected: auth)
        let current = Session.shared.playbackAuthorization
        guard current.generation == auth.generation, current.origin == auth.origin,
              current.token == auth.token, data.count <= 4_194_304,
              let detail = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let files = detail["files"] as? [[String: Any]]
        else { throw APIError.badURL }
        // No first-file fallback: the exact requested Source file must be present once.
        let candidates = files.filter { ($0["file_id"] as? String) == fileId }
        guard candidates.count == 1, let file = candidates.first,
              let binding = file["reference"] as? [String: Any],
              let item = binding["item"] as? [String: Any],
              (binding["file_id"] as? String) == fileId,
              let revision = file["revision"] as? String,
              (binding["revision"] as? String) == revision,
              matches(revision, "^[0-9a-f]{64}$"),
              let base = file["file_base"] as? String
        else { throw APIError.badURL }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        struct DetailLifecycle: Decodable { let lifecycleGeneration: Int64 }
        let lifecycle = try decoder.decode(DetailLifecycle.self, from: data).lifecycleGeneration
        let fileReference = try decoder.decode(SharedPlaybackFileReference.self, from: JSONSerialization.data(withJSONObject: binding))
        guard Self.canonicalLifecycle(detail["lifecycle_generation"]) == lifecycle, Self.canonicalLifecycle(binding["lifecycle_generation"]) == lifecycle, fileReference.lifecycleGeneration == lifecycle else { throw APIError.badURL }
        let actual = try decoder.decode(SharedPlaybackReference.self,
                                        from: JSONSerialization.data(withJSONObject: item))
        guard actual == reference else { throw APIError.badURL }
        let prefix = "/api/v1/shared/imports/\(reference.importId)/files/"
        guard base.hasPrefix(prefix), matches(String(base.dropFirst(prefix.count)), "^[A-Za-z0-9_-]{236}$")
        else { throw APIError.badURL }
        return Self(reference: reference, file: fileId, revision: revision, base: base,
                    generation: auth.generation, origin: auth.origin, lifecycle: lifecycle)
    }

    func localID(expected: Int? = nil) throws -> Int {
        guard reference == nil, let id = Int(sourceFileId), String(id) == sourceFileId,
              expected == nil || expected == id else { throw APIError.badURL }
        return id
    }
    static func localCall(_ id: Int, context: Self?) throws -> Self {
        let value = try context ?? local(id)
        _ = try value.localID(expected: id)
        return value
    }
    var sourceKey: String {
        guard let reference else { return sourceFileId }
        return [accountOrigin!, String(accountGeneration!), reference.importId, reference.serverId, reference.catalogueEpoch,
                reference.libraryId, reference.itemId, sourceFileId, revision!, String(lifecycleGeneration!), fileBase].joined(separator: "|")
    }
    private func requireCurrent() throws {
        if reference != nil {
            let auth = Session.shared.playbackAuthorization
            guard auth.generation == accountGeneration, auth.origin == accountOrigin, auth.token != nil
            else { throw APIError.badURL }
        }
    }
    func withSession(_ id: String) throws -> Self {
        try requireCurrent()
        guard Self.matches(id, "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
        else { throw APIError.badURL }
        return Self(reference: reference, file: sourceFileId, revision: revision, base: fileBase,
                    session: id, generation: accountGeneration, origin: accountOrigin, lifecycle: lifecycleGeneration)
    }
    /// Closed resource grammar. Ordinary HLS/control UUID paths never pass here.
    func path(_ resource: String, query: [URLQueryItem] = []) throws -> String {
        try requireCurrent()
        guard Self.matches(resource, "^(decision|hls/sessions|direct|stream\\.mp4|subs/[0-9]{1,6}(\\.vtt|/overlay\\.json|/overlay/[0-9a-f]{64}/objects/[0-9a-f]{64}\\.png)?|chapters/[0-9]{1,6}/thumb)$")
        else { throw APIError.badURL }
        if reference != nil && (resource == "direct" || resource == "stream.mp4") && sessionId == nil {
            throw APIError.badURL
        }
        if reference != nil { try Self.validateQuery(resource, query) }
        var components = URLComponents(string: fileBase + "/" + resource)!
        var items = query
        if let sessionId, resource != "decision", resource != "hls/sessions" { items.append(URLQueryItem(name: "session", value: sessionId)) }
        if !items.isEmpty { components.queryItems = items }
        guard let result = components.string else { throw APIError.badURL }
        return result
    }
    func apiPath(_ resource: String, query: [URLQueryItem] = []) throws -> String {
        String(try path(resource, query: query).dropFirst("/api/v1/".count))
    }
    func translatedDeliveryPath(_ value: String) throws -> String {
        guard reference != nil else { return value }
        try requireCurrent()
        guard let components = URLComponents(string: value), components.scheme == nil,
              components.host == nil, components.fragment == nil,
              [fileBase + "/direct", fileBase + "/stream.mp4"].contains(components.percentEncodedPath)
        else { throw APIError.badURL }
        let session = components.queryItems?.filter { $0.name == "session" } ?? []
        guard session.count <= 1, session.allSatisfy({ $0.value == sessionId && sessionId != nil }) else { throw APIError.badURL }
        let query = components.queryItems?.filter { $0.name != "session" } ?? []
        return try path(String(components.percentEncodedPath.dropFirst(fileBase.count + 1)), query: query)
    }
    /// Validation of metadata never admits media or binds a new session.
    func validateSharedReference(_ item: SharedPlaybackReference, file: String, revision: String) throws {
        try requireCurrent()
        guard reference != nil, reference == item, sourceFileId == file, self.revision == revision,
              Self.canonicalID(file), Self.matches(revision, "^[0-9a-f]{64}$") else { throw APIError.badURL }
    }
    func validateDescriptiveURL(_ value: String) throws {
        try requireCurrent()
        guard reference != nil, !value.contains("%"),
              let parts = URLComponents(string: value), parts.scheme == nil, parts.host == nil,
              parts.fragment == nil,
              [fileBase + "/direct", fileBase + "/stream.mp4", fileBase + "/hls/sessions"].contains(parts.path)
        else { throw APIError.badURL }
        let query = parts.queryItems ?? []
        guard Set(query.map(\.name)).count == query.count, parts.query == nil || !query.isEmpty else { throw APIError.badURL }
        for field in query {
            if field.name == "session" {
                guard sessionId != nil, field.value == sessionId else { throw APIError.badURL }
            } else {
                guard parts.path == fileBase + "/stream.mp4", field.name == "audio",
                      let text = field.value, let n = Int(text), String(n) == text, (0...4095).contains(n)
                else { throw APIError.badURL }
            }
        }
    }
    func validateSessionPlaylist(_ value: String, session: String) throws {
        try requireCurrent()
        guard reference != nil, sessionId == session, Self.canonicalV4(session),
              !value.contains("%"), let parts = URLComponents(string: value),
              parts.scheme == nil, parts.host == nil, parts.fragment == nil,
              ["master.m3u8", "index.m3u8", "video.m3u8"].contains(String(parts.path.dropFirst("/api/v1/hls/\(session)/".count))),
              parts.path.hasPrefix("/api/v1/hls/\(session)/") else { throw APIError.badURL }
        let query = parts.queryItems ?? []
        guard Set(query.map(\.name)).count == query.count, value.utf8.count <= 512,
              (parts.percentEncodedQuery?.utf8.count ?? 0) <= 256,
              parts.query == nil || !query.isEmpty else { throw APIError.badURL }
        for field in query {
            let valid: Bool
            switch field.name {
            case "native": valid = field.value == "0" || field.value == "1"
            case "subtitle":
                if let text = field.value, let index = Int(text) { valid = text == "-1" || (Self.matches(text, "^[0-9]+$") && (0...4095).contains(index)) } else { valid = false }
            case "diagnostic": valid = ["video-only", "video-only-codecs", "video-only-range", "video-only-hdr"].contains(field.value ?? "")
            default: valid = false
            }
            guard valid else { throw APIError.badURL }
        }
    }
    static func canonicalV4(_ id: String) -> Bool {
        matches(id, "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$")
    }
    private static func validateQuery(_ resource: String, _ query: [URLQueryItem]) throws {
        let capabilities = ["client", "device", "profile", "vcodec", "vmaxheight", "acodec", "container", "maxheight", "hdr", "dv", "dvprofile", "dvhls", "hdr10t"]
        let allowed = resource == "decision" ? capabilities + ["force", "audio", "subtitle", "audio_offset_ms", "achannels", "capver", "hdrtypes", "dvdecoders", "dvraw", "dvstatus"]
            : resource == "stream.mp4" ? capabilities + ["force", "audio", "audio_offset_ms", "start", "stream"]
            : resource.hasPrefix("chapters/") ? ["v"] : []
        guard Set(query.map(\.name)).count == query.count else { throw APIError.badURL }
        func integer(_ value: String, _ low: Int, _ high: Int) -> Bool {
            guard let n = Int(value), String(n) == value else { return false }; return n >= low && n <= high
        }
        for item in query {
            guard allowed.contains(item.name), let text = item.value else { throw APIError.badURL }
            let valid: Bool
            switch item.name {
            case "hdr", "dv", "dvhls", "hdr10t": valid = text == "0" || text == "1"
            case "vcodec", "acodec", "container":
                valid = text.count <= 256 && (text.isEmpty || text.split(separator: ",", omittingEmptySubsequences: false).allSatisfy { matches(String($0), "^[A-Za-z0-9_-]{1,32}$") })
            case "dvprofile":
                valid = text.count <= 64 && (text.isEmpty || text.split(separator: ",", omittingEmptySubsequences: false).allSatisfy { integer(String($0), 0, 255) })
            case "vmaxheight":
                valid = text.count <= 256 && (text.isEmpty || text.split(separator: ",", omittingEmptySubsequences: false).allSatisfy { value in
                    let parts = value.split(separator: ":", omittingEmptySubsequences: false)
                    return parts.count == 2 && matches(String(parts[0]), "^[A-Za-z0-9_-]{1,32}$") && integer(String(parts[1]), 1, 65535)
                })
            case "client", "profile": valid = matches(text, "^[A-Za-z0-9_.-]{1,64}$")
            case "device", "capver", "hdrtypes", "dvdecoders", "dvraw", "dvstatus":
                valid = text.count <= 256 && !text.unicodeScalars.contains { $0.value < 32 || $0.value == 127 }
            case "achannels": valid = integer(text, 1, 16)
            case "force": valid = ["auto", "original", "transcode"].contains(text)
            case "stream": valid = matches(text, "^[A-Za-z0-9_-]{1,200}$")
            case "start": valid = matches(text, "^(0|[1-9][0-9]{0,12})(\\.[0-9]{1,3})?$")
            case "v": valid = matches(text, "^-?(0|[1-9][0-9]{0,19})$")
            case "subtitle": valid = integer(text, -1, 999999)
            case "audio_offset_ms": valid = integer(text, -15000, 15000)
            case "maxheight": valid = integer(text, 0, 65535)
            default: valid = integer(text, 0, 999999)
            }
            guard valid else { throw APIError.badURL }
        }
    }
}
