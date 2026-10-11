import SwiftUI

/// Security settings are physically operated and never registered for Select.
struct RemoteDeviceSettingsView: View {
    @EnvironmentObject private var remote: RemoteClientModel
    @State private var revision = 0
    var body: some View {
        List {
            #if os(iOS)
            InvitationSettingsSections()
            Section("This phone") {
                Button("Open Cinema remote") { remote.remotePresented = true }
                ForEach(remote.localGrants, id: \.id) { grant in
                    VStack(alignment: .leading, spacing: 10) {
                        Text(remote.devices.first { $0.id == grant.receiverID }?.name ?? "Paired screen")
                        Button("Revoke this phone's pairing", role: .destructive) { Task { await remote.revokeGrant(grant.id); revision += 1 } }
                        Button("Forget saved pairing", role: .destructive) { remote.forgetGrant(grant.id); revision += 1 }
                    }
                }
            }
            #endif
            Section("Paired phones") {
                ForEach(remote.pairedGrants) { grant in
                    VStack(alignment: .leading, spacing: 10) {
                        Text(grant.name)
                        Button("Revoke pairing", role: .destructive) { Task { await remote.revokeGrant(grant.id); revision += 1 } }
                    }
                }
                Button("Refresh pairings") { Task { await remote.refreshGrants() } }
            }
            #if os(tvOS)
            if remote.localReceiverID != nil {
                Section("This TV") {
                    Text("Reset removes this TV registration and revokes every paired phone.")
                    Button("Reset TV remote registration", role: .destructive) { Task { await remote.resetReceiver(); revision += 1 } }
                }
            }
            #endif
            Text(remote.managementStatus).font(.caption)
        }
        .id(revision)
        .navigationTitle("Remotes & devices")
        .remoteRestricted()
        .task { await remote.refreshGrants() }
    }
}
