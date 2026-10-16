import SwiftUI

/// Every server Live TV card the Apple clients draw, and which settings screen
/// owns it. Paul's Developer rule (AGENTS.md, SETTINGS-NAVIGATION-AND-DEVELOPER-
/// STATUS): Developer is a waiting room, not a home. The web moved the Live TV
/// enable switch and the tuner, guide, recording and Library channel cards to
/// Settings → Live TV; these clients follow it, in the web's order. Bounded
/// pause/resume and prepared quality handoff graduated to Settings → Playback.
enum LiveTvSettingsPlacement {
    static let liveTvSettings = ["Enable Live TV", "HDHomeRun Live TV", "Programme guide", "Recording", "Library channels"]
    static let developer = [
        "Remote diagnostics · advisory readiness",
        "Cinema remotes · advisory enablement",
        "Screen invitations · advisory readiness",
    ]
}

/// Settings → Live TV: the Live TV enable switch, the tuner, the programme
/// guide, recording and Library channels. These used to sit in Developer; the
/// enable switch comes first, with its advisory prerequisites beside it. Always
/// present. The server enforces administrator access and the saved generation;
/// every readiness row here is advisory.
struct LiveTvSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var admin = LiveTvAdminModel()

    var body: some View {
        Form {
            Section("Enable Live TV") {
                Text("Watch unprotected antenna channels from one network tuner. No special build is needed.")
                Text("Before enabling: finish the HDHomeRun channel scan and reserve a stable private IPv4 address. Keep cluster servers upgraded and their clocks synchronized.")
                Text("At least one eligible server needs network access to the tuner and writable scratch space. Viewers and recordings share channel transports. Compatible broadcasts are copied without an encoder; conversion routes additionally need a working FFmpeg encoder and tone mapping when HDR must become SDR.")
                Text("ATSC 3.0 can require HEVC and AC-4 decoders your FFmpeg lacks. DRM, rewind, and captions are not supported. Unprotected channels can be scheduled or recorded manually. Readiness tests the output graph, not every broadcast codec, and never gates either switch.")
                if let saved = admin.saved {
                    Label(saved.liveTvEnabled ? "Live TV is enabled" : "Live TV is disabled", systemImage: "info.circle")
                    Button(saved.liveTvEnabled ? "Disable Live TV and drain sessions" : "Enable Live TV") {
                        admin.write(.enabled(!saved.liveTvEnabled))
                    }.disabled(admin.busy)
                }
                // The prerequisites as the server last observed them, every row
                // it sent in the order it sent them — the web card's
                // `/live-tv/readiness` read. Advisory: no row reaches the button.
                if let prerequisites = admin.prerequisites {
                    ForEach(prerequisites.checks) { check in
                        Label("\(check.ready ? "Met" : "Not met"): \(check.message)",
                              systemImage: check.ready ? "checkmark.circle" : "exclamationmark.triangle")
                    }
                    Text("Advisory only. Unknown or unmet requirements do not prevent enabling.").font(.caption)
                } else if let error = admin.prerequisitesError {
                    Text("Readiness unavailable: \(error). You can still enable Live TV.").font(.caption)
                }
                Button("Refresh Live TV prerequisites") { Task { await admin.loadPrerequisites() } }
            }
            Section("HDHomeRun Live TV") {
                Text("One network device shared by eligible cluster servers. Viewers and recordings on the same channel share one tuner stream; a session stays on its selected server and the tuner itself belongs to the cluster.")
            }
            if admin.saved != nil {
                Section("Tuner configuration") {
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
                    Text("Saving preserves enablement and ends streams using the previous configuration. The Live TV on/off switch is under Enable Live TV at the top of this screen.").font(.caption)
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
