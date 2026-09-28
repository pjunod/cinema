import SwiftUI

/// Settings → Developer: only what is still waiting on evidence, each with the
/// line that says what it waits on. The tuner, guide, recording and Library
/// channel cards graduated to Settings → Live TV (`LiveTvSettingsView`).
/// Always present. The server enforces administrator access and the saved
/// generation; enabling is a separate runtime mutation, never a build switch.
struct LiveTvDeveloperView: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage("plurx.preparedHandoff") private var preparedHandoffEnabled = true
    @AppStorage("plurx.boundedResume") private var boundedResumeEnabled = true
    @ObservedObject private var handoff = Caps.PreparedHandoffTelemetry.shared
    @StateObject private var admin = LiveTvAdminModel(readsGuide: false)

    var body: some View {
        Form {
            Section("Bounded pause/resume · advisory enablement") {
                Toggle("Enable bounded pause/resume", isOn: $boundedResumeEnabled)
                Text("On by default. Resume consumes a healthy retained buffer immediately and gives an established on-demand item one recipe-preserving repair inside one 15-second budget.")
                Label("Client ownership and deadline implementation: Met", systemImage: "checkmark.circle")
                Label("Server or protocol change required: Met · None", systemImage: "checkmark.circle")
                Label("Matched physical Apple TV latency: Not met", systemImage: "exclamationmark.triangle")
                Text("Qualification still needs matched fresh-open and pause/resume trials on the same Apple TV, delivery presentation, recipe, cache state, and network. This unmet row never disables or overrides the switch.")
                    .font(.caption)
                Text("Live TV, cold startup, AirPlay, and Picture in Picture keep their existing owners and budgets. A missing local video sample is not treated as a failed picture on an external display.")
                    .font(.caption)
                Text(LiveTvSettingsPlacement.boundedResumeGraduation).font(.caption)
            }
            Section("Prepared quality handoff · advisory enablement") {
                Toggle("Enable two-player prepared handoff", isOn: $preparedHandoffEnabled)
                Text("On by default. This controls whether Apple advertises dual-player preparation. The checks below explain risk; they never disable or override the switch, and you can turn it off here.")
                Label("Client implementation and first-frame proof: Met", systemImage: "checkmark.circle")
                Label("Measured Apple cohort: Met", systemImage: "checkmark.circle")
                Text("iPhone 17 Pro Max and Apple TV 4K (3rd generation) each completed 20 of 20 same-codec and codec/HDR handoffs.")
                    .font(.caption)
                Label("Server successor media priming: Met", systemImage: "checkmark.circle")
                Text("The server reserves the successor before warming local or remote media and cleans up a refused, cancelled, or expired attempt.")
                    .font(.caption)
                Label("Fleet and link evidence: Not met · Checked during playback", systemImage: "exclamationmark.triangle")
                Text("Throughput, decoder capacity, and physical-device observations improve rollout confidence. Missing or low evidence never disables this switch.")
                    .font(.caption)
                Label("Last directed quality change: \(handoff.lastOutcome ?? "none yet")", systemImage: "info.circle")
                Text("What the most recent viewer-initiated quality change did. Informational only — it never blocks a change and never overrides the switch above.")
                    .font(.caption)
                Label("Last server preparation value: \(handoff.lastPreparation ?? "not sent")", systemImage: "info.circle")
                Text("The newest delivery.preparation this device saw. Older servers and relays do not send it, and its absence is never read as a refusal.")
                    .font(.caption)
                // M3's measurements of that same change, and the one audio
                // measurement this platform does not take. Advisory in the
                // strict sense: no switch is attached to any of it, nothing
                // reads it back, and an unmet condition never blocks a change.
                if let measured = handoff.lastSwitch {
                    Label("Frames at the switch: \(measured.frames)", systemImage: "info.circle")
                    Label("Audio at the switch: \(measured.audio)", systemImage: "info.circle")
                    Label("Tap to new quality: \(measured.visibleIn)", systemImage: "info.circle")
                }
                Text("Measuring the switch \u{2014} what it needs, and what it cannot have:")
                    .font(.caption)
                ForEach(PreparedSwitchMeasurement.audioRequirements(accessLogAvailable: true)) { requirement in
                    Label(
                        "\(requirement.title): \(requirement.met ? "Met" : "Not met")",
                        systemImage: requirement.met ? "checkmark.circle" : "exclamationmark.triangle"
                    )
                    Text(requirement.detail).font(.caption)
                }
                Text(LiveTvSettingsPlacement.preparedHandoffGraduation).font(.caption)
            }
            Section("Enable Live TV · advisory enablement") {
                Text("Watch unprotected antenna channels from one network tuner. No special build is needed.")
                Text("Before enabling: finish the HDHomeRun channel scan and reserve a stable private IPv4 address. Keep cluster servers upgraded and their clocks synchronized.")
                Text("At least one eligible server needs network access to the tuner and writable scratch space. Viewers and recordings share channel transports. Compatible broadcasts are copied without an encoder; conversion routes additionally need a working FFmpeg encoder and tone mapping when HDR must become SDR.")
                Text("ATSC 3.0 can require HEVC and AC-4 decoders your FFmpeg lacks. DRM, rewind, and captions are not supported. Unprotected channels can be scheduled or recorded manually. Readiness tests the output graph, not every broadcast codec, and never gates either switch.")
                if let saved = admin.saved {
                    Label(saved.liveTvEnabled ? "Live TV is enabled" : "Live TV is disabled", systemImage: "info.circle")
                    Button(saved.liveTvEnabled ? "Disable Live TV and drain sessions" : "Enable Live TV") {
                        admin.write(.enabled(!saved.liveTvEnabled))
                    }.disabled(admin.busy)
                    Text("Readiness is advisory; unmet checks do not prevent enabling. Configure the tuner and check the saved configuration in Settings → Live TV.").font(.caption)
                }
                Text(LiveTvSettingsPlacement.enableLiveTvGraduation).font(.caption)
            }
            Section {
                Text(admin.message).accessibilityIdentifier("live-tv-developer-status")
                Button("Reload server settings") { Task { await admin.load(app: model) } }.disabled(admin.busy)
            }
        }
        #if os(tvOS)
        // Match Settings: keep these choices in a menu instead of pushing the
        // Form picker's empty destination through the tab bar's stack.
        .pickerStyle(.menu)
        #endif
        .navigationTitle("Developer")
        .task { await admin.load(app: model) }
        .onDisappear { admin.invalidate() }
    }
}
