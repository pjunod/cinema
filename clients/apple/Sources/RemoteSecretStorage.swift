import Foundation
import CryptoKit

/// Remote proofs never fall back to ordinary preferences. Identity-specific
/// Keychain accounts preserve origin/server/user scoping across app updates.
struct RemoteSecretStorage {
    struct Receiver: Codable { let id: UUID; let secret: String }
    struct Grant: Codable { let receiverID: UUID; let id: UUID; let secret: String }
    let identity: String
    var vault: (String) -> any TokenStoring = { TokenVault(service: "tv.plurx.cinema-remote", account: $0) }
    private var receiverVault: any TokenStoring { vault(identity + ":receiver") }
    private var grantVault: any TokenStoring { vault(identity + ":grants") }
    /// Old origin-less proofs cannot be safely attributed to a server. There
    /// is deliberately no migration fallback; those installations re-pair.
    static func scopedIdentity(origin: String, instance: String, userID: Int) -> String? {
        guard let canonical = Session.canonicalOrigin(origin), !canonical.isEmpty, !instance.isEmpty,
              instance.utf8.count <= 128, userID > 0,
              !instance.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }) else { return nil }
        let binding = canonical + "\n" + instance + "\n" + String(userID)
        return "origin-v1:" + SHA256.hash(data: Data(binding.utf8)).map { String(format: "%02x", $0) }.joined()
    }
    static func validSecret(_ value: String) -> Bool {
        guard value.utf8.count == 43, value.utf8.allSatisfy({ (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95 }) else { return false }
        return Data(base64Encoded: value.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/") + "=")?.count == 32
    }
    var receiver: Receiver? {
        guard let value = receiverVault.read()?.data(using: .utf8), let receiver = try? JSONDecoder().decode(Receiver.self, from: value), Self.validSecret(receiver.secret) else { return nil }
        return receiver
    }
    func saveReceiver(_ value: Receiver) throws {
        guard Self.validSecret(value.secret), let json = String(data: try JSONEncoder().encode(value), encoding: .utf8), receiverVault.write(json) else { throw CinemaRemoteOutcome.unavailable }
    }
    var grants: [Grant] {
        guard let data = grantVault.read()?.data(using: .utf8), let values = try? JSONDecoder().decode([Grant].self, from: data) else { return [] }
        return Array(values.filter { Self.validSecret($0.secret) }.prefix(20))
    }
    func saveGrant(_ value: Grant) throws {
        guard Self.validSecret(value.secret) else { throw CinemaRemoteOutcome.invalid }
        let values = grants.filter { $0.receiverID != value.receiverID } + [value]
        guard values.count <= 20, let json = String(data: try JSONEncoder().encode(values), encoding: .utf8), grantVault.write(json) else { throw CinemaRemoteOutcome.unavailable }
    }
    func forgetGrant(_ id: UUID) throws {
        let values = grants.filter { $0.id != id }
        guard let json = String(data: try JSONEncoder().encode(values), encoding: .utf8), grantVault.write(json) else { throw CinemaRemoteOutcome.unavailable }
    }
    func clearReceiver() { receiverVault.clear() }
    func clear() { receiverVault.clear(); grantVault.clear() }
}
