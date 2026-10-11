import Foundation
import Darwin
import Combine

enum SharedSharingManagementError: Error, LocalizedError {
    case invalid(String)
    var errorDescription: String? { if case .invalid(let message) = self { return message }; return nil }
}

/// Admin metadata never substitutes for a viewer's authenticated Shared reference.
struct SharedSharingEndpoint: Codable, Equatable {
    let ipv4: String
    let ipv6: String?
    let tsFqdn: String
    let port: Int
    let spkiSha256: String
    func validate() throws {
        let parts = ipv4.split(separator: ".", omittingEmptySubsequences: false)
        let bytes = parts.compactMap { Int($0) }
        guard bytes.count == 4, parts.count == 4, zip(parts, bytes).allSatisfy({ String($0.1) == $0.0 && (0...255).contains($0.1) }),
              bytes[0] == 100, (64...127).contains(bytes[1]), (1...65535).contains(port),
              PlaybackFileContext.matches(spkiSha256, "^[0-9a-f]{64}$"), tsFqdn.hasSuffix(".ts.net") else { throw APIError.badURL }
        let prefix = String(tsFqdn.dropLast(7)); let labels = prefix.split(separator: ".", omittingEmptySubsequences: false)
        guard prefix.utf8.count <= 240, labels.count >= 2,
              labels.allSatisfy({ PlaybackFileContext.matches(String($0), "^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$") }) else { throw APIError.badURL }
        if let ipv6 {
            var parsed = in6_addr()
            guard ipv6.withCString({ inet_pton(AF_INET6, $0, &parsed) }) == 1 else { throw APIError.badURL }
            let raw = withUnsafeBytes(of: parsed) { Array($0) }
            guard Array(raw.prefix(6)) == [0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0] else { throw APIError.badURL }
        }
    }
}
struct SharedSharingEndpointFields: Identifiable {
    let id = UUID()
    var ipv4 = ""
    var ipv6 = ""
    var tsFqdn = ""
    var port = "8443"
    var pin = ""
    init() {}
    init(_ endpoint: SharedSharingEndpoint) {
        ipv4 = endpoint.ipv4; ipv6 = endpoint.ipv6 ?? ""; tsFqdn = endpoint.tsFqdn
        port = String(endpoint.port); pin = endpoint.spkiSha256
    }
    func validated() throws -> SharedSharingEndpoint {
        guard let number = Int(port), String(number) == port else { throw SharedSharingManagementError.invalid("Enter a canonical port from 1 to 65535.") }
        let endpoint = SharedSharingEndpoint(ipv4: ipv4, ipv6: ipv6.isEmpty ? nil : ipv6, tsFqdn: tsFqdn, port: number, spkiSha256: pin)
        try endpoint.validate(); return endpoint
    }
}
struct SharedSharingEndpointManifest: Decodable { let revision: Int64; let endpoints: [SharedSharingEndpoint] }
struct SharedSharingExportGrant: Decodable {
    let id: String
    let recipientServerId: String
    let state: String
    let scopeGeneration: Int64
    let credentialGeneration: Int64
    let catalogueGeneration: Int64
    let mutationGeneration: Int64
    let pendingExpiresAtMs: Int64
}
struct SharedSharingExport: Decodable, Identifiable {
    let grant: SharedSharingExportGrant
    let recipientName: String
    let invitationId: String
    let claimId: String
    let libraryIds: [String]
    let pairingCode: String
    var id: String { grant.id }
    func validate() throws {
        try SharedSharingValidation.ids(libraryIds)
        guard [id, grant.recipientServerId, invitationId, claimId].allSatisfy(PlaybackFileContext.canonicalUUID),
              [grant.scopeGeneration, grant.credentialGeneration, grant.catalogueGeneration, grant.mutationGeneration].allSatisfy({ $0 >= 1 }),
              ["pending", "active", "disabled", "revoked"].contains(grant.state),
              SharedSharingValidation.code(pairingCode) else { throw APIError.badURL }
    }
}
struct SharedSharingImportSummary: Decodable {
    let id: String
    let sourceServerId: String
    let catalogueEpoch: String
    let sourceName: String
    let claimId: String
    let remoteGrantId: String?
    let state: String
    let assignmentGeneration: Int64
    let lifecycleGeneration: Int64
    let endpointGeneration: Int64
    let observedEndpointRevision: Int64?
    let endpoints: [SharedSharingEndpoint]
}
struct SharedSharingImport: Decodable, Identifiable {
    let `import`: SharedSharingImportSummary
    let pairingCode: String
    var id: String { `import`.id }
    func validate() throws {
        let row = `import`
        try row.endpoints.forEach { try $0.validate() }
        guard [row.id, row.sourceServerId, row.catalogueEpoch, row.claimId].allSatisfy(PlaybackFileContext.canonicalUUID),
              row.remoteGrantId.map(PlaybackFileContext.canonicalUUID) ?? true,
              (row.state != "active" || row.remoteGrantId != nil),
              [row.assignmentGeneration, row.lifecycleGeneration, row.endpointGeneration].allSatisfy({ $0 >= 1 }),
              (1...4).contains(row.endpoints.count), SharedSharingValidation.code(pairingCode) else { throw APIError.badURL }
    }
}
struct SharedSharingInvitation: Decodable {
    let id: String
    let invitation: String
    let expiresAtMs: Int64
}
struct SharedSharingLocalLibrary: Decodable, Identifiable {
    let id: Int64
    let name: String
    let kind: String
    var sourceId: String { String(id) }
}
struct SharedSharingViewer: Decodable, Identifiable {
    let id: Int64
    let username: String
    let isAdmin: Bool
}
struct SharedSharingAssignmentGroup: Codable, Equatable {
    let libraryId: String
    let userIds: [Int64]
}
struct SharedSharingAssignmentSnapshot: Decodable {
    let state: String
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let lifecycleGeneration: Int64
    let expectedAssignmentGeneration: Int64
    let assignments: [SharedSharingAssignmentGroup]
    func validate(for row: SharedSharingImportSummary) throws {
        guard importId == row.id, serverId == row.sourceServerId, catalogueEpoch == row.catalogueEpoch,
              lifecycleGeneration == row.lifecycleGeneration, expectedAssignmentGeneration == row.assignmentGeneration, state == row.state else { throw APIError.badURL }
        try SharedSharingValidation.assignments(assignments)
    }
}
/// Complete admin Source scope; an empty list is valid only after a successful bound read.
struct SharedSharingSourceLibrary: Decodable, Identifiable {
    let libraryId: String
    let name: String
    let kind: String
    let anime: Bool
    var id: String { libraryId }
}
struct SharedSharingSourceLibrariesSnapshot: Decodable {
    let state: String
    let importId: String
    let serverId: String
    let catalogueEpoch: String
    let lifecycleGeneration: Int64
    let expectedAssignmentGeneration: Int64
    let libraries: [SharedSharingSourceLibrary]
    func validate(for row: SharedSharingImportSummary) throws {
        guard state == "active", state == row.state, importId == row.id,
              serverId == row.sourceServerId, catalogueEpoch == row.catalogueEpoch,
              lifecycleGeneration == row.lifecycleGeneration,
              expectedAssignmentGeneration == row.assignmentGeneration else { throw APIError.badURL }
        try SharedSharingValidation.ids(libraries.map(\.libraryId))
        guard libraries.allSatisfy({ ["movies", "shows"].contains($0.kind) && $0.name.utf8.count <= 256 }) else { throw APIError.badURL }
    }
}

/// A whole-replacement draft retains rows and viewers omitted by today's Source/user lists.
struct SharedSharingAssignmentMatrix {
    struct Library: Identifiable { let id: String; let name: String; let outsideScope: Bool }
    let snapshot: SharedSharingAssignmentSnapshot
    let libraries: [Library]
    let viewers: [SharedSharingViewer]
    private(set) var groups: [SharedSharingAssignmentGroup]
    private(set) var revision: UInt64 = 0
    init(row: SharedSharingImportSummary, assignments: SharedSharingAssignmentSnapshot,
         scope: SharedSharingSourceLibrariesSnapshot, viewers currentViewers: [SharedSharingViewer]) throws {
        try assignments.validate(for: row); try scope.validate(for: row)
        guard currentViewers.count <= 4096, Set(currentViewers.map(\.id)).count == currentViewers.count,
              currentViewers.allSatisfy({ $0.id >= 0 }) else { throw APIError.badURL }
        snapshot = assignments; groups = assignments.assignments
        let currentIds = Set(scope.libraries.map(\.libraryId))
        libraries = scope.libraries.map { Library(id: $0.libraryId, name: $0.name, outsideScope: false) }
            + assignments.assignments.filter { !currentIds.contains($0.libraryId) }.map { Library(id: $0.libraryId, name: "Outside current Source scope · " + $0.libraryId, outsideScope: true) }
        let viewerIds = Set(currentViewers.map(\.id))
        let omitted = Set(assignments.assignments.flatMap(\.userIds)).subtracting(viewerIds).sorted()
        viewers = currentViewers + omitted.map { SharedSharingViewer(id: $0, username: "Unavailable viewer · " + String($0), isAdmin: false) }
    }
    func contains(library: String, viewer: Int64) -> Bool { groups.first { $0.libraryId == library }?.userIds.contains(viewer) ?? false }
    mutating func set(library: String, viewer: Int64, enabled: Bool) throws {
        guard libraries.contains(where: { $0.id == library }), viewers.contains(where: { $0.id == viewer }) else { throw APIError.badURL }
        let index = groups.firstIndex { $0.libraryId == library }
        var ids = Set(index.map { groups[$0].userIds } ?? [])
        if enabled { ids.insert(viewer) } else { ids.remove(viewer) }
        var replacement = groups
        let group = SharedSharingAssignmentGroup(libraryId: library, userIds: ids.sorted())
        if let index { replacement[index] = group } else { replacement.append(group) }
        try SharedSharingValidation.assignments(replacement); groups = replacement; revision += 1
    }
    mutating func removeOutsideScope(_ library: String) throws {
        guard libraries.contains(where: { $0.id == library && $0.outsideScope }) else { throw APIError.badURL }
        groups.removeAll { $0.libraryId == library }; revision += 1
    }
    func accepts(_ requestedRevision: UInt64) -> Bool { revision == requestedRevision }
}

enum SharedSharingValidation {
    static func ids(_ values: [String]) throws {
        guard values.count <= 64, Set(values).count == values.count, values.allSatisfy(PlaybackFileContext.canonicalID) else { throw APIError.badURL }
    }
    static func code(_ value: String) -> Bool { PlaybackFileContext.matches(value, "^[0-9a-f]{16}$") }
    static func assignments(_ values: [SharedSharingAssignmentGroup]) throws {
        try ids(values.map(\.libraryId))
        guard values.allSatisfy({ $0.userIds.count <= 256 && Set($0.userIds).count == $0.userIds.count && $0.userIds.allSatisfy({ $0 >= 0 }) }) else { throw APIError.badURL }
    }
}

/// Secrets live only in this account-bound draft, never defaults or navigation arguments.
final class SharedSharingSecretDraft: ObservableObject {
    private final class WeakDraft { weak var value: SharedSharingSecretDraft?; init(_ value: SharedSharingSecretDraft) { self.value = value } }
    private static let registryLock = NSLock()
    private static var registry: [UUID: WeakDraft] = [:]
    private let draftId = UUID()
    /// A refused current management authorization clears transient material without changing Session semantics.
    static func retireAuthorization(generation: UInt64) {
        registryLock.lock(); let drafts = registry.values.compactMap(\.value); registryLock.unlock()
        drafts.filter { $0.authorization.generation == generation }.forEach { $0.retire() }
    }
    private func removeDraftRegistration() {
        Self.registryLock.lock(); Self.registry.removeValue(forKey: draftId); Self.registryLock.unlock()
    }
    let objectWillChange = ObservableObjectPublisher()
    struct Snapshot { let revision: UInt64; let invitation: String; let pairingCode: String }
    private let lock = NSLock()
    private let authorization = Session.shared.playbackAuthorization
    private var observation: UUID?
    private var active = true
    private var revision: UInt64 = 0
    private var invitationValue = ""
    private var codeValue = ""
    init() { installObserver() }
    #if DEBUG
    init(beforeObserver: () -> Void) { beforeObserver(); installObserver() }
    #endif
    private func installObserver() {
        let registration = Session.shared.observeAuthorizationChanges { [weak self] _ in self?.retire() }
        observation = registration.id
        Self.registryLock.lock(); Self.registry[draftId] = WeakDraft(self); Self.registryLock.unlock()
        if registration.generation != authorization.generation || !current() { retire() }
    }
    deinit { removeDraftRegistration(); if let observation { Session.shared.removeAuthorizationObserver(observation) } }
    private func current() -> Bool {
        let now = Session.shared.playbackAuthorization
        return now.generation == authorization.generation && now.origin == authorization.origin && now.token == authorization.token && now.token != nil
    }
    private func notifyPresentation() {
        if Thread.isMainThread { objectWillChange.send() }
        else { DispatchQueue.main.async { [weak self] in self?.objectWillChange.send() } }
    }
    func edit(invitation: String? = nil, pairingCode: String? = nil) {
        lock.lock(); defer { lock.unlock(); notifyPresentation() }
        guard active, current() else { clearLocked(); return }
        if let invitation { invitationValue = invitation }
        if let pairingCode { codeValue = pairingCode }
        revision += 1
    }
    func snapshot() -> Snapshot? {
        lock.lock(); defer { lock.unlock() }
        guard active, current() else { clearLocked(); return nil }
        return Snapshot(revision: revision, invitation: invitationValue, pairingCode: codeValue)
    }
    func accepts(_ requestedRevision: UInt64) -> Bool {
        lock.lock(); defer { lock.unlock() }
        guard active, current() else { clearLocked(); return false }
        return revision == requestedRevision
    }
    func clear(ifRevision requestedRevision: UInt64) {
        lock.lock(); defer { lock.unlock(); notifyPresentation() }
        if active, current(), revision == requestedRevision { invitationValue = ""; codeValue = ""; revision += 1 }
    }
    private func clearLocked() { active = false; invitationValue = ""; codeValue = ""; revision += 1 }
    func retire() {
        lock.lock(); clearLocked(); lock.unlock()
        // Authority is already invalidated; presentation updates run on the UI queue.
        DispatchQueue.main.async { [weak self] in self?.objectWillChange.send() }
    }
    func leave() {
        retire(); removeDraftRegistration()
        if let observation { Session.shared.removeAuthorizationObserver(observation); self.observation = nil }
    }
}
