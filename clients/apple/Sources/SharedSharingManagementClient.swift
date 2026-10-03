import Foundation

private final class SharedManagementRedirectFence: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) { completionHandler(nil) }
}
struct SharedSharingManagementClient {
    private let auth: (origin: String, token: String?, generation: UInt64)
    private let transport: URLSession
    private static let session = URLSession(configuration: .ephemeral, delegate: SharedManagementRedirectFence(), delegateQueue: nil)
    init() throws { try self.init(transport: Self.session) }
    #if DEBUG
    init(testTransport: URLSession) throws { try self.init(transport: testTransport) }
    #endif
    private init(transport: URLSession) throws {
        let value = Session.shared.playbackAuthorization
        guard Session.canonicalOrigin(value.origin) != nil, value.token?.isEmpty == false else { throw APIError.badURL }
        auth = value; self.transport = transport
    }
    func requireCurrent() throws {
        let current = Session.shared.playbackAuthorization
        guard current.origin == auth.origin, current.token == auth.token, current.generation == auth.generation else { throw APIError.badURL }
    }
    private func id(_ value: String) throws -> String {
        guard PlaybackFileContext.canonicalUUID(value) else { throw APIError.badURL }; return value
    }
    private func request(_ path: String, method: String = "GET", body: [String: SharedPlaybackJSON]? = nil, after: String? = nil) async throws -> Data {
        try requireCurrent()
        var components = URLComponents(string: auth.origin + "/api/v1/" + path)!
        if let after { components.queryItems = [URLQueryItem(name: "after", value: try id(after))] }
        guard let url = components.url else { throw APIError.badURL }
        var request = URLRequest(url: url); request.httpMethod = method; request.timeoutInterval = 30
        request.setValue("Bearer \(auth.token!)", forHTTPHeaderField: "Authorization")
        if let body {
            let data = try JSONEncoder().encode(body)
            guard data.count <= 16_384 else { throw SharedSharingManagementError.invalid("Sharing update exceeds the server’s 16 KiB mutation limit. No request was sent.") }
            request.httpBody = data; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }
        let (data, response) = try await transport.data(for: request); try requireCurrent()
        guard let http = response as? HTTPURLResponse, http.url == url else { throw APIError.badURL }
        if http.statusCode == 401 || http.statusCode == 403 { SharedSharingSecretDraft.retireAuthorization(generation: auth.generation) }
        guard data.count <= ((path == "libraries" || path == "users" || path.hasSuffix("/assignments")) ? 4_194_304 : 131_072) else { throw APIError.badURL }
        guard (200...299).contains(http.statusCode) else {
            struct Refusal: Decodable { let code: String; let message: String }
            if let refusal = try? JSONDecoder().decode(Refusal.self, from: data) { throw APIError.refused(status: http.statusCode, code: refusal.code, message: refusal.message, positionMs: nil) }
            throw APIError.http(http.statusCode)
        }
        return data
    }
    private func decode<T: Decodable>(_ type: T.Type, _ data: Data) throws -> T {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(type, from: data)
    }
    func libraries() async throws -> [SharedSharingLocalLibrary] {
        let rows = try decode([SharedSharingLocalLibrary].self, await request("libraries"))
        guard rows.count <= 4096, Set(rows.map(\.id)).count == rows.count, rows.allSatisfy({ $0.id >= 0 }) else { throw APIError.badURL }
        return rows.filter { ["movies", "shows"].contains($0.kind) }
    }
    func viewers() async throws -> [SharedSharingViewer] {
        let rows = try decode([SharedSharingViewer].self, await request("users"))
        guard rows.count <= 4096, Set(rows.map(\.id)).count == rows.count, rows.allSatisfy({ $0.id >= 0 }) else { throw APIError.badURL }; return rows
    }
    func imports() async throws -> [SharedSharingImport] {
        struct Reply: Decodable { let imports: [SharedSharingImport] }
        let rows = try decode(Reply.self, await request("sharing/imports")).imports
        guard rows.count <= 32, Set(rows.map(\.id)).count == rows.count else { throw APIError.badURL }
        try rows.forEach { try $0.validate() }; return rows
    }
    func exports(after: String? = nil) async throws -> (rows: [SharedSharingExport], next: String?) {
        struct Reply: Decodable { let exports: [SharedSharingExport]; let next: String? }
        let reply = try decode(Reply.self, await request("sharing/exports", after: after))
        guard reply.exports.count <= 32, Set(reply.exports.map(\.id)).count == reply.exports.count,
              reply.next.map({ PlaybackFileContext.canonicalUUID($0) && $0 != after }) ?? true else { throw APIError.badURL }
        try reply.exports.forEach { try $0.validate() }; return (reply.exports, reply.next)
    }
    func invite(libraries: [String], ttlSeconds: Int64 = 86400) async throws -> SharedSharingInvitation {
        try SharedSharingValidation.ids(libraries)
        guard !libraries.isEmpty, (1...604800).contains(ttlSeconds) else { throw APIError.badURL }
        let row = try decode(SharedSharingInvitation.self, await request("sharing/invitations", method: "POST", body: ["library_ids": .array(libraries.map(SharedPlaybackJSON.string)), "ttl_seconds": .integer(ttlSeconds)]))
        guard PlaybackFileContext.canonicalUUID(row.id), row.invitation.hasPrefix("cinema-share-v1:"), row.invitation.utf8.count <= 10939 else { throw APIError.badURL }; return row
    }
    private func invitation(_ value: String) throws -> SharedPlaybackJSON {
        guard value.hasPrefix("cinema-share-v1:"), value.utf8.count <= 10939, PlaybackFileContext.matches(String(value.dropFirst(16)), "^[A-Za-z0-9_-]{1,10923}$") else { throw SharedSharingManagementError.invalid("Paste a valid cinema-share-v1 invitation.") }; return .string(value)
    }
    func importSource(invitation value: String) async throws -> SharedSharingImport {
        let row = try decode(SharedSharingImport.self, await request("sharing/imports", method: "POST", body: ["invitation": try invitation(value)])); try row.validate(); return row
    }
    func rePair(_ row: SharedSharingImportSummary, invitation value: String) async throws -> SharedSharingImport {
        guard row.lifecycleGeneration >= 1 else { throw APIError.badURL }
        let result = try decode(SharedSharingImport.self, await request("sharing/imports/\(id(row.id))/re-pair", method: "POST", body: ["expected_lifecycle_generation": .integer(row.lifecycleGeneration), "invitation": try invitation(value)]))
        try result.validate(); guard result.id == row.id, result.import.sourceServerId == row.sourceServerId, result.import.catalogueEpoch == row.catalogueEpoch else { throw APIError.badURL }; return result
    }
    func rotate(_ row: SharedSharingImportSummary) async throws -> SharedSharingImport {
        guard row.state == "active" else { throw SharedSharingManagementError.invalid("Source must be active to rotate its credential.") }
        let result = try decode(SharedSharingImport.self, await request("sharing/imports/\(id(row.id))/rotate", method: "POST", body: [:]))
        try result.validate(); guard result.id == row.id, result.import.sourceServerId == row.sourceServerId, result.import.catalogueEpoch == row.catalogueEpoch else { throw APIError.badURL }; return result
    }
    private func mutation(_ path: String, method: String, body: [String: SharedPlaybackJSON]? = nil, resultKey: String = "updated") async throws {
        let wire = try JSONDecoder().decode([String: SharedPlaybackJSON].self, from: await request(path, method: method, body: body))
        guard wire[resultKey] == .bool(true) else { throw APIError.badURL }
    }
    func approve(_ row: SharedSharingExport, enteredCode: String) async throws {
        try row.validate(); guard SharedSharingValidation.code(enteredCode), enteredCode == row.pairingCode else { throw SharedSharingManagementError.invalid("Enter the matching 16-character pairing code shown on the importing server.") }
        try await mutation("sharing/exports/\(id(row.id))/approve", method: "POST", body: ["expected_mutation_generation": .integer(row.grant.mutationGeneration), "pairing_code": .string(enteredCode)])
    }
    func scope(_ row: SharedSharingExport, libraries: [String]) async throws {
        try row.validate(); try SharedSharingValidation.ids(libraries)
        try await mutation("sharing/exports/\(id(row.id))/libraries", method: "PUT", body: ["expected_mutation_generation": .integer(row.grant.mutationGeneration), "library_ids": .array(libraries.map(SharedPlaybackJSON.string))])
    }
    func cancelInvitation(_ value: String) async throws { try await mutation("sharing/invitations/\(id(value))", method: "DELETE", resultKey: "cancelled") }
    func revoke(_ value: String) async throws { try await mutation("sharing/exports/\(id(value))", method: "DELETE", resultKey: "revoked") }
    func disconnect(_ value: String) async throws { try await mutation("sharing/imports/\(id(value))", method: "DELETE", resultKey: "disabled") }
    func endpoints() async throws -> SharedSharingEndpointManifest? {
        struct Reply: Decodable { let manifest: SharedSharingEndpointManifest? }
        let value = try decode(Reply.self, await request("sharing/endpoints")).manifest
        if let value {
            guard value.revision >= 0, (1...4).contains(value.endpoints.count) else { throw APIError.badURL }
            try value.endpoints.forEach { try $0.validate() }
        }
        return value
    }
    private func endpointBody(_ endpoints: [SharedSharingEndpoint]) throws -> SharedPlaybackJSON {
        guard (1...4).contains(endpoints.count) else { throw APIError.badURL }
        try endpoints.forEach { try $0.validate() }
        return .array(endpoints.map { endpoint in .object([
            "ipv4": .string(endpoint.ipv4), "ipv6": endpoint.ipv6.map(SharedPlaybackJSON.string) ?? .null,
            "ts_fqdn": .string(endpoint.tsFqdn), "port": .integer(Int64(endpoint.port)), "spki_sha256": .string(endpoint.spkiSha256)
        ]) })
    }
    func saveManifest(expectedRevision: Int64, endpoints: [SharedSharingEndpoint]) async throws {
        guard (0..<Int64.max).contains(expectedRevision) else { throw APIError.badURL }
        try await mutation("sharing/endpoints", method: "PUT", body: ["expected_revision": .integer(expectedRevision), "endpoints": try endpointBody(endpoints)])
    }
    func saveSourceEndpoints(_ row: SharedSharingImportSummary, endpoints: [SharedSharingEndpoint], confirmNewPins: Bool) async throws {
        guard (1..<Int64.max).contains(row.endpointGeneration), ["claiming", "pending", "active"].contains(row.state) else { throw APIError.badURL }
        let oldPins = Set(row.endpoints.map(\.spkiSha256))
        guard confirmNewPins || endpoints.allSatisfy({ oldPins.contains($0.spkiSha256) }) else { throw SharedSharingManagementError.invalid("Review and explicitly confirm every new TLS pin before saving.") }
        try await mutation("sharing/imports/\(id(row.id))/endpoints", method: "PUT", body: ["expected_endpoint_generation": .integer(row.endpointGeneration), "endpoints": try endpointBody(endpoints), "confirm_new_pins": .bool(confirmNewPins)])
    }
    func assignments(_ row: SharedSharingImportSummary) async throws -> SharedSharingAssignmentSnapshot {
        let result = try decode(SharedSharingAssignmentSnapshot.self, await request("sharing/imports/\(id(row.id))/assignments"))
        try result.validate(for: row); return result
    }
    func sourceLibraries(_ row: SharedSharingImportSummary) async throws -> SharedSharingSourceLibrariesSnapshot {
        guard row.state == "active" else { throw APIError.badURL }
        let result = try decode(SharedSharingSourceLibrariesSnapshot.self, await request("sharing/imports/\(id(row.id))/libraries"))
        try result.validate(for: row); return result
    }
    func saveAssignments(_ snapshot: SharedSharingAssignmentSnapshot, for row: SharedSharingImportSummary, groups: [SharedSharingAssignmentGroup]) async throws {
        try snapshot.validate(for: row); try SharedSharingValidation.assignments(groups)
        let values = groups.map { group in SharedPlaybackJSON.object(["library_id": .string(group.libraryId), "user_ids": .array(group.userIds.map(SharedPlaybackJSON.integer))]) }
        try await mutation("sharing/imports/\(id(row.id))/assignments", method: "PUT", body: ["expected_assignment_generation": .integer(snapshot.expectedAssignmentGeneration), "assignments": .array(values)])
    }

}
