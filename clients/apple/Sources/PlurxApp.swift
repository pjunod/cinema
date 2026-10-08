import SwiftUI

#if DEBUG
/// Command-line-only entry point for physical-device acceptance. `devicectl`
/// can supply these defaults when it launches a signed debug build, so the
/// test rig does not need a person to navigate the app or time remote presses.
struct PlaybackAcceptanceLaunch: Equatable {
    /// Read the launch argument itself. A saved `plurx.origin` must never make
    /// an acceptance run appear to have used the shaping proxy.
    let requestedOrigin: String
    let itemId: Int
    let fileId: Int
    let startMs: Int
    let durationMs: Int
    let title: String
    let height: Int?
    let probesEnabled: Bool

    static func current(
        defaults: UserDefaults = .standard,
        arguments: [String] = ProcessInfo.processInfo.arguments
    ) -> Self? {
        let fileId = defaults.integer(forKey: "plurx.acceptance.fileId")
        guard fileId > 0 else { return nil }
        let height = defaults.integer(forKey: "plurx.acceptance.height")
        return Self(
            requestedOrigin: originArgument(arguments) ?? "",
            itemId: max(0, defaults.integer(forKey: "plurx.acceptance.itemId")),
            fileId: fileId,
            startMs: max(0, defaults.integer(forKey: "plurx.acceptance.startMs")),
            durationMs: max(0, defaults.integer(forKey: "plurx.acceptance.durationMs")),
            title: defaults.string(forKey: "plurx.acceptance.title") ?? "Playback acceptance",
            height: height > 0 ? height : nil,
            probesEnabled: defaults.bool(forKey: "plurx.acceptance.probe")
        )
    }

    private static func originArgument(_ arguments: [String]) -> String? {
        guard let flag = arguments.firstIndex(of: "-plurx.origin"),
              arguments.indices.contains(flag + 1)
        else { return nil }
        return Session.canonicalOrigin(arguments[flag + 1])
    }

    func matchesActiveOrigins(model: String, session: String) -> Bool {
        !requestedOrigin.isEmpty
            && Session.canonicalOrigin(model) == requestedOrigin
            && Session.canonicalOrigin(session) == requestedOrigin
    }
}
#endif

@main
struct PlurxApp: App {
    #if os(iOS)
    @UIApplicationDelegateAdaptor(OfflineAppDelegate.self) private var appDelegate
    #endif
    @StateObject private var model = AppModel()
    @StateObject private var remoteNavigation = RemoteNavigationCoordinator()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(remoteNavigation)
                .background(RemotePresentationProbe(navigation: remoteNavigation))
                .preferredColorScheme(model.appearance.preferredColorScheme)
                .fontDesign(model.theme.fontDesign)
                .tint(Palette.accent)
        }
    }
}

/// Navigation targets shared by each top-level tab's navigation stack.
enum Route: Hashable {
    case collection(LibraryCollection)
    case item(Int)
}

struct RootView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    @Environment(\.scenePhase) private var scenePhase
    #if os(iOS)
    @ObservedObject private var downloads = OfflineDownloadManager.shared
    @ObservedObject private var bookDownloads = OfflineBookManager.shared
    #endif
    #if DEBUG
    private let playbackAcceptance = PlaybackAcceptanceLaunch.current()
    #endif

    var body: some View {
        ZStack {
            Palette.bg.ignoresSafeArea()
            switch model.phase {
            case .loading:
                #if os(iOS)
                if downloads.items.isEmpty && bookDownloads.books.isEmpty {
                    ProgressView().tint(Palette.accent)
                } else {
                    // Keep Downloads inside the shared shell. A durable failed
                    // row must not hide Home's session-recovery controls.
                    HomeView(initialTab: HomeLayoutPolicy.offlineLaunchTab)
                }
                #else
                ProgressView().tint(Palette.accent)
                #endif
            case .needServer:
                ConnectView(discovery: model.discovery)
            case .needLogin:
                LoginView()
            case .reconnectFailed:
                #if os(iOS)
                if downloads.items.isEmpty && bookDownloads.books.isEmpty {
                    ReconnectView()
                } else {
                    // Start in the local library without making it a dead-end
                    // root when the saved server is still unreachable.
                    HomeView(initialTab: HomeLayoutPolicy.offlineLaunchTab)
                }
                #else
                ReconnectView()
                #endif
            case .ready:
                #if DEBUG
                if let launch = playbackAcceptance {
                    if launch.matchesActiveOrigins(
                        model: model.origin,
                        session: Session.shared.credentials.origin
                    ) {
                        PlayerView(
                            itemId: launch.itemId,
                            fileId: launch.fileId,
                            startMs: launch.startMs,
                            durationMs: launch.durationMs,
                            title: launch.title,
                            initialHeight: launch.height,
                            diagnosticProbesEnabled: launch.probesEnabled
                        )
                    } else {
                        VStack(spacing: 12) {
                            Text("Acceptance playback stopped")
                                .font(.headline)
                            Text("Active server does not match the requested shaping proxy.")
                            Text("Requested: \(launch.requestedOrigin.isEmpty ? "missing or invalid" : launch.requestedOrigin)")
                            Text("Active: \(model.origin)")
                        }
                        .padding()
                        .accessibilityIdentifier("acceptance-origin-mismatch")
                    }
                } else {
                    HomeView()
                }
                #else
                HomeView()
                #endif
            }
        }
        .onChange(of: model.phase) { _, phase in
            if phase != .ready {
                remoteNavigation.resetIdentity()
                Task { await LiveTvPlayerController.shared.stop(clearProfile: true) }
                Task { await LibraryChannelPlayerController.shared.stop() }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { remoteNavigation.invalidate() }
        }
        .remoteRestricted(model.phase != .ready)
        #if os(iOS)
        .onChange(of: scenePhase) { _, phase in
            guard phase == .active else { return }
            Task { await downloads.resumePendingPreparation() }
            Task { await bookDownloads.syncPendingProgress() }
            mirrorReminders()
        }
        .onChange(of: model.phase) { _, phase in
            guard phase == .ready else { return }
            Task { await downloads.resumePendingPreparation() }
            Task { await bookDownloads.syncPendingProgress() }
            mirrorReminders()
        }
        #endif
    }

    #if os(iOS)
    /// Launch and every foreground. The phone's notifications are a mirror of
    /// the server's reminders, and this is the only moment it can be brought
    /// back into line — see `LocalReminders` for what that cannot cover.
    private func mirrorReminders() {
        guard model.phase == .ready else { return }
        let origin = model.origin
        let token = Session.shared.credentials.token
        Task { await LocalReminders.shared.reconcile(origin: origin, token: token) }
    }
    #endif
}
