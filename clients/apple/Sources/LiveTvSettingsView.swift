import SwiftUI

/// Every server Live TV card the Apple clients draw, and which settings screen
/// owns it. Paul's Developer rule (AGENTS.md, SETTINGS-NAVIGATION-AND-DEVELOPER-
/// STATUS): Developer is a waiting room, not a home. The web moved the tuner,
/// guide, recording and Library channel cards to Settings → Live TV; these
/// clients follow it, in the web's order.
enum LiveTvSettingsPlacement {
    static let liveTvSettings = ["HDHomeRun Live TV", "Programme guide", "Recording", "Library channels"]
    static let developer = [
        "Bounded pause/resume · advisory enablement",
        "Prepared quality handoff · advisory enablement",
        "Enable Live TV · advisory enablement",
    ]

    /// The web's `devGraduation` line for the Enable Live TV card, word for word.
    static let enableLiveTvGraduation =
        "Leaves Developer when: the Live TV plans' outstanding fleet prompts are recorded: "
        + "L-02's leader-restart, cold/warm-start (L6) and scratch-fault (L9) prompts, and "
        + "L-03's capacity offer and caption-positive pass. "
        + "Then: the switch moves to Settings → Live TV as a permanent on/off."
    static let boundedResumeGraduation =
        "Leaves Developer when: the matched fresh-open and pause/resume trials on one physical Apple TV "
        + "(APPLE-PAUSE-RESUME-STATUS) are recorded. "
        + "Then: Paul chooses: the switch moves to Settings → Playback as a permanent on/off, "
        + "or it is removed and bounded resume is simply on."
    static let preparedHandoffGraduation =
        "Leaves Developer when: prepared quality handoff graduates (the quality-switch continuity "
        + "build finishes M2-Android, M1-web and M3). Then: this device's switch moves with it to "
        + "Settings → Playback."
}

/// Settings → Live TV: the tuner, the programme guide, recording and Library
/// channels. These used to sit in Developer; the Live TV enable switch itself
/// still waits there. Always present. The server enforces administrator access
/// and the saved generation; every readiness row here is advisory.
struct LiveTvSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var admin = LiveTvAdminModel(surface: .liveTvSettings)

    var body: some View {
        Form {
            Section("HDHomeRun Live TV") {
                Text("One network device shared by eligible cluster servers. Viewers and recordings on the same channel share one tuner stream; a session stays on its selected server and the tuner itself belongs to the cluster.")
            }
            if let saved = admin.saved {
                Section(saved.liveTvEnabled ? "Live TV is enabled" : "Live TV is disabled") {
                    TextField("Tuner private IPv4", text: $admin.ipv4)
                        .disabled(admin.busy)
                    Picker("Maximum channel streams", selection: $admin.sessions) {
                        ForEach(1...4, id: \.self) { Text(String($0)).tag($0) }
                    }.disabled(admin.busy)
                    Picker("Maximum quality", selection: $admin.height) {
                        Text("Original / Auto").tag(0)
                        Text("480p ceiling").tag(480)
                        Text("720p ceiling").tag(720)
                        Text("1080p ceiling").tag(1080)
                        Text("2160p ceiling").tag(2160)
                    }.disabled(admin.busy)
                    Button("Save configuration") {
                        admin.write(.configure(ipv4: admin.ipv4.trimmingCharacters(in: .whitespacesAndNewlines),
                                               owner: admin.owner.trimmingCharacters(in: .whitespacesAndNewlines),
                                               sessions: admin.sessions, height: 720, maxHeight: admin.height))
                    }.disabled(admin.busy || !admin.dirty)
                    Button("Check saved configuration") { admin.checkReadiness() }.disabled(admin.busy || admin.dirty)
                    Text("Saving preserves enablement and ends streams using the previous configuration. The Live TV on/off switch is in Settings → Developer → Enable Live TV.").font(.caption)
                }
            }
            if let readiness = admin.readiness {
                // Every row the server sends, drawn the same way — including
                // rows this build has never heard of. `start_recovery` arrives
                // here without a line of its own, which is the point: an
                // advisory card that has to be extended for each new check is
                // one that silently drops the check nobody remembered.
                Section(readiness.ready ? "Saved configuration is ready" : "Readiness needs attention") {
                    ForEach(readiness.checks) { check in
                        Label(check.message, systemImage: check.ready ? "checkmark.circle" : "exclamationmark.triangle")
                    }
                }
            }
            Section("Programme guide") {
                Text("What the guide needs to fill in, and whether it is true right now. Nothing here refuses a save, a toggle, or a start — a guide that will not load leaves a working page with number and callsign rows.")
                if let guideReadiness = admin.guideReadiness {
                    ForEach(guideReadiness.checks) { check in
                        Label(check.message, systemImage: check.ready ? "checkmark.circle" : "exclamationmark.triangle")
                    }
                    Text(guideReadiness.allMet
                         ? "Source \(guideReadiness.source) · \(guideReadiness.freshness) · \(guideReadiness.programmes) programmes across \(guideReadiness.matchedChannels) of \(guideReadiness.lineupChannels) lineup channels."
                         : "Source \(guideReadiness.source) · \(guideReadiness.freshness). Unmet rows explain what is missing; the guide is still served, and Live TV still starts.")
                        .font(.caption)
                        .accessibilityIdentifier("live-tv-guide-readiness-summary")
                } else {
                    Text("The guide card has not been read yet, or this node is not the tuner owner.").font(.caption)
                }
                Button("Check the guide") { Task { await admin.loadGuideReadiness() } }.disabled(admin.busy)
            }
            if let saved = admin.saved {
                Section("Recording") {
                    Toggle("Enable recording", isOn: Binding(
                        get: { saved.dvrEnabled },
                        set: { admin.write(.dvrEnabled($0)) }
                    ))
                    Text("This switch is always yours to operate. The checks explain what is needed for safe capture; unmet or unobservable checks never disable or override it.")
                    LabeledContent("Recording root", value: saved.dvrRoot.isEmpty ? "Not set" : saved.dvrRoot)
                    LabeledContent("Reserved tuner slots", value: String(saved.dvrTunerReserve))
                    if let item = admin.developerReadiness?.items.first(where: { $0.id == "dvr" }) {
                        ForEach(item.requirements) { requirement in
                            Label(requirement.title, systemImage: requirement.status == "met" ? "checkmark.circle" : (requirement.status == "unmet" ? "exclamationmark.triangle" : "questionmark.circle"))
                            Text(requirement.evidence).font(.caption)
                        }
                    } else {
                        Text("Readiness is unavailable. That does not gate the enable switch.").font(.caption)
                    }
                    Button("Refresh recording readiness") { Task { await admin.loadDeveloperReadiness(app: model) } }
                }
                Section("Library channels") {
                    Toggle("Enable Library channels", isOn: Binding(
                        get: { saved.libraryChannelsEnabled },
                        set: { admin.write(.libraryChannelsEnabled($0)) }
                    ))
                    Text("Schedules use already-probed local movies and episodes. The checks explain whether this server looks ready; they do not disable or override the switch.")
                    if let item = admin.developerReadiness?.items.first(where: { $0.id == "library_channels" }) {
                        ForEach(item.requirements) { requirement in
                            Label(requirement.title, systemImage: requirement.status == "met" ? "checkmark.circle" : (requirement.status == "unmet" ? "exclamationmark.triangle" : "questionmark.circle"))
                            Text(requirement.evidence).font(.caption)
                        }
                    }
                    Button("Refresh Library channel readiness") { Task { await admin.loadDeveloperReadiness(app: model) } }
                }
            }
            Section {
                Text(admin.message).accessibilityIdentifier("live-tv-settings-status")
                Button("Reload server settings") { Task { await admin.load(app: model) } }.disabled(admin.busy)
            }
        }
        #if os(tvOS)
        // Match Settings: keep these choices in a menu instead of pushing the
        // Form picker's empty destination through the tab bar's stack.
        .pickerStyle(.menu)
        #endif
        .navigationTitle("Live TV")
        .task { await admin.load(app: model) }
        .onDisappear { admin.invalidate() }
    }
}
