import Foundation

private final class SharedLibraryRedirectFence: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}

/// All browsing stays on the captured authenticated B origin. References are metadata, not authority.
struct SharedLibraryClient {
    private let origin: String
    private let token: String
    private let generation: UInt64
    private let transport: URLSession
    private static let session = URLSession(configuration: .ephemeral, delegate: SharedLibraryRedirectFence(), delegateQueue: nil)
    init() throws { try self.init(transport: Self.session) }
    #if DEBUG
    init(testTransport: URLSession) throws { try self.init(transport: testTransport) }
    #endif
    private init(transport: URLSession) throws {
        let auth = Session.shared.playbackAuthorization
        guard Session.canonicalOrigin(auth.origin) != nil, let token = auth.token, !token.isEmpty else { throw APIError.badURL }
        origin = auth.origin; self.token = token; generation = auth.generation; self.transport = transport
    }
    func requireCurrent() throws {
        let auth = Session.shared.playbackAuthorization
        guard auth.origin == origin, auth.token == token, auth.generation == generation else { throw APIError.badURL }
    }
    private func request(_ path: String, query: [URLQueryItem] = [], enabled: Bool? = nil) async throws -> Data {
        try requireCurrent()
        var components = URLComponents(string: origin + "/api/v1/" + path)!
        if !query.isEmpty {
            components.queryItems = query
            // Axum form decoding treats '+' as a space; preserve opaque cursors and searches.
            components.percentEncodedQuery = components.percentEncodedQuery?.replacingOccurrences(of: "+", with: "%2B")
        }
        guard let url = components.url else { throw APIError.badURL }
        var request = URLRequest(url: url); request.timeoutInterval = 30
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let enabled {
            request.httpMethod = "PUT"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: ["enabled": enabled])
        }
        let (data, response) = try await transport.data(for: request)
        try requireCurrent()
        guard let response = response as? HTTPURLResponse, response.url == url,
              data.count <= 4_194_304 else { throw APIError.badURL }
        guard (200...299).contains(response.statusCode) else {
            struct Refusal: Decodable { let code: String; let message: String }
            if let refusal = try? JSONDecoder().decode(Refusal.self, from: data) {
                throw APIError.refused(status: response.statusCode, code: refusal.code, message: refusal.message, positionMs: nil)
            }
            throw APIError.http(response.statusCode)
        }
        return data
    }
    private func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }
    func assignments() async throws -> [SharedLibraryAssignment] {
        struct Reply: Decodable { let libraries: [SharedLibraryAssignment] }
        let rows = try decode(Reply.self, await request("shared/libraries")).libraries
        guard rows.count <= 2048, Set(rows.map(\.id)).count == rows.count else { throw APIError.badURL }
        try rows.forEach { try $0.identity.validate() }
        return rows
    }
    func libraries(_ assigned: [SharedLibraryAssignment]) async throws -> [SharedLibraryRow] {
        guard let first = assigned.first else { return [] }
        try assigned.forEach { try $0.identity.validate(); guard $0.identity.sourceId == first.identity.sourceId else { throw APIError.badURL } }
        struct Reply: Decodable { let importId: String; let serverId: String; let catalogueEpoch: String; let libraries: [SharedLibraryMetadata] }
        let reply = try decode(Reply.self, await request("shared/imports/\(first.importId)/libraries"))
        guard reply.importId == first.importId, reply.serverId == first.serverId, reply.catalogueEpoch == first.catalogueEpoch,
              reply.libraries.count <= 64, Set(reply.libraries.map(\.libraryId)).count == reply.libraries.count else { throw APIError.badURL }
        try reply.libraries.forEach { guard PlaybackFileContext.canonicalID($0.libraryId) else { throw APIError.badURL } }
        let allowed = Set(assigned.map(\.libraryId))
        return reply.libraries.filter { allowed.contains($0.libraryId) }.map {
            SharedLibraryRow(identity: .init(importId: first.importId, serverId: first.serverId, catalogueEpoch: first.catalogueEpoch, libraryId: $0.libraryId),
                             name: $0.name, sourceName: first.sourceName, kind: $0.kind)
        }
    }
    func page(_ library: SharedLibraryIdentity, parent: SharedPlaybackReference? = nil, q: String = "", cursor: String? = nil) async throws -> SharedLibraryPage {
        try library.validate()
        guard q.utf8.count <= 512, !q.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }),
              (cursor?.utf8.count ?? 0) <= 4096 else { throw APIError.badURL }
        let path: String
        if let parent { try parent.validate(); guard library.contains(parent) else { throw APIError.badURL }; path = "shared/imports/\(parent.importId)/items/\(parent.itemId)/children" }
        else { path = "shared/imports/\(library.importId)/libraries/\(library.libraryId)/items" }
        var query = [URLQueryItem(name: "q", value: q), URLQueryItem(name: "limit", value: "60")]
        if let cursor { query.append(URLQueryItem(name: "cursor", value: cursor)) }
        let page = try decode(SharedLibraryPage.self, await request(path, query: query))
        try page.validate(in: library); return page
    }
    func detail(_ reference: SharedPlaybackReference) async throws -> SharedLibraryDetail {
        try reference.validate()
        let detail = try decode(SharedLibraryDetail.self, await request("shared/imports/\(reference.importId)/items/\(reference.itemId)"))
        try detail.validate(expected: reference); return detail
    }
    func settings() async throws -> Bool { try decode(SharingSetting.self, await request("sharing/settings")).enabled }
    func save(enabled: Bool) async throws -> Bool {
        let saved = try decode(SharingSetting.self, await request("sharing/settings", enabled: enabled)).enabled
        guard saved == enabled else { throw APIError.badURL }; return saved
    }
    func management(_ kind: String) async throws -> [String: SharedPlaybackJSON] {
        guard ["imports", "exports", "status"].contains(kind) else { throw APIError.badURL }
        let data = try await request("sharing/" + kind)
        guard let object = try JSONDecoder().decode(SharedPlaybackJSON.self, from: data).object else { throw APIError.badURL }
        return object
    }
    private struct SharingSetting: Decodable { let enabled: Bool }
}
