import Foundation
import SwiftUI
import UserNotifications
#if os(iOS)
import UIKit
#endif

struct InvitationTapHandoff: Equatable {
    let lookup: InvitationLookup
    let owner: InvitationNotificationIdentity
    let installationID: UUID
    let choiceIntent: UUID
    let deadline: TimeInterval
}

/// The foreground model serializes home CAS updates. Saved choices are local
/// intent; an OS token or readiness response cannot turn a choice on.
@MainActor
final class InvitationClientModel: ObservableObject {
    @Published private(set) var authorizationGeneration: UInt64 = 0
    @Published private(set) var profile: InvitationProfileMetadata?
    @Published private(set) var record: InvitationProofRecord?
    @Published private(set) var homePhones: [InvitationPhone] = []
    @Published private(set) var localStoreUnreadable = false
    @Published private(set) var profiles: [InvitationProfileMetadata] = []
    @Published private(set) var status = "Invitations are a separate saved choice for each paired screen."
    @Published private(set) var busy = false
    @Published private(set) var requiresRebind = false
    @Published private(set) var requiresReset = false
    @Published private(set) var pendingTapNotice: String?
    @Published private(set) var tapReady: InvitationLookup?
    private let store: InvitationSecureState
    private let bridge: InvitationNotificationBridge
    private let authorization: () -> InvitationNotificationIdentity?
    private let permission: () async -> Bool
    private let requestPermission: () async -> Bool
    private let grantLookup: (InvitationIdentity, UUID) -> RemoteSecretStorage.Grant?
    private let makeAPI: (InvitationNotificationIdentity) -> HomeInvitationAPI?
    private let broker: InvitationBrokerClaim
    private let monotonic: () -> TimeInterval
    private var authorizationObserver: UUID?
    private var tapHandoff: InvitationTapHandoff?
    private var auth: InvitationNotificationIdentity?
    private var homePhonesOwner: InvitationNotificationIdentity?
    private var active = false
    private var foregroundRemoteEnabled = false
    private var generation: UInt64 = 0
    private var task: Task<Void, Never>?
    private var attemptedTransport: [UUID: String] = [:]
    private var pendingTokenRefresh = false
    private var notificationTokens: [NSObjectProtocol] = []
    init(store: InvitationSecureState? = nil, bridge: InvitationNotificationBridge? = nil,
         authorization: @escaping () -> InvitationNotificationIdentity? = { .current() },
         permission: @escaping () async -> Bool = {
             #if os(iOS)
             return [.authorized, .provisional, .ephemeral].contains(await UNUserNotificationCenter.current().notificationSettings().authorizationStatus)
             #else
             return false
             #endif
         }, requestPermission: @escaping () async -> Bool = {
             #if os(iOS)
             return await LocalReminders.shared.requestAuthorization()
             #else
             return false
             #endif
         }, grantLookup: @escaping (InvitationIdentity, UUID) -> RemoteSecretStorage.Grant? = { identity, receiver in
             guard let key = RemoteSecretStorage.scopedIdentity(origin: identity.origin, instance: identity.instance, userID: identity.userID) else { return nil }
             return RemoteSecretStorage(identity: key).grants.first { $0.receiverID == receiver }
         }, makeAPI: @escaping (InvitationNotificationIdentity) -> HomeInvitationAPI? = { owner in
             let current = Session.shared.playbackAuthorization
             guard current.generation == owner.generation, let token = current.token else { return nil }
             return HomeInvitationAPI(origin: owner.identity.origin, token: token, authorizationGeneration: owner.generation)
         }, broker: InvitationBrokerClaim = InvitationBrokerClaim(), monotonic: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.store = store ?? InvitationSecureState(); self.bridge = bridge ?? .shared
        self.authorization = authorization; self.permission = permission; self.requestPermission = requestPermission
        self.grantLookup = grantLookup; self.makeAPI = makeAPI; self.broker = broker; self.monotonic = monotonic
        notificationTokens.append(NotificationCenter.default.addObserver(forName: .plurxInvitationToken, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.tokenChanged() }
        })
        notificationTokens.append(NotificationCenter.default.addObserver(forName: .plurxInvitationTap, object: nil, queue: .main) { [weak self] _ in
            Task { @MainActor in self?.processPendingTap() }
        })
        authorizationObserver = Session.shared.observeAuthorizationChanges { [weak self] _ in
            Task { @MainActor in
                guard let self else { return }
                self.configure(active: self.active, remoteEnabled: self.foregroundRemoteEnabled)
            }
        }.id
        reload()
    }
    deinit { if let authorizationObserver { Session.shared.removeAuthorizationObserver(authorizationObserver) }; task?.cancel(); for token in notificationTokens { NotificationCenter.default.removeObserver(token) } }
    private func tokenChanged() {
        if bridge.registrationFailure {
            status = "Choice saved. APNs registration is unavailable; check signing and provisioning, then retry delivery setup explicitly."
            return
        }
        if task != nil { pendingTokenRefresh = true } else { refresh() }
    }
    func configure(active: Bool, remoteEnabled: Bool) {
        let next = authorization()
        authorizationGeneration = next?.generation ?? Session.shared.playbackAuthorization.generation
        let changedIdentity = next != auth
        if changedIdentity {
            retire()
            if let old = auth, old.identity != next?.identity || next == nil,
               let oldProfile = try? store.profile(old.identity) {
                _ = try? store.disableAll(oldProfile, logout: true)
            }
            auth = next; bridge.activeIdentity = next?.identity; bridge.retireRegistration(unregister: next == nil)
            attemptedTransport.removeAll(); tapReady = nil; tapHandoff = nil; homePhones = []; homePhonesOwner = nil
        }
        let becameActive = !self.active && active
        self.active = active; foregroundRemoteEnabled = remoteEnabled
        if !remoteEnabled { tapReady = nil; tapHandoff = nil }
        if !active { retire(); tapReady = nil; tapHandoff = nil }
        reload()
        if becameActive || (active && changedIdentity) { refresh() }
        if active { processPendingTap() }
    }
    private func completeTask(_ ticket: UInt64) {
        guard ticket == generation else { return }
        task = nil; busy = false; reload()
        if pendingTokenRefresh { pendingTokenRefresh = false; refresh() }
        else { processPendingTap() }
    }
    private func retire() { generation += 1; task?.cancel(); task = nil; busy = false }
    private func current(_ ticket: UInt64, _ owner: InvitationNotificationIdentity, _ installation: UUID) -> Bool {
        active && !Task.isCancelled && ticket == generation && auth == owner && authorization() == owner &&
        (try? store.profile(owner.identity))?.installationID == installation
    }
    private func reload() {
        do {
            profiles = try store.profiles(); localStoreUnreadable = false
            profile = try auth.flatMap { try store.profile($0.identity) }
            record = try profile.flatMap { try store.proof($0) }
            requiresReset = profile != nil && record?.registrationAttempted == true && record?.secret == nil
            requiresRebind = record?.loginFingerprint != nil && record?.loginFingerprint != auth?.fingerprint
        } catch {
            localStoreUnreadable = true; profile = nil; record = nil; profiles = [];
            if homePhonesOwner != auth || auth != authorization() { homePhones = []; homePhonesOwner = nil }
            requiresReset = false; requiresRebind = false; tapReady = nil
            status = "Saved invitation settings could not be read. Current invitation authority is unavailable."
        }
    }
    func saveChoice(_ enabled: Bool, receiver: UUID, name: String) {
        guard let owner = auth, owner == authorization() else { status = "Sign in to save invitations."; return }
        do {
            let existing = try store.profile(owner.identity) ?? store.create(owner.identity, name: Self.phoneName)
            _ = try store.choose(enabled, receiverID: receiver, name: name, profile: existing)
            if !enabled, (try store.profile(owner.identity))?.choices.values.contains(where: { $0.enabled }) != true { bridge.retireRegistration(unregister: true) }
            reload(); retire()
            launch(requestPermission: enabled)
        } catch { status = "Saved invitation capacity is full or unavailable. Remove a saved profile to recover." }
    }
    func refresh() { guard task == nil else { return }; launch(requestPermission: false) }
    func retryTransport(receiver: UUID) {
        if let auth { bridge.requestRegistration(owner: auth, force: true) }
        attemptedTransport.removeValue(forKey: receiver)
        retire(); launch(requestPermission: false)
    }
    private func launch(requestPermission shouldRequest: Bool) {
        guard active, let owner = auth, owner == authorization(), let profile, let api = makeAPI(owner) else { return }
        generation += 1; let ticket = generation; busy = true
        task = Task { [weak self] in
            guard let self else { return }
            defer { self.completeTask(ticket) }
            do {
                guard self.current(ticket, owner, profile.installationID) else { throw CancellationError() }
                try await self.reconcile(owner: owner, installation: profile.installationID, ticket: ticket, api: api, requestPermission: shouldRequest)
            }
            catch is CancellationError { }
            catch { if self.current(ticket, owner, profile.installationID) { self.status = Self.message(error); self.reload() } }
        }
    }
    private func reconcile(owner: InvitationNotificationIdentity, installation: UUID, ticket: UInt64,
                           api: HomeInvitationAPI, requestPermission shouldRequest: Bool) async throws {
        guard var profile = try store.profile(owner.identity), profile.installationID == installation else { throw CancellationError() }
        if profile.pendingDelete {
            try await api.remove(id: installation)
            guard current(ticket, owner, installation) else { throw CancellationError() }
            try store.removeLocal(profile); homePhones.removeAll { $0.id == installation }; bridge.retireRegistration(unregister: true); status = "Installation removed."; return
        }
        var proof = try store.proof(profile) ?? InvitationProofRecord(identity: owner.identity, installationID: installation)
        let hasOn = profile.choices.values.contains { $0.enabled }
        if proof.secret == nil && !hasOn { status = "Invitations are off on this phone."; return }
        if proof.secret == nil && proof.registrationAttempted {
            status = "The installation proof was not received. Remove this installation and register a new one."; return
        }
        proof = try await synchronizePhone(proof, profile: profile, owner: owner, ticket: ticket, api: api)
        guard current(ticket, owner, installation), let secret = proof.secret, var phone = proof.phone else { throw CancellationError() }
        // OFF needs same-human authentication and phone proof, not the old
        // login binding, permission, a live grant, or provider readiness.
        proof = try await reconcileOff(proof, profile: profile, owner: owner, ticket: ticket, api: api)
        if proof.loginFingerprint != owner.fingerprint {
            status = "OFF choices synchronized. Rebind explicitly before enabling invitations with this sign-in."; return
        }
        if shouldRequest && hasOn { _ = await requestPermission() }
        guard current(ticket, owner, installation) else { throw CancellationError() }
        let allowed = await permission()
        guard current(ticket, owner, installation) else { throw CancellationError() }
        if phone.permissionGranted != allowed {
            proof.metadataNeedsRefresh = true; try store.saveProof(proof, profile: profile)
            phone = try await api.availability(phone: phone, secret: secret, permission: allowed)
            guard current(ticket, owner, installation) else { throw CancellationError() }
            proof.phone = phone; proof.metadataNeedsRefresh = false; proof.consentsNeedRefresh = true
            try store.saveProof(proof, profile: profile)
        }
        let remoteConsents = try await api.consents(phone: phone, secret: secret)
        guard current(ticket, owner, installation) else { throw CancellationError() }
        try store.mergeConsents(remoteConsents, record: &proof, profile: profile)
        proof.consentsNeedRefresh = false; try store.saveProof(proof, profile: profile)
        profile = try store.profile(owner.identity) ?? profile
        for (key, choice) in profile.choices.sorted(by: { $0.key < $1.key }) where choice.pending && choice.enabled {
            guard let receiver = UUID(uuidString: key), current(ticket, owner, installation),
                  let latest = try store.profile(owner.identity), latest.choices[key]?.intent == choice.intent else { throw CancellationError() }
            let grant = choice.enabled ? grantLookup(owner.identity, receiver) : nil
            proof.consentsNeedRefresh = true; try store.saveProof(proof, profile: latest)
            let consent = try await api.consent(phone: phone, secret: secret, receiver: receiver,
                generation: proof.consents[key]?.generation ?? 0, enabled: choice.enabled, grant: grant)
            guard current(ticket, owner, installation) else { throw CancellationError() }
            try cache(consent, proof: &proof, profile: latest)
            try store.acknowledge(consent, intent: choice.intent, profile: latest)
        }
        guard current(ticket, owner, installation), let latest = try store.profile(owner.identity) else { throw CancellationError() }
        if latest.loggedOut || latest.pendingDelete { return }
        if allowed && latest.choices.values.contains(where: { $0.enabled }) { bridge.requestRegistration(owner: owner) }
        for consent in proof.consents.values.sorted(by: { $0.receiverID.uuidString < $1.receiverID.uuidString }) where consent.enabled && consent.transport == "apns" {
            let key = consent.receiverID.uuidString.lowercased()
            guard let choice = latest.choices[key], choice.enabled, !choice.pending,
                  let grant = grantLookup(owner.identity, consent.receiverID), grant.id == consent.grantID,
                  allowed, let token = bridge.deviceToken else { continue }
            let attempt = installation.uuidString + "/" + consent.receiverID.uuidString + "/" + String(phone.generation) + "/" + String(bridge.tokenGeneration) + "/" + grant.id.uuidString + "/" + choice.intent.uuidString
            guard attemptedTransport[consent.receiverID] != attempt else { continue }
            guard attemptedTransport[consent.receiverID] != nil || attemptedTransport.count < InvitationWire.maximumConsents else { throw InvitationLocalError.capacity }
            attemptedTransport[consent.receiverID] = attempt
            try await enroll(consent, choice: choice, phone: phone, secret: secret, token: token, proof: &proof,
                             profile: latest, owner: owner, ticket: ticket, api: api, grant: grant)
        }
        status = bridge.registrationFailure ? "Choice saved. APNs registration is unavailable; check signing and provisioning, then retry delivery setup explicitly." :
            allowed ? "Saved choices synchronized. Delivery readiness is shown for each screen." : "Choice saved. Allow notifications in iOS Settings to receive invitations."
    }
    private func reconcileOff(_ original: InvitationProofRecord, profile: InvitationProfileMetadata,
                              owner: InvitationNotificationIdentity, ticket: UInt64, api: HomeInvitationAPI) async throws -> InvitationProofRecord {
        var proof = original
        guard let phone = proof.phone, let secret = proof.secret else { return proof }
        let rows = try await api.consents(phone: phone, secret: secret)
        guard current(ticket, owner, profile.installationID) else { throw CancellationError() }
        try store.mergeConsents(rows, record: &proof, profile: profile)
        proof.consentsNeedRefresh = false; try store.saveProof(proof, profile: profile)
        guard let latest = try store.profile(owner.identity) else { throw CancellationError() }
        for (key, choice) in latest.choices.sorted(by: { $0.key < $1.key }) where choice.pending && !choice.enabled {
            guard let receiver = UUID(uuidString: key), current(ticket, owner, profile.installationID),
                  (try store.profile(owner.identity))?.choices[key]?.intent == choice.intent else { throw CancellationError() }
            proof.consentsNeedRefresh = true; try store.saveProof(proof, profile: latest)
            let off = try await api.consent(phone: phone, secret: secret, receiver: receiver,
                generation: proof.consents[key]?.generation ?? 0, enabled: false, grant: nil)
            guard current(ticket, owner, profile.installationID) else { throw CancellationError() }
            try cache(off, proof: &proof, profile: latest)
            try store.acknowledge(off, intent: choice.intent, profile: latest)
        }
        return proof
    }
    private func synchronizePhone(_ original: InvitationProofRecord, profile: InvitationProfileMetadata,
                                  owner: InvitationNotificationIdentity, ticket: UInt64, api: HomeInvitationAPI) async throws -> InvitationProofRecord {
        var proof = original
        // Persist the one-time registration boundary before sending. A lost reply
        // is explicit recovery, never an unproved automatic registration loop.
        proof.registrationAttempted = true; proof.metadataNeedsRefresh = true; try store.saveProof(proof, profile: profile)
        let (phone, issuedSecret) = try await api.register(id: profile.installationID, name: profile.name, secret: proof.secret)
        guard current(ticket, owner, profile.installationID) else { throw CancellationError() }
        if proof.secret == nil { proof.secret = issuedSecret; proof.loginFingerprint = owner.fingerprint }
        proof.phone = phone; proof.metadataNeedsRefresh = false
        try store.saveProof(proof, profile: profile); return proof
    }
    private func cache(_ consent: InvitationConsent, proof: inout InvitationProofRecord, profile: InvitationProfileMetadata) throws {
        proof.consents[consent.receiverID.uuidString.lowercased()] = consent
        proof.consentsNeedRefresh = false; try store.saveProof(proof, profile: profile)
    }
    private func enroll(_ consent: InvitationConsent, choice: InvitationChoice, phone: InvitationPhone, secret: String,
                        token: String, proof: inout InvitationProofRecord, profile: InvitationProfileMetadata,
                        owner: InvitationNotificationIdentity, ticket: UInt64, api: HomeInvitationAPI, grant: RemoteSecretStorage.Grant) async throws {
        let started = monotonic(), deadline = started + 120
        let tokenGeneration = bridge.tokenGeneration
        func admitted() -> Bool {
            let now = monotonic()
            guard now.isFinite, now >= started, now < deadline,
                  current(ticket, owner, profile.installationID), bridge.tokenGeneration == tokenGeneration, bridge.deviceToken == token,
                  let latest = try? store.profile(owner.identity), !latest.loggedOut, !latest.pendingDelete,
                  let saved = latest.choices[consent.receiverID.uuidString.lowercased()], saved.enabled, saved.intent == choice.intent,
                  grantLookup(owner.identity, consent.receiverID)?.id == grant.id else { return false }
            return true
        }
        guard admitted() else { throw CancellationError() }
        proof.consentsNeedRefresh = true; try store.saveProof(proof, profile: profile)
        let (startedConsent, issued) = try await api.start(phone: phone, secret: secret, consent: consent, grant: grant)
        guard admitted() else { throw CancellationError() }
        try cache(startedConsent, proof: &proof, profile: profile)
        guard let issued else { return } // Saved ON survives unavailable provider/readiness.
        try await broker.claim(ticket: issued, deviceToken: token)
        guard admitted() else { throw CancellationError() }
        let confirmed = try await api.confirm(phone: phone, secret: secret, consent: startedConsent, ticket: issued, grant: grant)
        guard admitted() else { throw CancellationError() }
        try cache(confirmed, proof: &proof, profile: profile)
    }
    func refreshHomeInstallations() {
        guard active, task == nil, let owner = auth, owner == authorization(), let api = makeAPI(owner) else { return }
        generation += 1; let ticket = generation; busy = true
        task = Task { [weak self] in
            guard let self else { return }
            defer { self.completeTask(ticket) }
            do {
                let phones = try await api.phones()
                guard self.active, ticket == self.generation, owner == self.auth, owner == self.authorization(), !Task.isCancelled else { throw CancellationError() }
                self.homePhones = phones; self.homePhonesOwner = owner
            } catch { if ticket == self.generation, owner == self.auth { self.status = Self.message(error) } }
        }
    }
    func removeHomeInstallation(_ phone: InvitationPhone) {
        guard active, let owner = auth, owner == authorization(), homePhonesOwner == owner,
              let api = makeAPI(owner), homePhones.contains(where: { $0.id == phone.id }) else { return }
        if let selected = profile, selected.installationID == phone.id { removeInstallation(selected); return }
        retire(); generation += 1; let ticket = generation; busy = true
        task = Task { [weak self] in
            guard let self else { return }
            defer { self.completeTask(ticket) }
            do {
                guard ticket == self.generation, owner == self.authorization() else { throw CancellationError() }
                try await api.remove(id: phone.id)
                guard self.active, ticket == self.generation, owner == self.auth, owner == self.authorization(), !Task.isCancelled else { throw CancellationError() }
                self.homePhones.removeAll { $0.id == phone.id }; self.status = "Selected home installation removed."
            } catch { if ticket == self.generation, owner == self.auth { self.status = Self.message(error) } }
        }
    }
    func resetUnreadableLocalSettings() {
        guard localStoreUnreadable else { return }
        retire(); bridge.retireRegistration(unregister: true)
        store.resetLocalMetadata(knownIdentity: auth?.identity); reload()
        status = "Local invitation settings reset. Unknown home installations remain; list and remove them while signed in."
    }
    func removeInstallation(_ selected: InvitationProfileMetadata) {
        do {
            _ = try store.disableAll(selected, removal: true)
            if selected.identity == auth?.identity { bridge.retireRegistration(unregister: true); retire(); reload(); launch(requestPermission: false) }
            else { reload(); status = "Removal saved. Sign in to that server account to revoke it remotely." }
        } catch { status = "Could not save installation removal. Try again." }
    }
    func forgetProfile(_ selected: InvitationProfileMetadata) {
        // This is explicit local recovery, not a claim that an offline home has
        // revoked its installation. The settings view explains the distinction.
        do { try store.removeLocal(selected); if selected.identity == auth?.identity { retire(); bridge.retireRegistration(unregister: true) }; reload() }
        catch { status = "Could not remove the saved profile." }
    }
    func rebind() {
        guard active, let owner = auth, let profile, let original = try? store.proof(profile), original.phone != nil,
              let secret = original.secret, let api = makeAPI(owner) else { return }
        do { _ = try store.disableAll(profile); bridge.retireRegistration(unregister: true) } catch { status = "Could not save OFF before rebind."; return }
        retire(); generation += 1; let ticket = generation; busy = true
        task = Task { [weak self] in
            guard let self else { return }
            defer { self.completeTask(ticket) }
            do {
                guard self.current(ticket, owner, profile.installationID) else { throw CancellationError() }
                var proof = original
                if proof.metadataNeedsRefresh {
                    proof = try await self.synchronizePhone(proof, profile: profile, owner: owner, ticket: ticket, api: api)
                }
                guard self.current(ticket, owner, profile.installationID), let latestPhone = proof.phone else { throw CancellationError() }
                proof.metadataNeedsRefresh = true; try self.store.saveProof(proof, profile: profile)
                let rebound = try await api.rebind(phone: latestPhone, secret: secret)
                guard self.current(ticket, owner, profile.installationID) else { throw CancellationError() }
                proof.phone = rebound; proof.loginFingerprint = owner.fingerprint; proof.metadataNeedsRefresh = false; proof.consentsNeedRefresh = true
                try self.store.saveProof(proof, profile: profile); self.status = "Installation rebound. Invitation choices are off; enable screens explicitly."
            } catch { if self.current(ticket, owner, profile.installationID) { self.status = Self.message(error) } }
        }
    }
    func processPendingTap() {
        let pending = bridge.pendingTaps()
        if let first = pending.first, let saved = try? store.profile(installationID: first.installationID) {
            if saved.identity != auth?.identity {
                pendingTapNotice = "Sign in normally to \(saved.identity.origin), account \(saved.identity.userID), to check this invitation. Cinema will not switch accounts automatically."
            } else if !foregroundRemoteEnabled {
                pendingTapNotice = "An invitation is waiting. Enable Cinema remote explicitly to check it and open the remote."
            } else { pendingTapNotice = nil }
        } else { pendingTapNotice = nil }
        guard active, foregroundRemoteEnabled, task == nil, let owner = auth, owner == authorization(),
              let profile, !profile.loggedOut, !profile.pendingDelete, let proof = record,
              proof.loginFingerprint == owner.fingerprint, let phone = proof.phone, let secret = proof.secret,
              let pending = bridge.pendingTaps().first(where: { $0.installationID == profile.installationID }), let api = makeAPI(owner) else { return }
        generation += 1; let ticket = generation; busy = true
        task = Task { [weak self] in
            guard let self else { return }
            defer { self.completeTask(ticket) }
            do {
                let lookup = try await api.lookup(phone: phone, secret: secret, invitationID: pending.invitationID)
                guard self.current(ticket, owner, profile.installationID), self.foregroundRemoteEnabled,
                      let latest = try self.store.profile(owner.identity), !latest.loggedOut, !latest.pendingDelete else { throw CancellationError() }
                guard let choice = latest.choices[lookup.receiverID.uuidString.lowercased()], choice.enabled else {
                    self.bridge.retireTap(pending.id); return
                }
                self.tapHandoff = InvitationTapHandoff(lookup: lookup, owner: owner, installationID: profile.installationID, choiceIntent: choice.intent, deadline: self.monotonic() + 10)
                self.tapReady = lookup; self.bridge.retireTap(pending.id)
            } catch is CancellationError { }
            catch { if self.current(ticket, owner, profile.installationID) { self.bridge.retireTap(pending.id); self.status = "This invitation is no longer available. Open the remote to choose a screen." } }
        }
    }
    var pairedScreens: [RemoteSecretStorage.Grant] {
        guard let auth else { return [] }
        guard let key = RemoteSecretStorage.scopedIdentity(origin: auth.identity.origin, instance: auth.identity.instance, userID: auth.identity.userID) else { return [] }
        return RemoteSecretStorage(identity: key).grants
    }
    func prepareTapHandoff() -> InvitationTapHandoff? {
        defer { tapReady = nil; tapHandoff = nil }
        guard let handoff = tapHandoff, validateTapHandoff(handoff) != nil else { return nil }
        return handoff
    }
    func validateTapHandoff(_ handoff: InvitationTapHandoff) -> InvitationLookup? {
        let now = monotonic()
        guard now.isFinite, now < handoff.deadline, now >= handoff.deadline - 10,
              active, foregroundRemoteEnabled, auth == handoff.owner, authorization() == handoff.owner,
              let latest = try? store.profile(handoff.owner.identity), latest.installationID == handoff.installationID,
              !latest.loggedOut, !latest.pendingDelete,
              let choice = latest.choices[handoff.lookup.receiverID.uuidString.lowercased()], choice.enabled,
              choice.intent == handoff.choiceIntent else { return nil }
        return handoff.lookup
    }
    private static var phoneName: String {
        #if os(iOS)
        return RemoteTextBounds.label(UIDevice.current.name, maximumBytes: 80)
        #else
        return "Phone"
        #endif
    }
    private static func message(_ error: Error) -> String {
        if let error = error as? InvitationAPIError {
            switch error.code {
            case "unauthorized", "login_changed": return "Sign in again, then explicitly rebind this installation."
            case "grant_revoked": return "Pair with this screen again to enable invitations. Turning OFF remains available."
            case "stale_phone", "stale_consent", "stale_transport": return "Settings changed. Refresh before retrying delivery setup."
            case "retention_limit": return "Invitation capacity is full. Remove an installation or saved screen choice."
            case "invalid": return "Installation or delivery setup needs recovery. Refresh or remove this installation to retry."
            default: break
            }
        }
        return "Choice saved locally. Connection or delivery setup is unavailable; refresh to reconcile it."
    }
}
