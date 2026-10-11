import Foundation
import CryptoKit

struct InvitationIdentity: Codable, Equatable {
    let origin: String
    let instance: String
    let userID: Int
    var key: String {
        let value = origin + "\n" + instance + "\n" + String(userID)
        return SHA256.hash(data: Data(value.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    var valid: Bool {
        Session.canonicalOrigin(origin) == origin && !origin.isEmpty && userID > 0 &&
        UInt64(userID) <= CinemaRemoteCommand.maximumInteger && !instance.isEmpty && instance.utf8.count <= 128 &&
        !instance.unicodeScalars.contains { $0.properties.generalCategory == .control }
    }
    static func fingerprint(_ token: String) -> String {
        SHA256.hash(data: Data(token.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}
struct InvitationChoice: Codable {
    var name: String
    var enabled: Bool
    var pending: Bool
    var intent = UUID()
}
struct InvitationProfileMetadata: Codable, Identifiable {
    let identity: InvitationIdentity
    var installationID: UUID
    var name: String
    var choices: [String: InvitationChoice] = [:]
    var loggedOut = false
    var pendingDelete = false
    var id: String { identity.key }
}
struct InvitationProofRecord: Codable {
    let identity: InvitationIdentity
    let installationID: UUID
    var phone: InvitationPhone?
    var secret: String?
    var loginFingerprint: String?
    var registrationAttempted = false
    var metadataNeedsRefresh = false
    var consentsNeedRefresh = false
    var consents: [String: InvitationConsent] = [:]
}
enum InvitationLocalError: Error { case capacity, storageUnavailable, invalidStoredState, installationRecovery }

/// Ordinary saved choices remain writable offline; only the phone proof and
/// bound-login digest live in the existing device-only Keychain abstraction.
/// Capacity failures are explicit. Existing profiles/consents are never trimmed.
@MainActor
final class InvitationSecureState {
    static let maximumProfiles = 8
    private let defaults: UserDefaults
    private let vault: (String) -> any TokenStoring
    private let indexKey = "plurx.cinema-invitations.profiles.v1"
    init(defaults: UserDefaults = .standard, vault: @escaping (String) -> any TokenStoring = {
        TokenVault(service: "tv.plurx.cinema-invitations", account: $0)
    }) { self.defaults = defaults; self.vault = vault }
    func profiles() throws -> [InvitationProfileMetadata] {
        guard let bytes = defaults.data(forKey: indexKey) else { return [] }
        guard bytes.count <= 512 * 1_024, let values = try? JSONDecoder().decode([InvitationProfileMetadata].self, from: bytes),
              values.count <= Self.maximumProfiles, Set(values.map(\.id)).count == values.count,
              Set(values.map(\.installationID)).count == values.count,
              values.allSatisfy({ $0.identity.valid && $0.name.utf8.count <= 80 && $0.choices.count <= InvitationWire.maximumConsents &&
                  $0.choices.allSatisfy({ key, value in
                      (try? InvitationWire.canonicalUUID(key)) != nil && value.name.utf8.count <= 80
                  }) }) else { throw InvitationLocalError.invalidStoredState }
        return values
    }
    func profile(_ identity: InvitationIdentity) throws -> InvitationProfileMetadata? { try profiles().first { $0.identity == identity } }
    func profile(installationID: UUID) throws -> InvitationProfileMetadata? { try profiles().first { $0.installationID == installationID } }
    @discardableResult
    func create(_ identity: InvitationIdentity, name: String) throws -> InvitationProfileMetadata {
        guard identity.valid else { throw InvitationLocalError.invalidStoredState }
        if let existing = try profile(identity) { return existing }
        var values = try profiles()
        guard values.count < Self.maximumProfiles else { throw InvitationLocalError.capacity }
        let value = InvitationProfileMetadata(identity: identity, installationID: UUID(), name: RemoteTextBounds.label(name, maximumBytes: 80))
        values.append(value); try saveIndex(values)
        return value
    }
    func saveMetadata(_ profile: InvitationProfileMetadata) throws {
        var values = try profiles()
        guard let index = values.firstIndex(where: { $0.id == profile.id }),
              values[index].installationID == profile.installationID,
              profile.choices.count <= InvitationWire.maximumConsents else { throw InvitationLocalError.invalidStoredState }
        values[index] = profile; try saveIndex(values)
    }
    private func saveIndex(_ values: [InvitationProfileMetadata]) throws {
        guard values.count <= Self.maximumProfiles else { throw InvitationLocalError.capacity }
        let data = try JSONEncoder().encode(values)
        guard data.count <= 512 * 1_024 else { throw InvitationLocalError.capacity }
        defaults.set(data, forKey: indexKey)
    }
    @discardableResult
    func choose(_ enabled: Bool, receiverID: UUID, name: String, profile: InvitationProfileMetadata) throws -> InvitationProfileMetadata {
        guard var current = try self.profile(profile.identity), current.installationID == profile.installationID,
              !current.pendingDelete else { throw InvitationLocalError.invalidStoredState }
        let key = receiverID.uuidString.lowercased()
        guard current.choices[key] != nil || current.choices.count < InvitationWire.maximumConsents else { throw InvitationLocalError.capacity }
        current.choices[key] = InvitationChoice(name: RemoteTextBounds.label(name, maximumBytes: 80), enabled: enabled, pending: true)
        if enabled { current.loggedOut = false }
        try saveMetadata(current); return current
    }
    @discardableResult
    func disableAll(_ profile: InvitationProfileMetadata, logout: Bool = false, removal: Bool = false) throws -> InvitationProfileMetadata {
        guard var current = try self.profile(profile.identity), current.installationID == profile.installationID else { throw InvitationLocalError.invalidStoredState }
        for key in current.choices.keys {
            current.choices[key]?.enabled = false; current.choices[key]?.pending = true; current.choices[key]?.intent = UUID()
        }
        if logout { current.loggedOut = true }
        if removal { current.pendingDelete = true }
        try saveMetadata(current); return current
    }
    func proof(_ profile: InvitationProfileMetadata) throws -> InvitationProofRecord? {
        guard let text = vault(profile.id).read() else { return nil }
        guard let data = text.data(using: .utf8), data.count <= 256 * 1_024,
              let record = try? JSONDecoder().decode(InvitationProofRecord.self, from: data),
              record.identity == profile.identity, record.installationID == profile.installationID,
              record.consents.count <= InvitationWire.maximumConsents,
              record.secret.map({ (try? InvitationWire.secret($0)) != nil }) ?? true,
              record.phone.map({ $0.id == profile.installationID && $0.platform == "apple" && $0.generation > 0 &&
                  $0.generation <= CinemaRemoteCommand.maximumInteger && !$0.residentActive }) ?? true,
              record.consents.allSatisfy({ key, value in key == value.receiverID.uuidString.lowercased() && Self.validConsent(value) }) else {
            throw InvitationLocalError.invalidStoredState
        }
        return record
    }
    private static func validConsent(_ value: InvitationConsent) -> Bool {
        guard value.generation <= CinemaRemoteCommand.maximumInteger, value.transportGeneration <= CinemaRemoteCommand.maximumInteger,
              value.eligible == (value.enabled && value.readiness == .ready),
              !value.enabled || (value.generation > 0 && value.grantID != nil && value.transport != nil) else { return false }
        return value.generation != 0 || (!value.enabled && value.grantID == nil && value.transport == nil && value.transportGeneration == 0 && value.readiness == .disabled)
    }
    func saveProof(_ record: InvitationProofRecord, profile: InvitationProfileMetadata) throws {
        guard record.identity == profile.identity, record.installationID == profile.installationID,
              (try self.profile(profile.identity))?.installationID == record.installationID,
              record.consents.count <= InvitationWire.maximumConsents,
              record.consents.allSatisfy({ $0.key == $0.value.receiverID.uuidString.lowercased() && Self.validConsent($0.value) }),
              record.phone.map({ $0.id == profile.installationID && $0.platform == "apple" && $0.generation > 0 && $0.generation <= CinemaRemoteCommand.maximumInteger && !$0.residentActive }) ?? true,
              record.secret.map({ (try? InvitationWire.secret($0)) != nil }) ?? true else { throw InvitationLocalError.invalidStoredState }
        let data = try JSONEncoder().encode(record)
        guard data.count <= 256 * 1_024, let text = String(data: data, encoding: .utf8), vault(profile.id).write(text) else { throw InvitationLocalError.storageUnavailable }
    }
    /// The response caches remote truth without overwriting a newer local OFF
    /// or replacing a different installation. Its exact intent owns completion.
    func acknowledge(_ consent: InvitationConsent, intent: UUID, profile: InvitationProfileMetadata) throws {
        guard var current = try self.profile(profile.identity), current.installationID == profile.installationID else { return }
        let key = consent.receiverID.uuidString.lowercased()
        guard current.choices[key]?.intent == intent, current.choices[key]?.enabled == consent.enabled else { return }
        current.choices[key]?.pending = false; try saveMetadata(current)
    }
    func mergeConsents(_ consents: [InvitationConsent], record: inout InvitationProofRecord, profile: InvitationProfileMetadata) throws {
        guard consents.count <= InvitationWire.maximumConsents, Set(consents.map(\.receiverID)).count == consents.count,
              var current = try self.profile(profile.identity), current.installationID == profile.installationID else { throw InvitationLocalError.invalidStoredState }
        guard Set(current.choices.keys).union(consents.map { $0.receiverID.uuidString.lowercased() }).count <= InvitationWire.maximumConsents else { throw InvitationLocalError.capacity }
        var merged = record
        merged.consents = Dictionary(uniqueKeysWithValues: consents.map { ($0.receiverID.uuidString.lowercased(), $0) })
        for consent in consents {
            let key = consent.receiverID.uuidString.lowercased()
            if let choice = current.choices[key] {
                if !choice.pending && !current.loggedOut { current.choices[key]?.enabled = consent.enabled }
            } else {
                current.choices[key] = InvitationChoice(name: "Saved screen", enabled: !current.loggedOut && consent.enabled,
                                                        pending: current.loggedOut && consent.enabled)
            }
        }
        guard current.choices.count <= InvitationWire.maximumConsents else { throw InvitationLocalError.capacity }
        try saveProof(merged, profile: current); try saveMetadata(current)
        record = merged
    }
    /// Explicit recovery of unreadable local metadata. Unknown home
    /// installations remain obligations; this is never a remote revocation.
    func resetLocalMetadata(knownIdentity: InvitationIdentity?) {
        if let data = defaults.data(forKey: indexKey), data.count <= 512 * 1024,
           let rows = try? JSONDecoder().decode([InvitationProfileMetadata].self, from: data), rows.count <= Self.maximumProfiles {
            for row in rows where row.identity.valid { vault(row.identity.key).clear() }
        }
        if let knownIdentity, knownIdentity.valid { vault(knownIdentity.key).clear() }
        defaults.removeObject(forKey: indexKey)
    }
    func removeLocal(_ profile: InvitationProfileMetadata) throws {
        guard let current = try self.profile(profile.identity), current.installationID == profile.installationID else { return }
        try saveIndex(profiles().filter { $0.id != profile.id })
        vault(profile.id).clear()
    }
}
