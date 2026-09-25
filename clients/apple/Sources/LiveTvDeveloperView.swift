import SwiftUI

/// Always present. The server enforces administrator access and the saved
/// generation; enabling is a separate runtime mutation, never a build switch.
struct LiveTvDeveloperView: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage("plurx.preparedHandoff") private var preparedHandoffEnabled = true
    @AppStorage("plurx.boundedResume") private var boundedResumeEnabled = true
    @ObservedObject private var handoff = Caps.PreparedHandoffTelemetry.shared
    @State private var api: LiveTvAPI?
    @State private var saved: LiveTvSettings?
    @State private var readiness: LiveTvReadiness?
    @State private var guideReadiness: LiveTvGuideReadiness?
    @State private var developerReadiness: DeveloperReadiness?
    @State private var ipv4 = ""
    @State private var owner = ""
    @State private var sessions = 2
    @State private var height = 0
    @State private var attested = false
    @State private var busy = false
    @State private var message = "Administrator access is required."
    @State private var revision = UUID()

    private var dirty: Bool {
        guard let saved else { return false }
        return ipv4 != saved.liveTvDeviceIpv4 || owner != saved.liveTvOwnerNodeId
            || sessions != saved.liveTvMaxSessions || height != saved.liveTvMaxOutputHeight
    }

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
            }
            if let saved {
                Section("Library channels · advisory enablement") {
                    Toggle("Enable Library channels", isOn: Binding(
                        get: { saved.libraryChannelsEnabled },
                        set: { write(.libraryChannelsEnabled($0)) }
                    ))
                    Text("Schedules use already-probed local movies and episodes. The checks explain whether this server looks ready; they do not disable or override the switch.")
                    if let item = developerReadiness?.items.first(where: { $0.id == "library_channels" }) {
                        ForEach(item.requirements) { requirement in
                            Label(requirement.title, systemImage: requirement.status == "met" ? "checkmark.circle" : (requirement.status == "unmet" ? "exclamationmark.triangle" : "questionmark.circle"))
                            Text(requirement.evidence).font(.caption)
                        }
                    }
                    Button("Refresh Library channel readiness") { Task { await loadDeveloperReadiness() } }
                }
                Section("Recording · advisory enablement") {
                    Toggle("Enable recording", isOn: Binding(
                        get: { saved.dvrEnabled },
                        set: { write(.dvrEnabled($0)) }
                    ))
                    Text("This switch is always yours to operate. The checks explain what is needed for safe capture; unmet or unobservable checks never disable or override it.")
                    LabeledContent("Recording root", value: saved.dvrRoot.isEmpty ? "Not set" : saved.dvrRoot)
                    LabeledContent("Reserved tuner slots", value: String(saved.dvrTunerReserve))
                    if let item = developerReadiness?.items.first(where: { $0.id == "dvr" }) {
                        ForEach(item.requirements) { requirement in
                            Label(requirement.title, systemImage: requirement.status == "met" ? "checkmark.circle" : (requirement.status == "unmet" ? "exclamationmark.triangle" : "questionmark.circle"))
                            Text(requirement.evidence).font(.caption)
                        }
                    } else {
                        Text("Readiness is unavailable. That does not gate the enable switch.").font(.caption)
                    }
                    Button("Refresh recording readiness") { Task { await loadDeveloperReadiness() } }
                }
            }
            Section("HDHomeRun Live TV · runtime enablement") {
                Text("Watch unprotected antenna channels from one network tuner. No special build is needed.")
                Text("Before enabling: finish the HDHomeRun channel scan, reserve a stable private IPv4 address, and choose one reachable, committed tuner-owner node. Keep all serving nodes on a compatible plurx version.")
                Text("The owner needs network access to the tuner and writable scratch space. Compatible broadcasts are copied without an encoder; conversion routes additionally need a working FFmpeg encoder and tone mapping when HDR must become SDR.")
                Text("ATSC 3.0 can require HEVC and AC-4 decoders your FFmpeg lacks. DRM, rewind, and captions are not supported. Unprotected channels can be scheduled or recorded manually. Readiness tests the output graph, not every broadcast codec, and never gates either switch.")
            }
            if let saved {
                Section(saved.liveTvEnabled ? "Live TV is enabled" : "Live TV is disabled") {
                    TextField("Tuner private IPv4", text: $ipv4)
                        .disabled(saved.liveTvEnabled || busy)
                    TextField("Tuner-owner node ID", text: $owner)
                        .disabled(saved.liveTvEnabled || busy)
                    Text("Copy the node ID from the server's Settings → Cluster page.").font(.caption)
                    Picker("Maximum sessions", selection: $sessions) {
                        ForEach(1...4, id: \.self) { Text(String($0)).tag($0) }
                    }.disabled(saved.liveTvEnabled || busy)
                    Picker("Maximum quality", selection: $height) {
                        Text("Original / Auto").tag(0)
                        Text("480p ceiling").tag(480)
                        Text("720p ceiling").tag(720)
                        Text("1080p ceiling").tag(1080)
                        Text("2160p ceiling").tag(2160)
                    }.disabled(saved.liveTvEnabled || busy)
                    Button("Save configuration while disabled") {
                        write(.configure(ipv4: ipv4.trimmingCharacters(in: .whitespacesAndNewlines),
                                         owner: owner.trimmingCharacters(in: .whitespacesAndNewlines),
                                         sessions: sessions, height: 720, maxHeight: height))
                    }.disabled(saved.liveTvEnabled || busy || !dirty)
                    Button("Check saved configuration") { checkReadiness() }.disabled(busy || dirty)
                    Button(saved.liveTvEnabled ? "Disable Live TV and drain sessions" : "Enable Live TV") {
                        write(.enabled(!saved.liveTvEnabled))
                    }.disabled(busy || dirty)
                    Text("Saving never enables playback. Enable rechecks readiness on the server. Disable before changing owner or tuner.").font(.caption)
                }
                if !saved.liveTvTransitionFromOwnerNodeId.isEmpty {
                    Section("Previous owner cleanup is unconfirmed") {
                        Text("Previous owner: \(saved.liveTvTransitionFromOwnerNodeId) · generations before \(saved.liveTvTransitionDrainBefore). An unreachable node is not proof that its tuner process stopped.")
                        Text("First restore the old node's connection, then save the disabled configuration to retry authenticated cleanup. Only use physical recovery after actually stopping that node and preventing its restart.")
                        Toggle("I have stopped or powered off the previous owner and prevented it from restarting until it can synchronize the current configuration.", isOn: $attested)
                            .disabled(saved.liveTvEnabled || busy)
                        Button("Record physical fencing of this exact previous owner") {
                            write(.fencedOwner(owner: saved.liveTvTransitionFromOwnerNodeId,
                                               cutoff: saved.liveTvTransitionDrainBefore))
                        }.disabled(saved.liveTvEnabled || busy || !attested || dirty)
                        Button("Retry authenticated cleanup while disabled") {
                            write(.configure(ipv4: saved.liveTvDeviceIpv4, owner: saved.liveTvOwnerNodeId,
                                             sessions: saved.liveTvMaxSessions, height: saved.liveTvOutputHeight,
                                             maxHeight: saved.liveTvMaxOutputHeight))
                        }.disabled(saved.liveTvEnabled || busy || dirty)
                    }
                }
            }
            if let readiness {
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
            Section("Programme guide · advisory readiness") {
                Text("What the guide needs to fill in, and whether it is true right now. Nothing here refuses a save, a toggle, or a start — a guide that will not load leaves a working page with number and callsign rows.")
                if let guideReadiness {
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
                Button("Check the guide") { Task { await loadGuideReadiness() } }.disabled(busy)
            }
            Section {
                Text(message).accessibilityIdentifier("live-tv-developer-status")
                Button("Reload server settings") { Task { await load() } }.disabled(busy)
            }
        }
        .navigationTitle("Developer")
        .task { await load() }
        .onDisappear { revision = UUID() }
    }

    private func apply(_ settings: LiveTvSettings) {
        saved = settings
        ipv4 = settings.liveTvDeviceIpv4
        owner = settings.liveTvOwnerNodeId
        sessions = settings.liveTvMaxSessions
        height = settings.liveTvMaxOutputHeight
        readiness = nil
        // A save can change the guide source, so the card that described the
        // previous one is cleared rather than left to look current.
        guideReadiness = nil
        attested = false
    }

    @MainActor private func load() async {
        let expected = UUID()
        revision = expected
        busy = true
        let client = LiveTvAPI(origin: model.origin, token: Session.shared.credentials.token)
        api = client
        do {
            let settings = try await client.settings()
            guard revision == expected else { return }
            apply(settings)
            await loadDeveloperReadiness()
            await loadGuideReadiness()
            message = "Settings loaded. Save, check readiness, then enable."
        } catch {
            guard revision == expected else { return }
            saved = nil
            message = error.localizedDescription
        }
        busy = false
    }

    private func write(_ change: LiveTvSettingsChange) {
        guard !busy, let saved, let api else { return }
        busy = true
        let expected = revision
        Task { @MainActor in
            do {
                let settings = try await api.update(change, generation: saved.liveTvConfigGeneration)
                guard revision == expected else { return }
                apply(settings)
                await loadGuideReadiness()
                message = "Saved. Recording is \(settings.dvrEnabled ? "enabled" : "disabled"); Library channels are \(settings.libraryChannelsEnabled ? "enabled" : "disabled"); Live TV is \(settings.liveTvEnabled ? "enabled" : "disabled")."
            } catch {
                guard revision == expected else { return }
                // No automatic retry of an uncertain mutation: reload its
                // authoritative generation before another operator action.
                self.saved = nil
                readiness = nil
                message = error.localizedDescription + " Reload settings before trying again."
            }
            busy = false
        }
    }

    private func checkReadiness() {
        guard !busy, !dirty, let api, let saved else { return }
        busy = true
        let expected = revision
        Task { @MainActor in
            do {
                let result = try await api.readiness()
                guard revision == expected else { return }
                guard result.generation == saved.liveTvConfigGeneration else {
                    throw LiveTvFailure(code: "settings_conflict")
                }
                readiness = result
                message = result.ready ? "Ready. Enable is a separate action." : "Resolve the failed checks before enabling."
            } catch {
                guard revision == expected else { return }
                readiness = nil
                message = error.localizedDescription
            }
            busy = false
        }
    }

    /// Advisory in the strongest sense: a card that cannot be read is an empty
    /// card, never an error banner and never a reason to stop the operator
    /// saving or enabling. A non-owner node answers `owner_unavailable` here
    /// and that is a normal, expected answer.
    @MainActor private func loadGuideReadiness() async {
        guard let api else { return }
        guideReadiness = try? await api.guideReadiness()
    }

    @MainActor private func loadDeveloperReadiness() async {
        do {
            developerReadiness = try await model.requireAPI().developerReadiness()
        } catch {
            developerReadiness = nil
            message = error.localizedDescription
        }
    }
}
