import SwiftUI

/// Settings → Developer: only what is still waiting on evidence, each with the
/// line that says what it waits on. The Live TV enable switch and the tuner,
/// guide, recording and Library channel cards graduated to Settings → Live TV
/// (`LiveTvSettingsView`); bounded pause/resume and prepared quality handoff
/// graduated to Settings → Playback (`SettingsView`). Always present; enabling
/// is a runtime choice, never a build switch.
struct LiveTvDeveloperView: View {
    @AppStorage("plurx.cinemaRemote") private var cinemaRemoteEnabled = false
    @EnvironmentObject private var remoteClient: RemoteClientModel

    var body: some View {
        Form {
            Section("Remote diagnostics · advisory readiness") {
                DiagnosticReportButton()
                Text("Local history, capture, sharing and support delivery are available from Help. Platform crash evidence may be delayed or unavailable.").font(.caption)
                Text("Awaiting privacy/resource qualification and physical phone, iPad and Apple TV approval-flow acceptance. This advisory graduates from Developer after those checks pass; local history remains a privacy choice in Help.").font(.caption)
            }
            SharedSharingDeveloperCard()
            Section("Cinema remotes · advisory enablement") {
                Toggle("Enable Cinema remotes on this installation", isOn: $cinemaRemoteEnabled)
                Text(remoteClient.status)
                Text("The server must also enable Cinema remote control. Missing server support or network access does not prevent saving this choice.").font(.caption)
                Text("Awaiting physical Apple TV menus, lazy-grid page boundaries, mixed Siri Remote/phone focus, and cross-platform pairing acceptance. This switch graduates to Remotes & devices after those checks pass.").font(.caption)
            }
            #if os(iOS)
            Section("Screen invitations · advisory readiness") {
                NavigationLink("Saved invitation choices and removal") { RemoteDeviceSettingsView() }
                Text("Each paired screen has its own saved invitation choice, independent of the foreground Cinema remote switch. Provider setup, notification permission, or missing readiness never overrides that choice.").font(.caption)
                Text("Awaiting signed APNs delivery and physical cold-launch acceptance. iOS has no continuous resident connection. This advisory graduates after those checks pass.").font(.caption)
            }
            #endif
        }
        #if os(tvOS)
        // Match Settings: keep these choices in a menu instead of pushing the
        // Form picker's empty destination through the tab bar's stack.
        .pickerStyle(.menu)
        #endif
        .navigationTitle("Developer")
    }
}
