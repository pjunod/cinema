import Foundation

/// The server's Live TV settings as Settings → Live TV and Settings → Developer
/// both edit them. Load, save and the advisory reads have one owner, so moving
/// a card from one screen to the other moves where it is drawn and nothing
/// else: the same requests, the same generation CAS, the same messages.
///
/// The server enforces administrator access and the saved generation; enabling
/// is a separate runtime mutation, never a build switch. Nothing here reads a
/// readiness value back into an enable or a save.
/// Which settings screen a `LiveTvAdminModel` serves: what it reads and how it speaks.
enum LiveTvAdminSurface: Sendable {
    case liveTvSettings
    case developer

    /// Only Settings → Live TV draws the guide, so only it reads the guide.
    var readsGuide: Bool { self == .liveTvSettings }
    /// Only Developer draws the enable card, so only it reads its prerequisites.
    var readsEnablePrerequisites: Bool { self == .developer }

    var loadedMessage: String {
        switch self {
        case .liveTvSettings: return "Settings loaded. Save the configuration, then check it."
        case .developer: return "Settings loaded. The prerequisites are advisory; enabling is a separate action."
        }
    }

    var readyMessage: String {
        switch self {
        case .liveTvSettings: return "The saved configuration is ready. Live TV is switched on or off in Settings → Developer."
        case .developer: return "Ready. Enable is a separate action."
        }
    }

    var needsAttentionMessage: String {
        switch self {
        case .liveTvSettings: return "Some checks need attention. They are advisory and never block the switch in Settings → Developer."
        case .developer: return "Some prerequisites need attention. They never prevent enabling."
        }
    }
}

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
    private let surface: LiveTvAdminSurface

    init(surface: LiveTvAdminSurface) {
        self.surface = surface
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
            // A failed read is a sentence on the card, never this load's
            // failure and never a reason the enable cannot be pressed.
            await loadPrerequisites()
            message = surface.loadedMessage
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
                message = result.ready ? surface.readyMessage : surface.needsAttentionMessage
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
        guard surface.readsGuide, let api else { return }
        guideReadiness = try? await api.guideReadiness()
    }

    /// The Developer enable card's prerequisites, as the web card reads them.
    func loadPrerequisites() async {
        guard surface.readsEnablePrerequisites, let api else { return }
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
