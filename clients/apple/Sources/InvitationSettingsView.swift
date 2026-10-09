import SwiftUI

#if os(iOS)
/// This stays reachable with the separate foreground remote switch OFF and
/// without a live TV/grant. Destructive installation recovery is physical UI.
struct InvitationSettingsSections: View {
    @EnvironmentObject private var invitations: InvitationClientModel
    @EnvironmentObject private var remote: RemoteClientModel
    @AppStorage("plurx.cinemaRemote") private var remoteEnabled = false
    @State private var deleteHome: InvitationPhone?
    @State private var resetLocal = false
    @State private var forget: InvitationProfileMetadata?
    private var receiverIDs: [UUID] {
        Set((invitations.profile?.choices.keys.compactMap(UUID.init(uuidString:)) ?? []) + invitations.pairedScreens.map(\.receiverID))
            .sorted { $0.uuidString < $1.uuidString }
    }
    var body: some View {
        Section("Screen invitations") {
            Text("Receive a generic notification when a paired screen is ready. Opening it checks your current sign-in and opens the remote; taking control is a separate action.").font(.caption)
            if !remoteEnabled {
                Text("Invitations are a separate saved choice. Enable Cinema remote in Developer settings before opening one to control a screen. Turning invitations OFF and removing this installation remain available.").font(.caption)
            }
            ForEach(receiverIDs, id: \.self) { receiver in
                let key = receiver.uuidString.lowercased()
                let name = remote.devices.first { $0.id == receiver }?.name ?? invitations.profile?.choices[key]?.name ?? "Paired screen"
                Toggle(name, isOn: Binding(get: { invitations.profile?.choices[key]?.enabled ?? false }, set: { invitations.saveChoice($0, receiver: receiver, name: name) }))
                if let consent = invitations.record?.consents[key] {
                    Text(readiness(consent.readiness)).font(.caption).foregroundStyle(.secondary)
                }
                if invitations.profile?.choices[key]?.pending == true { Text("Saved on this phone; waiting to synchronize.").font(.caption) }
                if invitations.profile?.choices[key]?.enabled == true {
                    Button("Retry delivery setup for \(name)") { invitations.retryTransport(receiver: receiver) }
                }
            }
            if receiverIDs.isEmpty { Text("Pair with a screen to enable invitations. Saved choices remain here when a screen is offline.").font(.caption) }
            if let notice = invitations.pendingTapNotice { Text(notice).font(.caption) }
            Text(invitations.status).font(.caption)
            Text("Push delivery needs a signed APNs-enabled app and a configured server provider. Simulator builds do not verify delivery. iOS has no continuous resident connection; OFF takes effect on this phone immediately; an offline home may still send until synchronization, and alerts already sent cannot be recalled. Generic alerts cannot identify which saved screen changed; every tap checks current authority.").font(.caption)
            Button("Refresh invitation settings") { invitations.refresh() }
            if invitations.requiresRebind { Button("Rebind to this sign-in and turn old choices OFF") { invitations.rebind() } }
            if invitations.requiresReset { Text("The one-time proof is unavailable. Remove this installation online, or explicitly forget it locally and register a new one.").font(.caption) }
            if let profile = invitations.profile {
                Button("Remove invitation installation", role: .destructive) { invitations.removeInstallation(profile) }
            }
        }
        Section("Home invitation installations") {
            Text("Lists this signed-in account's installations, including a phone whose local proof was lost. Removal needs no paired-screen proof or foreground remote switch.").font(.caption)
            Button("List home installations") { invitations.refreshHomeInstallations() }
            ForEach(invitations.homePhones, id: \.id) { phone in
                VStack(alignment: .leading, spacing: 8) {
                    Text(phone.name)
                    Text(phone.id.uuidString.lowercased()).font(.caption).textSelection(.enabled)
                    Button("Remove this home installation", role: .destructive) { deleteHome = phone }
                }
            }
            if invitations.localStoreUnreadable { Button("Reset unreadable local invitation settings", role: .destructive) { resetLocal = true } }
        }
        .confirmationDialog("Remove this selected home installation and its invitations?", isPresented: Binding(get: { deleteHome != nil }, set: { if !$0 { deleteHome = nil } })) {
            Button("Remove selected installation", role: .destructive) { if let deleteHome { invitations.removeHomeInstallation(deleteHome) }; deleteHome = nil }
        }
        .confirmationDialog("Reset local invitation settings? Unknown home installations remain until removed while signed in.", isPresented: $resetLocal) {
            Button("Reset local settings", role: .destructive) { invitations.resetUnreadableLocalSettings() }
        }
        Section("Saved invitation profiles") {
            Text("Up to eight server/account profiles are saved. Removing locally frees capacity but cannot revoke an unreachable server installation; sign in there to remove it remotely.").font(.caption)
            ForEach(invitations.profiles) { profile in
                VStack(alignment: .leading, spacing: 8) {
                    Text(profile.identity.origin)
                    Text("Account \(profile.identity.userID) · \(profile.name)").font(.caption)
                    if profile.pendingDelete { Text("Removal queued; sign in to synchronize it.").font(.caption) }
                    Button("Queue installation removal", role: .destructive) { invitations.removeInstallation(profile) }
                    Button("Forget saved invitation profile", role: .destructive) { forget = profile }
                }
            }
        }
        .confirmationDialog("Forget this saved profile? The server installation may remain until removed while signed in.", isPresented: Binding(get: { forget != nil }, set: { if !$0 { forget = nil } })) {
            Button("Forget locally", role: .destructive) { if let forget { invitations.forgetProfile(forget) }; forget = nil }
        }
    }
    private func readiness(_ value: InvitationReadiness) -> String {
        switch value {
        case .disabled: return "Invitations are off."
        case .ready: return "Delivery setup is ready; actual provider delivery remains unverified."
        case .globalDisabled: return "Saved choice is on; server Cinema remote control is off."
        case .loginChanged: return "Saved choice is on; explicit sign-in rebind is needed."
        case .grantRevoked: return "Saved choice is on; pair with this screen again."
        case .permissionUnavailable: return "Saved choice is on; iOS notification permission is unavailable."
        case .providerUnconfigured: return "Saved choice is on; the server provider is not configured."
        case .transportPending: return "Saved choice is on; delivery setup is pending."
        case .transportUnavailable: return "Saved choice is on; delivery setup is unavailable."
        case .retentionLimit: return "Saved choice is on; server capacity needs cleanup."
        case .migrationRemediation: return "Saved choice is on; server delivery migration needs operator attention."
        }
    }
}
#endif

#if os(iOS)
struct InvitationTapRecoveryView: View {
    @EnvironmentObject private var invitations: InvitationClientModel
    @EnvironmentObject private var model: AppModel
    @AppStorage("plurx.cinemaRemote") private var remoteEnabled = false
    var body: some View {
        NavigationStack {
            List {
                Text(invitations.pendingTapNotice ?? "Invitations expire shortly. Open the remote to choose a screen if this one is no longer available.")
                if !remoteEnabled {
                    Button("Enable Cinema remote and check invitation") { remoteEnabled = true }
                }
                if model.phase == .ready {
                    NavigationLink("Account and server settings") { SettingsView() }
                    NavigationLink("Invitation choices and removal") { RemoteDeviceSettingsView() }
                } else { Text("Use the sign-in screen to connect to the saved server and account. This invitation never signs you in or takes control automatically.") }
                Button("Check pending invitation") { invitations.processPendingTap() }
            }
            .navigationTitle("Screen invitation")
        }
        .remoteRestricted()
    }
}
#endif
