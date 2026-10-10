import Foundation

/// The server's Live TV settings as Settings → Live TV edits them: the enable
/// switch, the tuner, the guide, recording and Library channels. Load, save and
/// the advisory reads have one owner, so the screen draws and this decides
/// nothing: the same requests, the same generation CAS, the same messages.
/// (Developer drew the enable card until 2026-10-10; no Developer card reads
/// server Live TV settings any more, so there is one surface and no switch on it.)
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
    /// The enable card's prerequisites (`GET /live-tv/readiness`); advisory only.
    @Published private(set) var prerequisites: LiveTvReadiness?
    /// Why `prerequisites` could not be read, when it could not. Never a gate.
    @Published private(set) var prerequisitesError: String?
    @Published var ipv4 = ""
    @Published private(set) var owner = ""
    @Published var sessions = 2
    @Published var height = 0
    @Published private(set) var busy = false
    @Published private(set) var message = "Administrator access is required."
    private var revision = UUID()

    nonisolated static let loadedMessage = "Settings loaded. The prerequisites are advisory; enabling is a separate action. Save the configuration, then check it."
    nonisolated static let readyMessage = "The saved configuration is ready. Live TV is switched on or off under Enable Live TV at the top of this screen."
    nonisolated static let needsAttentionMessage = "Some checks need attention. They are advisory and never block the Enable Live TV switch at the top of this screen."

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
            // A failed read is a sentence on the card, never this load's
            // failure and never a reason the enable cannot be pressed.
            await loadPrerequisites()
            message = Self.loadedMessage
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
                await loadPrerequisites()
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
                message = result.ready ? Self.readyMessage : Self.needsAttentionMessage
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
        guard let api else { return }
        guideReadiness = try? await api.guideReadiness()
    }

    /// The Settings → Live TV enable card's prerequisites, as the web card reads them.
    func loadPrerequisites() async {
        guard let api else { return }
        do {
            prerequisites = try await api.currentReadiness()
            prerequisitesError = nil
        } catch {
            prerequisites = nil
            prerequisitesError = error.localizedDescription
        }
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
