import Foundation
import UserNotifications
#if os(iOS)
import UIKit
#endif

struct InvitationNotificationIdentity: Equatable {
    let identity: InvitationIdentity
    let generation: UInt64
    let fingerprint: String
    static func current() -> Self? {
        let settings = SettingsStore(), auth = Session.shared.playbackAuthorization
        guard let token = auth.token, let instance = settings.instanceId, let userID = settings.userId,
              let origin = Session.canonicalOrigin(auth.origin), origin == Session.canonicalOrigin(settings.origin) else { return nil }
        let identity = InvitationIdentity(origin: origin, instance: instance, userID: userID)
        guard identity.valid else { return nil }
        return .init(identity: identity, generation: auth.generation, fingerprint: InvitationIdentity.fingerprint(token))
    }
}
struct InvitationPendingTap: Codable, Identifiable {
    let invitationID: String
    let installationID: UUID
    let receivedAt: TimeInterval
    var id: String { invitationID }
}
private struct InvitationSeenEvent: Codable {
    let invitationID: String
    let kind: String
    let receivedAt: TimeInterval
}
extension Notification.Name {
    static let plurxInvitationTap = Notification.Name("plurx.invitation.tap")
    static let plurxInvitationToken = Notification.Name("plurx.invitation.token")
    static let plurxInvitationForeground = Notification.Name("plurx.invitation.foreground")
}

/// Single routing bridge called by the existing reminder notification delegate.
/// Opaque payloads are not origins, logins, receiver IDs or control capabilities.
@MainActor
final class InvitationNotificationBridge {
    static let category = "CINEMA_REMOTE_INVITATION"
    static let shared = InvitationNotificationBridge()
    private let store: InvitationSecureState
    private let defaults: UserDefaults
    private let clock: () -> TimeInterval
    private let authorization: () -> InvitationNotificationIdentity?
    private let permission: () async -> Bool
    private let registrationRequest: () -> Void
    private let unregistrationRequest: () -> Void
    private let tapsKey = "plurx.cinema-invitations.pending-taps.v1"
    private let seenKey = "plurx.cinema-invitations.seen.v1"
    var activeIdentity: InvitationIdentity?
    private(set) var deviceToken: String?
    private(set) var tokenGeneration: UInt64 = 0
    private(set) var registrationFailure = false
    // APNs callbacks have no request identifier. This is an authorized lifetime,
    // not callback correlation; the token belongs to this app installation.
    private var registrationOwner: InvitationNotificationIdentity?
    var foregroundPromptPresented: (() -> Void)?
    init(store: InvitationSecureState? = nil, defaults: UserDefaults = .standard,
         clock: @escaping () -> TimeInterval = { Date().timeIntervalSince1970 },
         authorization: @escaping () -> InvitationNotificationIdentity? = { InvitationNotificationIdentity.current() },
         registrationRequest: (() -> Void)? = nil, unregistrationRequest: (() -> Void)? = nil,
         permission: @escaping () async -> Bool = {
             let settings = await UNUserNotificationCenter.current().notificationSettings()
             #if os(iOS)
             return [.authorized, .provisional, .ephemeral].contains(settings.authorizationStatus)
             #else
             return [.authorized, .provisional].contains(settings.authorizationStatus)
             #endif
         }) {
        self.store = store ?? InvitationSecureState(); self.defaults = defaults; self.clock = clock; self.authorization = authorization; self.permission = permission
        self.registrationRequest = registrationRequest ?? {
            #if os(iOS)
            UIApplication.shared.registerForRemoteNotifications()
            #endif
        }
        self.unregistrationRequest = unregistrationRequest ?? {
            #if os(iOS)
            UIApplication.shared.unregisterForRemoteNotifications()
            #endif
        }
    }
    func pendingTaps() -> [InvitationPendingTap] {
        guard let data = defaults.data(forKey: tapsKey), data.count <= 8192,
              let rows = try? JSONDecoder().decode([InvitationPendingTap].self, from: data), rows.count <= 8 else { return [] }
        let now = clock()
        return rows.filter {
            InvitationWire.opaqueID($0.invitationID) == $0.installationID && $0.receivedAt.isFinite &&
            now >= $0.receivedAt && now - $0.receivedAt < 120 &&
            (try? store.profile(installationID: $0.installationID)) != nil
        }
    }
    @discardableResult
    func acceptDefaultTap(category: String, invitationID: String?, defaultAction: Bool) -> Bool {
        guard category == Self.category, defaultAction, let invitationID, let phoneID = InvitationWire.opaqueID(invitationID),
              let profile = try? store.profile(installationID: phoneID), !profile.loggedOut, !profile.pendingDelete,
              profile.choices.values.contains(where: { $0.enabled }), remember(invitationID, kind: "tap") else { return false }
        var rows = pendingTaps().filter { $0.id != invitationID }
        rows.append(.init(invitationID: invitationID, installationID: phoneID, receivedAt: clock()))
        if let data = try? JSONEncoder().encode(Array(rows.suffix(8))) { defaults.set(data, forKey: tapsKey) }
        NotificationCenter.default.post(name: .plurxInvitationTap, object: nil)
        return true
    }
    func retireTap(_ id: String) {
        if let data = try? JSONEncoder().encode(pendingTaps().filter { $0.id != id }) { defaults.set(data, forKey: tapsKey) }
    }
    private struct Eligibility: Equatable {
        let authorization: InvitationNotificationIdentity
        let phone: UUID
        let generation: UInt64
        let consentSignature: String
    }
    private func eligible(_ phoneID: UUID) -> Eligibility? {
        guard let auth = authorization(), activeIdentity == auth.identity,
              let profile = try? store.profile(installationID: phoneID), profile.identity == auth.identity,
              !profile.loggedOut, !profile.pendingDelete, let proof = try? store.proof(profile),
              let phone = proof.phone, proof.secret != nil, phone.permissionGranted,
              proof.loginFingerprint == auth.fingerprint, !proof.metadataNeedsRefresh, !proof.consentsNeedRefresh else { return nil }
        let admitted = proof.consents.values.filter { consent in
            consent.enabled && consent.transport == "apns" && consent.eligible && consent.readiness == .ready &&
            profile.choices[consent.receiverID.uuidString.lowercased()]?.enabled == true
        }.sorted { $0.receiverID.uuidString < $1.receiverID.uuidString }
        guard !admitted.isEmpty else { return nil }
        let signature = admitted.map { $0.receiverID.uuidString + ":" + String($0.generation) + ":" + String($0.transportGeneration) }.joined(separator: "|")
        return .init(authorization: auth, phone: phoneID, generation: phone.generation, consentSignature: signature)
    }
    func shouldPresent(category: String, invitationID: String?) async -> Bool {
        guard category == Self.category, let invitationID, let phoneID = InvitationWire.opaqueID(invitationID),
              let snapshot = eligible(phoneID), await permission(), snapshot == eligible(phoneID),
              remember(invitationID, kind: "visible") else { return false }
        foregroundPromptPresented?()
        NotificationCenter.default.post(name: .plurxInvitationForeground, object: nil)
        return true
    }
    private func remember(_ id: String, kind: String) -> Bool {
        let now = clock()
        guard now.isFinite else { return false }
        var rows: [InvitationSeenEvent] = []
        if let data = defaults.data(forKey: seenKey) {
            guard data.count <= 512 * 1024, let decoded = try? JSONDecoder().decode([InvitationSeenEvent].self, from: data), decoded.count <= 4096 else { return false }
            guard decoded.allSatisfy({ $0.receivedAt.isFinite }) else { return false }
            // Clock rollback retains uncertainty; it must not permit redisplay.
            rows = decoded.filter { now < $0.receivedAt || now - $0.receivedAt < 180 }
        }
        guard !rows.contains(where: { $0.invitationID == id && $0.kind == kind }), rows.count < 4096 else { return false }
        rows.append(.init(invitationID: id, kind: kind, receivedAt: now))
        guard let data = try? JSONEncoder().encode(rows), data.count <= 512 * 1024 else { return false }
        defaults.set(data, forKey: seenKey); return true
    }
    private func registrationPermitted(_ owner: InvitationNotificationIdentity) -> Bool {
        guard owner == authorization(), activeIdentity == owner.identity,
              let profile = try? store.profile(owner.identity), !profile.loggedOut, !profile.pendingDelete,
              profile.choices.values.contains(where: { $0.enabled }),
              let proof = try? store.proof(profile), proof.phone != nil, proof.secret != nil,
              proof.loginFingerprint == owner.fingerprint else { return false }
        return true
    }
    func requestRegistration(owner: InvitationNotificationIdentity, force: Bool = false) {
        guard registrationPermitted(owner) else { return }
        if !force, registrationOwner == owner { return }
        registrationOwner = owner; registrationFailure = false
        registrationRequest()
    }
    func registered(_ token: Data) {
        guard let owner = registrationOwner, registrationPermitted(owner) else { return }
        registrationFailure = false
        guard !token.isEmpty, token.count <= 256 else { registrationFailure = true; return }
        let value = token.map { String(format: "%02x", $0) }.joined()
        if value != deviceToken {
            deviceToken = value; tokenGeneration += 1
            NotificationCenter.default.post(name: .plurxInvitationToken, object: nil)
        }
    }
    func registrationFailed() {
        guard let owner = registrationOwner, registrationPermitted(owner) else { return }
        registrationFailure = true
        NotificationCenter.default.post(name: .plurxInvitationToken, object: nil)
    }
    func retireRegistration(unregister: Bool) {
        registrationOwner = nil; deviceToken = nil; registrationFailure = false
        if unregister { unregistrationRequest() }
    }
}
