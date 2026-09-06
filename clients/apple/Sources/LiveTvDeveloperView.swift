import SwiftUI

/// Always present. The server enforces administrator access and the saved
/// generation; enabling is a separate runtime mutation, never a build switch.
struct LiveTvDeveloperView: View {
    @EnvironmentObject private var model: AppModel
    @State private var api: LiveTvAPI?
    @State private var saved: LiveTvSettings?
    @State private var readiness: LiveTvReadiness?
    @State private var ipv4 = ""
    @State private var owner = ""
    @State private var sessions = 2
    @State private var height = 720
    @State private var attested = false
    @State private var busy = false
    @State private var message = "Administrator access is required."
    @State private var revision = UUID()

    private var dirty: Bool {
        guard let saved else { return false }
        return ipv4 != saved.liveTvDeviceIpv4 || owner != saved.liveTvOwnerNodeId
            || sessions != saved.liveTvMaxSessions || height != saved.liveTvOutputHeight
    }

    var body: some View {
        Form {
            Section("HDHomeRun Live TV · runtime enablement") {
                Text("Watch unprotected antenna channels from one network tuner. No special build is needed.")
                Text("Before enabling: finish the HDHomeRun channel scan, reserve a stable private IPv4 address, and choose one reachable, committed tuner-owner node. Keep all serving nodes on a compatible plurx version.")
                Text("The owner needs network access to the tuner, writable scratch space, and FFmpeg H.264/AAC encoding. Every viewer uses one physical tuner and one encoder slot. Start with 720p and two sessions.")
                Text("ATSC 3.0 can require HEVC and AC-4 decoders your FFmpeg lacks. DRM, recording, rewind, captions, and guide scheduling are not supported. Readiness tests the output graph, not every broadcast codec.")
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
                    Picker("Output height", selection: $height) {
                        Text("720p").tag(720)
                        Text("1080p").tag(1080)
                    }.disabled(saved.liveTvEnabled || busy)
                    Button("Save configuration while disabled") {
                        write(.configure(ipv4: ipv4.trimmingCharacters(in: .whitespacesAndNewlines),
                                         owner: owner.trimmingCharacters(in: .whitespacesAndNewlines),
                                         sessions: sessions, height: height))
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
                                             sessions: saved.liveTvMaxSessions, height: saved.liveTvOutputHeight))
                        }.disabled(saved.liveTvEnabled || busy || dirty)
                    }
                }
            }
            if let readiness {
                Section(readiness.ready ? "Saved configuration is ready" : "Readiness needs attention") {
                    ForEach(readiness.checks) { check in
                        Label(check.message, systemImage: check.ready ? "checkmark.circle" : "exclamationmark.triangle")
                    }
                }
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
        height = settings.liveTvOutputHeight
        readiness = nil
        attested = false
    }

    @MainActor private func load() async {
        let expected = UUID()
        revision = expected
        busy = true
        let client = LiveTvAPI(origin: model.origin, token: Session.shared.token)
        api = client
        do {
            let settings = try await client.settings()
            guard revision == expected else { return }
            apply(settings)
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
                message = "Saved. Live TV is \(settings.liveTvEnabled ? "enabled" : "disabled")."
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
}
