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
        if registration.generation != authorization.generation || !current() { retire() }
    }
    deinit { if let observation { Session.shared.removeAuthorizationObserver(observation) } }
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
        retire()
        if let observation { Session.shared.removeAuthorizationObserver(observation); self.observation = nil }
    }
}
