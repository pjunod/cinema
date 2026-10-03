import Foundation

/// Source IDs are metadata strings; only B's authenticated opaque locator is a path.
struct SharedPlaybackReference: Codable, Hashable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let libraryId: String
    let itemId: String

    func validate() throws {
        guard [importId, serverId, catalogueEpoch].allSatisfy(PlaybackFileContext.canonicalUUID),
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
    private let accountGeneration: UInt64?
    private let accountOrigin: String?

    private init(reference: SharedPlaybackReference?, file: String, revision: String?,
                 base: String, session: String? = nil, generation: UInt64? = nil, origin: String? = nil) {
        self.reference = reference; sourceFileId = file; self.revision = revision
        fileBase = base; sessionId = session; accountGeneration = generation; accountOrigin = origin
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
              Session.canonicalOrigin(auth.origin) != nil,
              let url = URL(string: auth.origin + "/api/v1/shared/imports/\(reference.importId)/items/\(reference.itemId)")
        else { throw APIError.badURL }
        var request = URLRequest(url: url); request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let (data, response) = try await transport.data(for: request)
        let current = Session.shared.playbackAuthorization
        guard current.generation == auth.generation, current.origin == auth.origin,
              current.token == auth.token, let http = response as? HTTPURLResponse,
              http.statusCode == 200, response.url == url, data.count <= 1_048_576,
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
        let actual = try decoder.decode(SharedPlaybackReference.self,
                                        from: JSONSerialization.data(withJSONObject: item))
        guard actual == reference else { throw APIError.badURL }
        let prefix = "/api/v1/shared/imports/\(reference.importId)/files/"
        guard base.hasPrefix(prefix), matches(String(base.dropFirst(prefix.count)), "^[A-Za-z0-9_-]{1,2048}$")
        else { throw APIError.badURL }
        return Self(reference: reference, file: fileId, revision: revision, base: base,
                    generation: auth.generation, origin: auth.origin)
    }

    var sourceKey: String {
        guard let reference else { return sourceFileId }
        return [String(accountGeneration!), reference.importId, reference.serverId, reference.catalogueEpoch,
                reference.libraryId, reference.itemId, sourceFileId, revision!, fileBase].joined(separator: "|")
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
                    session: id, generation: accountGeneration, origin: accountOrigin)
    }
    /// Closed resource grammar. Ordinary HLS/control UUID paths never pass here.
    func path(_ resource: String) throws -> String {
        try requireCurrent()
        guard Self.matches(resource, "^(decision|hls/sessions|direct|stream\\.mp4|subs/[0-9]{1,6}(\\.vtt|/overlay\\.json|/overlay/[0-9a-f]{64}/objects/[0-9a-f]{64}\\.png)?|chapters/[0-9]{1,6}/thumb)$")
        else { throw APIError.badURL }
        if reference != nil && (resource == "direct" || resource == "stream.mp4") && sessionId == nil {
            throw APIError.badURL
        }
        var value = fileBase + "/" + resource
        if let sessionId, resource != "decision", resource != "hls/sessions" { value += "?session=\(sessionId)" }
        return value
    }
    func apiPath(_ resource: String) throws -> String { String(try path(resource).dropFirst("/api/v1/".count)) }
}
