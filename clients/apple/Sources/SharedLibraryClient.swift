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
    private func request(_ path: String, query: [URLQueryItem] = [], enabled: Bool? = nil, post: [String: Any]? = nil) async throws -> Data {
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
        } else if let post {
            request.httpMethod = "POST"; request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: post, options: [.sortedKeys])
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
    func page(_ library: SharedLibraryIdentity, parent: SharedPlaybackReference? = nil, q: String = "", cursor: String? = nil, limit: Int = 60) async throws -> SharedLibraryPage {
        try library.validate()
        guard q.utf8.count <= 512, !q.unicodeScalars.contains(where: { $0.value < 32 || $0.value == 127 }),
              (cursor?.utf8.count ?? 0) <= 4096 else { throw APIError.badURL }
        let path: String
        if let parent { try parent.validate(); guard library.contains(parent) else { throw APIError.badURL }; path = "shared/imports/\(parent.importId)/items/\(parent.itemId)/children" }
        else { path = "shared/imports/\(library.importId)/libraries/\(library.libraryId)/items" }
        guard (1...200).contains(limit) else { throw APIError.badURL }
        var query = [URLQueryItem(name: "q", value: q), URLQueryItem(name: "limit", value: String(limit))]
        if let cursor { query.append(URLQueryItem(name: "cursor", value: cursor)) }
        var page = try decode(SharedLibraryPage.self, await request(path, query: query))
        try page.validate(in: library); try requireCurrent()
        for index in page.items.indices { try page.items[index].bindArtwork(artworkSubject(page.items[index])) }; return page
    }
    func detail(_ reference: SharedPlaybackReference) async throws -> SharedLibraryDetail {
        try reference.validate()
        var detail = try decode(SharedLibraryDetail.self, await request("shared/imports/\(reference.importId)/items/\(reference.itemId)"))
        try detail.validate(expected: reference); try requireCurrent()
        try detail.item.bindArtwork(artworkSubject(detail.item)); return detail
    }
    /// B-private explicit watched state for a Shared movie or episode (§5.3).
    /// B takes the next history sequence inside the write, so a progress beat
    /// sent before this cannot restore the old position; the next beat this
    /// client sends resyncs from fresh details on its typed conflict.
    func setWatched(_ reference: SharedPlaybackReference, watched: Bool) async throws -> SharedLibraryWatch {
        try reference.validate()
        struct Reply: Decodable { let updated: Int; let watch: SharedLibraryWatch }
        let reply = try decode(Reply.self, await request("shared/imports/\(reference.importId)/items/\(reference.itemId)/watched", post: ["watched": watched]))
        try requireCurrent()
        let watch = reply.watch
        guard reply.updated == 1, watch.watched == watched, watch.positionMs >= 0, watch.sequence >= 0, watch.updatedAtMs >= 0,
              watch.durationMs.map({ $0 >= 0 }) ?? true else { throw APIError.badURL }
        return watch
    }
    /// Next episode in Source order, read only through B's viewer routes: the
    /// next episode of the season, else the first episode of the next season.
    /// The answer is a full Shared reference for a fresh authorized Start; it
    /// never derives a Local item ID and carries no playback authority.
    func nextEpisode(after reference: SharedPlaybackReference) async throws -> SharedPlaybackReference? {
        try reference.validate()
        let library = SharedLibraryIdentity(importId: reference.importId, serverId: reference.serverId,
                                            catalogueEpoch: reference.catalogueEpoch, libraryId: reference.libraryId)
        let current = try await detail(reference).item
        guard current.kind == "episode", let season = current.parent else { return nil }
        let episodes = try await children(of: season, kind: "episode", in: library)
        guard let at = episodes.firstIndex(of: reference) else { return nil }
        if episodes.indices.contains(at + 1) { return episodes[at + 1] }
        guard let series = try await detail(season).item.parent else { return nil }
        let seasons = try await children(of: series, kind: "season", in: library)
        guard let index = seasons.firstIndex(of: season), seasons.indices.contains(index + 1) else { return nil }
        return try await children(of: seasons[index + 1], kind: "episode", in: library).first
    }
    /// Every child of one kind, in B's order, at most ten bounded pages.
    private func children(of parent: SharedPlaybackReference, kind: String, in library: SharedLibraryIdentity) async throws -> [SharedPlaybackReference] {
        var rows: [SharedPlaybackReference] = [], seen = Set<SharedPlaybackReference>(), cursors = Set<String>()
        var cursor: String?
        for _ in 0..<10 {
            let page = try await self.page(library, parent: parent, cursor: cursor, limit: 200)
            for item in page.items where item.kind == kind && seen.insert(item.reference).inserted { rows.append(item.reference) }
            guard let next = page.nextCursor else { return rows }
            guard cursors.insert(next).inserted else { throw APIError.badURL }
            cursor = next
        }
        throw APIError.transport("This Shared season is too long to follow.")
    }
    private func artworkSubject(_ item: SharedLibraryItem) throws -> SharedArtworkSubject {
        try requireCurrent()
        return SharedArtworkSubject(reference: item.reference, descriptors: item.art ?? [], poster: item.posterUrl,
            backdrop: item.backdropUrl, origin: origin, token: token, generation: generation, configuration: transport.configuration)
    }
    func continueGroups() async throws -> [SharedContinueGroup] {
        struct Reply: Decodable { let groups: [SharedContinueGroup] }
        let groups = try decode(Reply.self, await request("shared/continue-watching", query: [URLQueryItem(name: "limit", value: "200")])).groups
        guard groups.count <= 32, Set(groups.map(\.id)).count == groups.count else { throw APIError.badURL }
        try groups.forEach { try $0.validate() }; try requireCurrent(); return groups
    }
    func continueItems(_ group: SharedContinueGroup, assigned: [SharedLibraryAssignment]) async throws -> SharedContinueItems {
        try group.validate()
        var reply = try decode(SharedContinueItems.self, await request("shared/imports/\(group.importId)/continue-watching", query: [URLQueryItem(name: "limit", value: "200")]))
        try reply.validate(group: group, assigned: assigned); try requireCurrent()
        for index in reply.items.indices { try reply.items[index].item.bindArtwork(artworkSubject(reply.items[index].item)) }
        return reply
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

/// Constructed only by the authenticated metadata client in this file.
final class SharedArtworkSubject {
    let reference: SharedPlaybackReference
    let descriptors: [SharedArtworkDescriptor]
    let poster: String?
    let backdrop: String?
    private let origin: String
    private let token: String
    let generation: UInt64
    private let configuration: URLSessionConfiguration
    fileprivate init(reference: SharedPlaybackReference, descriptors: [SharedArtworkDescriptor], poster: String?, backdrop: String?,
                     origin: String, token: String, generation: UInt64, configuration: URLSessionConfiguration) {
        self.reference = reference; self.descriptors = descriptors; self.poster = poster; self.backdrop = backdrop
        self.origin = origin; self.token = token; self.generation = generation; self.configuration = configuration
    }
    func requireCurrent() throws {
        let auth = Session.shared.playbackAuthorization
        guard auth.origin == origin, auth.token == token, auth.generation == generation else { throw APIError.badURL }
    }
    var key: String { [origin, String(generation), reference.importId, reference.serverId, reference.catalogueEpoch, reference.libraryId, reference.itemId].joined(separator: "|") }
    func descriptor(backdrop: Bool) throws -> SharedArtworkDescriptor? {
        try requireCurrent()
        let path = backdrop ? self.backdrop : poster
        guard let path else { return nil }
        return descriptors.first { $0.url == path && $0.kind == (backdrop ? "backdrop" : "poster") && $0.variant == (backdrop ? "w780" : "w300") }
    }
    func read(_ descriptor: SharedArtworkDescriptor) async throws -> SharedArtworkBytes {
        try Task.checkCancellation(); try requireCurrent(); try descriptor.validate(reference: reference)
        guard descriptors.contains(descriptor) else { throw APIError.badURL }
        let admission = try SharedArtworkBudget.compressed.acquire(SharedArtworkBudget.assetLimit)
        guard let url = URL(string: origin + descriptor.url) else { throw APIError.badURL }
        var request = URLRequest(url: url); request.timeoutInterval = 30
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        let plan = SharedArtworkRequest(request: request, configuration: configuration, admission: admission, current: { [self] in try requireCurrent() })
        let reader = SharedArtworkReadOperation(plan: plan)
        let result = try await reader.read()
        try Task.checkCancellation(); try requireCurrent(); return SharedArtworkBytes(data: result.0, mime: result.1, admission: admission)
    }
}

/// No caller can construct an arbitrary URL request plan outside this
/// authenticated subject factory file.
struct SharedArtworkRequest {
    let request: URLRequest
    let configuration: URLSessionConfiguration
    let admission: SharedArtworkBudget.Lease
    let current: () throws -> Void
    fileprivate init(request: URLRequest, configuration: URLSessionConfiguration, admission: SharedArtworkBudget.Lease, current: @escaping () throws -> Void) {
        self.request = request; self.configuration = configuration; self.admission = admission; self.current = current
    }
}
