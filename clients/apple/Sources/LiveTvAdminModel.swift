import Foundation

/// The server's Live TV settings as Settings → Live TV and Settings → Developer
/// both edit them. Load, save and the advisory reads have one owner, so moving
/// a card from one screen to the other moves where it is drawn and nothing
/// else: the same requests, the same generation CAS, the same messages.
///
/// The server enforces administrator access and the saved generation; enabling
/// is a separate runtime mutation, never a build switch. Nothing here reads a
/// readiness value back into an enable or a save.
@MainActor
final class LiveTvAdminModel: ObservableObject {
    @Published private(set) var api: LiveTvAPI?
    @Published private(set) var saved: LiveTvSettings?
    @Published private(set) var readiness: LiveTvReadiness?
    @Published private(set) var guideReadiness: LiveTvGuideReadiness?
    @Published private(set) var developerReadiness: DeveloperReadiness?
    @Published var ipv4 = ""
    @Published private(set) var owner = ""
    @Published var sessions = 2
    @Published var height = 0
    @Published private(set) var busy = false
    @Published private(set) var message = "Administrator access is required."
    private var revision = UUID()
    /// Developer no longer draws the guide, so it does not read it.
    private let readsGuide: Bool

    init(readsGuide: Bool) {
        self.readsGuide = readsGuide
    }

    var dirty: Bool {
        guard let saved else { return false }
        return ipv4 != saved.liveTvDeviceIpv4
            || sessions != saved.liveTvMaxSessions || height != saved.liveTvMaxOutputHeight
    }

    /// A response that lands after the screen left must not repaint it.
    func invalidate() {
        revision = UUID()
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
    }

    func load(app: AppModel) async {
        let expected = UUID()
        revision = expected
        busy = true
        let client = LiveTvAPI(origin: app.origin, token: Session.shared.credentials.token)
        api = client
        do {
            let settings = try await client.settings()
            guard revision == expected else { return }
            apply(settings)
            await loadDeveloperReadiness(app: app)
            await loadGuideReadiness()
            message = "Settings loaded. Save, check readiness, then enable."
        } catch {
            guard revision == expected else { return }
            saved = nil
            message = error.localizedDescription
        }
        busy = false
    }

    func write(_ change: LiveTvSettingsChange) {
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

    func checkReadiness() {
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
    func loadGuideReadiness() async {
        guard readsGuide, let api else { return }
        guideReadiness = try? await api.guideReadiness()
    }

    func loadDeveloperReadiness(app: AppModel) async {
        do {
            developerReadiness = try await app.requireAPI().developerReadiness()
        } catch {
            developerReadiness = nil
            message = error.localizedDescription
        }
    }
}
