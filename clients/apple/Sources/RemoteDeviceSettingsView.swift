import SwiftUI

/// Security settings remain physically operated; confirmations register no
/// companion actions and retain the restricted surface while presented.
struct RemoteDeviceSettingsView: View {
    @EnvironmentObject private var remote: RemoteClientModel
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @AppStorage("plurx.cinemaRemote") private var remoteEnabled = false
    @State private var pendingConfirmation: RemoteManagementConfirmation?
    @FocusState private var focusedControl: String?
    @State private var opener = "refresh"
    @State private var previousOrder: [String] = []
    @State private var restoreTarget: String?
    @State private var restorationRevision: UInt64 = 0

    private var controlOrder: [String] {
        remote.pairedGrants.map { "grant:" + $0.id.uuidString } + ["refresh"]
            + (remoteEnabled && remote.target != nil ? ["pair"] : [])
            + (remote.localReceiverID != nil ? ["reset"] : [])
    }
    var body: some View {
        ScrollViewReader { proxy in
            List {
                #if os(iOS)
                InvitationSettingsSections()
                Section("This phone") {
                    Button("Open Cinema remote") { remote.remotePresented = true }
                    ForEach(remote.localGrants, id: \.id) { grant in
                        VStack(alignment: .leading, spacing: 10) {
                            Text(remote.devices.first { $0.id == grant.receiverID }?.name ?? "Paired screen")
                            Button("Revoke this phone's pairing", role: .destructive) { Task { await remote.revokeGrant(grant.id) } }
                            Button("Forget saved pairing", role: .destructive) { remote.forgetGrant(grant.id) }
                        }
                    }
                }
                #endif
                Section("Paired phones") {
                    ForEach(remote.pairedGrants) { grant in
                        VStack(alignment: .leading, spacing: 10) {
                            Text(grant.name)
                            Button("Revoke pairing", role: .destructive) {
                                #if os(tvOS)
                                confirm(title: "Revoke pairing with \(grant.name)?", message: "This phone will lose control of this TV.") {
                                    await remote.revokeGrant(grant.id)
                                }
                                #else
                                Task { await remote.revokeGrant(grant.id) }
                                #endif
                            }
                            .tvSecondaryControl("grant:" + grant.id.uuidString, focus: $focusedControl, restore: $restoreTarget)
                        }
                        .id("grant:" + grant.id.uuidString)
                    }
                    Button("Refresh pairings") { Task { await remote.refreshGrants() } }
                        .tvSecondaryControl("refresh", focus: $focusedControl, restore: $restoreTarget)
                }
                #if os(tvOS)
                if remoteEnabled, remote.target != nil {
                    Section("Pair a phone") {
                        Button("Pair a phone") { remote.startPairing() }
                            .tvSecondaryControl("pair", focus: $focusedControl, restore: $restoreTarget)
                            .accessibilityIdentifier("remotes-pair-phone")
                    }
                }
                if remote.localReceiverID != nil {
                    Section("This TV") {
                        Text("Reset removes this TV registration and revokes every paired phone.")
                        Button("Reset TV remote registration", role: .destructive) {
                            confirm(title: "Reset this TV's remote registration?", message: "Every paired phone will lose access. Pair phones again after the reset.") {
                                await remote.resetReceiver()
                            }
                        }
                        .tvSecondaryControl("reset", focus: $focusedControl, restore: $restoreTarget)
                        .accessibilityIdentifier("remotes-reset-registration")
                    }
                }
                #endif
                Text(remote.managementStatus).font(.caption)
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            #endif
            .onChange(of: restoreTarget) { _, key in
                guard let key else { return }
                proxy.scrollTo(key, anchor: .center)
                focusedControl = key
            }
        }
        .navigationTitle("Remotes & devices")
        .remoteRestricted()
        .remotePhysicalDirections()
        .onRemotePhysicalInput(navigation) { value in
            if value != restorationRevision { restoreTarget = nil }
        }
        .task { await remote.refreshGrants() }
        #if os(tvOS)
        .remoteChoicePresentation(isPresented: Binding(get: { pendingConfirmation != nil }, set: { if !$0 { pendingConfirmation = nil } }), scope: "remotes:confirmation", onDismiss: {
            guard navigation.canRestorePhysicalFocus(after: "remotes:confirmation") else { restoreTarget = nil; return }
            restorationRevision = navigation.physicalInputRevision
            restoreTarget = TVSecondaryFocusFallback.target(opener: opener, previous: previousOrder, surviving: controlOrder)
            focusedControl = restoreTarget
        }) {
            if let confirmation = pendingConfirmation {
                TVSecondaryConfirmation(title: confirmation.title, message: confirmation.message, onCancel: { navigation.closeModal() }) {
                    let action = confirmation.action
                    navigation.closeModal()
                    action()
                }
            }
        }
        #endif
    }
    private func confirm(title: String, message: String, action: @escaping () async -> Void) {
        guard pendingConfirmation == nil else { return }
        let generation = Session.shared.playbackAuthorization.generation
        opener = focusedControl ?? "refresh"
        previousOrder = controlOrder
        restoreTarget = nil
        pendingConfirmation = RemoteManagementConfirmation(title: title, message: message) {
            Task {
                guard generation == Session.shared.playbackAuthorization.generation else { return }
                await action()
            }
        }
        navigation.openModal(scope: "remotes:confirmation", opener: opener) { pendingConfirmation = nil }
    }
}

private struct RemoteManagementConfirmation {
    let title: String
    let message: String
    let action: () -> Void
}

/// Identity fallback shared by the physical secondary screens. A removed
/// opener chooses its next surviving neighbor, then previous, then recovery.
enum TVSecondaryFocusFallback {
    static func target(opener: String, previous: [String], surviving: [String]) -> String? {
        if surviving.contains(opener) { return opener }
        if let index = previous.firstIndex(of: opener) {
            if let next = previous.dropFirst(index + 1).first(where: surviving.contains) { return next }
            if let prior = previous.prefix(index).reversed().first(where: surviving.contains) { return prior }
        }
        return surviving.first
    }
}

#if os(tvOS)
struct TVSecondaryConfirmation: View {
    let title: String
    let message: String
    var cancelLabel = "Cancel"
    var confirmLabel = "Confirm"
    let onCancel: () -> Void
    let onConfirm: () -> Void
    @Namespace private var focusScope
    var body: some View {
        VStack(alignment: .leading, spacing: 28) {
            Text(title).font(.title2.bold()).fixedSize(horizontal: false, vertical: true)
            Text(message).font(.body).fixedSize(horizontal: false, vertical: true)
            Button(cancelLabel, action: onCancel)
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .prefersDefaultFocus(true, in: focusScope)
                .accessibilityIdentifier("secondary-confirm-cancel")
            Button(confirmLabel, role: .destructive, action: onConfirm)
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .accessibilityIdentifier("secondary-confirm-destructive")
        }
        .padding(48).frame(maxWidth: 900)
        .focusScope(focusScope)
        .remoteRestricted()
    }
}
#endif

extension View {
    @ViewBuilder
    func tvSecondaryControl(_ key: String, focus: FocusState<String?>.Binding, restore: Binding<String?>) -> some View {
        #if os(tvOS)
        self.focused(focus, equals: key)
            .accessibilityIdentifier(key)
            .id(key)
            .onAppear { if restore.wrappedValue == key { focus.wrappedValue = key } }
            .onChange(of: restore.wrappedValue) { _, target in if target == key { focus.wrappedValue = key } }
            .remoteNativeFocusObservation(revision: restore.wrappedValue) { actual in
                if actual && restore.wrappedValue == key { restore.wrappedValue = nil }
            }
        #else
        self
        #endif
    }
}
