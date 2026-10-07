import Foundation

struct SharedLibraryIdentity: Codable, Hashable, Identifiable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let libraryId: String
    var id: String { [importId, serverId, catalogueEpoch, libraryId].joined(separator: "|") }
    var sourceId: String { [importId, serverId, catalogueEpoch].joined(separator: "|") }
    func reference(_ item: String) -> SharedPlaybackReference {
        SharedPlaybackReference(importId: importId, serverId: serverId, catalogueEpoch: catalogueEpoch,
                                libraryId: libraryId, itemId: item)
    }
    func validate() throws { try reference("0").validate() }
    func contains(_ reference: SharedPlaybackReference) -> Bool { self.reference(reference.itemId) == reference }
}

struct SharedLibraryAssignment: Decodable, Identifiable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let libraryId: String
    let sourceName: String
    let availability: String
    var identity: SharedLibraryIdentity { .init(importId: importId, serverId: serverId, catalogueEpoch: catalogueEpoch, libraryId: libraryId) }
    var id: String { identity.id }
}
struct SharedLibraryMetadata: Decodable {
    let libraryId: String
    let name: String
    let kind: String
    let anime: Bool
}
struct SharedLibraryRow: Identifiable, Hashable {
    let identity: SharedLibraryIdentity
    let name: String
    let sourceName: String
    let kind: String
    var id: String { identity.id }
}
struct SharedLibraryItem: Decodable, Identifiable {
    let source: String
    let reference: SharedPlaybackReference
    let parent: SharedPlaybackReference?
    let title: String
    let kind: String
    let year: Int?
    let overview: String?
    let genres: [String]
    let seasonNumber: Int?
    let episodeNumber: Int?
    let art: [SharedArtworkDescriptor]?
    let posterUrl: String?
    let backdropUrl: String?
    private(set) var artworkSubject: SharedArtworkSubject?
    private enum CodingKeys: String, CodingKey { case source, reference, parent, title, kind, year, overview, genres, seasonNumber, episodeNumber, art, posterUrl, backdropUrl }
    mutating func bindArtwork(_ subject: SharedArtworkSubject) throws {
        guard subject.reference == reference, subject.descriptors == (art ?? []) else { throw APIError.badURL }
        artworkSubject = subject
    }
    var id: String { [reference.importId, reference.serverId, reference.catalogueEpoch, reference.libraryId, reference.itemId].joined(separator: "|") }
    var hasChildren: Bool { ["series", "season", "album", "artist", "collection"].contains(kind) }
    func validate(in library: SharedLibraryIdentity) throws {
        try reference.validate()
        guard source == "shared", library.contains(reference), title.utf8.count <= 512, genres.count <= 64 else { throw APIError.badURL }
        if let parent { try parent.validate(); guard library.contains(parent) else { throw APIError.badURL } }
        try SharedArtworkDescriptor.validate(art, poster: posterUrl, backdrop: backdropUrl, reference: reference)
    }
}
struct SharedLibraryPage: Decodable {
    var items: [SharedLibraryItem]
    let nextCursor: String?
    let catalogueRevision: Int64
    let scopeGeneration: Int64
    let catalogueGeneration: Int64
    func validate(in library: SharedLibraryIdentity) throws {
        guard items.count <= 200, (nextCursor?.utf8.count ?? 0) <= 4096,
              catalogueRevision >= 0, scopeGeneration >= 0, catalogueGeneration >= 0 else { throw APIError.badURL }
        try items.forEach { try $0.validate(in: library) }
    }
}
struct SharedLibraryFile: Decodable, Identifiable {
    let fileBase: String?
    let fileId: String
    let revision: String
    let reference: SharedPlaybackFileReference
    let size: String
    let durationMs: Int?
    let container: String?
    let videoCodec: String?
    let width: Int?
    let height: Int?
    var id: String { fileId + "|" + revision }
}
struct SharedLibraryWatch: Decodable {
    let positionMs: Int64
    let durationMs: Int64?
    let watched: Bool
    let sequence: Int64
    let updatedAtMs: Int64
}
struct SharedLibraryDetail: Decodable {
    var item: SharedLibraryItem
    let files: [SharedLibraryFile]
    let watch: SharedLibraryWatch?
    let deliveryStatus: String
    let lifecycleGeneration: Int64?
    func validate(expected: SharedPlaybackReference) throws {
        let library = SharedLibraryIdentity(importId: expected.importId, serverId: expected.serverId,
                                           catalogueEpoch: expected.catalogueEpoch, libraryId: expected.libraryId)
        try item.validate(in: library)
        guard item.reference == expected, files.count <= 64, Set(files.map(\.fileId)).count == files.count else { throw APIError.badURL }
        for file in files {
            guard PlaybackFileContext.canonicalID(file.fileId), PlaybackFileContext.canonicalID(file.size),
                  PlaybackFileContext.matches(file.revision, "^[0-9a-f]{64}$"),
                  file.reference.item == expected, file.reference.fileId == file.fileId,
                  file.reference.revision == file.revision else { throw APIError.badURL }
        }
        if let watch {
            guard watch.positionMs >= 0, watch.sequence >= 0, watch.updatedAtMs >= 0,
                  watch.durationMs == nil || watch.durationMs! >= 0 else { throw APIError.badURL }
        }
    }
}

/// Readiness never changes this editor or decides whether its Save is available.
struct SharedSharingDraft {
    private(set) var enabled = false
    private(set) var revision = 0
    mutating func choose(_ value: Bool) { enabled = value; revision += 1 }
    mutating func received(_ value: Bool, requestedAt: Int) {
        if revision == requestedAt { enabled = value }
    }
}

struct SharedBrowseAccumulator {
    private(set) var items: [SharedLibraryItem] = []
    private(set) var nextCursor: String?
    private var seen: Set<String> = []
    mutating func append(_ page: SharedLibraryPage, requestedCursor: String?, library: SharedLibraryIdentity) throws {
        try page.validate(in: library)
        guard requestedCursor == nextCursor,
              page.nextCursor == nil || page.nextCursor != requestedCursor else { throw APIError.badURL }
        for item in page.items where seen.insert(item.id).inserted { items.append(item) }
        nextCursor = page.nextCursor
    }
}


struct SharedContinueGroup: Decodable, Identifiable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let sourceName: String
    let count: Int
    var id: String { [importId, serverId, catalogueEpoch].joined(separator: "|") }
    func validate() throws {
        try SharedLibraryIdentity(importId: importId, serverId: serverId, catalogueEpoch: catalogueEpoch, libraryId: "0").validate()
        guard (0...200).contains(count), sourceName.utf8.count <= 512 else { throw APIError.badURL }
    }
}
struct SharedContinueEntry: Decodable, Identifiable {
    var item: SharedLibraryItem
    let watch: SharedLibraryWatch
    var id: String { item.id }
}
struct SharedContinueItems: Decodable {
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let sourceName: String
    let count: Int
    let availability: String
    var items: [SharedContinueEntry]
    func validate(group: SharedContinueGroup, assigned: [SharedLibraryAssignment]) throws {
        guard [importId, serverId, catalogueEpoch].joined(separator: "|") == group.id,
              (0...200).contains(count), items.count <= count, Set(items.map(\.id)).count == items.count,
              ["online", "unavailable", "busy"].contains(availability), availability == "online" || items.isEmpty else { throw APIError.badURL }
        for entry in items {
            guard let assignment = assigned.first(where: { $0.identity.contains(entry.item.reference) }) else { throw APIError.badURL }
            try entry.item.validate(in: assignment.identity)
            let watch = entry.watch
            guard !watch.watched, (0...9_007_199_254_740_991).contains(watch.positionMs),
                  (0...9_007_199_254_740_991).contains(watch.sequence), watch.updatedAtMs >= 0,
                  watch.durationMs.map({ (0...9_007_199_254_740_991).contains($0) }) ?? true else { throw APIError.badURL }
        }
    }
}
