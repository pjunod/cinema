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
    @StateObject private var remotePlayback = RemotePlaybackAdapter()
    @StateObject private var remoteClient = RemoteClientModel()
    @StateObject private var invitations = InvitationClientModel()
    #if os(tvOS)
    @StateObject private var remotePhysical = RemotePhysicalInputObserver()
    #endif

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .environmentObject(remoteNavigation)
                .environmentObject(remotePlayback)
                .environmentObject(remoteClient)
                .environmentObject(invitations)
                .background(RemotePresentationProbe(navigation: remoteNavigation))
                #if os(tvOS)
                .onAppear { remotePhysical.start { remotePlayback.physicalInput(); remoteNavigation.physicalInput() } }
                #endif
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
    case sharedLibraries
    case sharedLibrary(SharedLibraryRow, parent: SharedPlaybackReference?, title: String?)
    case sharedItem(SharedPlaybackReference, SharedLibraryRow)
}

struct RootView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    @EnvironmentObject private var remotePlayback: RemotePlaybackAdapter
    @EnvironmentObject private var remoteClient: RemoteClientModel
    @EnvironmentObject private var invitations: InvitationClientModel
    @AppStorage("plurx.cinemaRemote") private var remoteEnabled = false
    @Environment(\.scenePhase) private var scenePhase
    #if os(iOS)
    @State private var invitationRecoveryPresented = false
    @State private var pendingInvitationHandoff: InvitationTapHandoff?
    @State private var invitationForegroundPresented = false
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
            #if os(iOS)
            invitations.configure(active: scenePhase == .active && phase == .ready, remoteEnabled: remoteEnabled)
            if phase != .ready { pendingInvitationHandoff = nil }
            #endif
            if phase != .ready {
                remoteNavigation.resetIdentity()
                Task { await LiveTvPlayerController.shared.stop(clearProfile: true) }
                Task { await LibraryChannelPlayerController.shared.stop() }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            remoteClient.sceneChanged(active: phase == .active, background: phase == .background)
            #if os(iOS)
            invitations.configure(active: phase == .active && model.phase == .ready, remoteEnabled: remoteEnabled)
            if phase != .active { pendingInvitationHandoff = nil }
            #endif
            if phase != .active { remoteNavigation.invalidate() }
        }
        .remoteRestricted(model.phase != .ready)
        .task(id: remoteLifecycleKey) {
            remoteClient.configure(model: model, navigation: remoteNavigation, playback: remotePlayback,
                                   foreground: scenePhase == .active, background: scenePhase == .background, enabled: remoteEnabled)
            #if os(iOS)
            invitations.configure(active: scenePhase == .active && model.phase == .ready, remoteEnabled: remoteEnabled)
            #endif
        }
        .overlay(alignment: .topTrailing) {
            #if os(tvOS)
            if remoteEnabled, model.phase == .ready, remoteClient.target != nil {
                Button("Pair a phone") { remoteClient.startPairing() }
                    .buttonStyle(.bordered)
                    .padding(24)
            }
            #endif
        }
        .overlay { RemotePairingApprovalView() }
        #if os(iOS)
        .overlay(alignment: .bottom) {
            VStack {
                if invitations.pendingTapNotice != nil {
                    Button("Review screen invitation") { invitationRecoveryPresented = true }.buttonStyle(.borderedProminent)
                }
                if !invitationForegroundPresented { RemoteSuggestionCard() }
            }.padding()
        }
        .sheet(isPresented: $invitationRecoveryPresented, onDismiss: finishInvitationHandoff) { InvitationTapRecoveryView() }
        .onReceive(NotificationCenter.default.publisher(for: .plurxInvitationForeground)) { _ in invitationForegroundPresented = true }
        .task(id: invitationForegroundPresented) {
            guard invitationForegroundPresented else { return }
            try? await Task.sleep(for: .seconds(8))
            if !Task.isCancelled { invitationForegroundPresented = false }
        }
        .sheet(isPresented: $remoteClient.remotePresented) { RemoteCompanionView() }
        .onChange(of: remoteEnabled) { _, enabled in
            invitations.configure(active: scenePhase == .active && model.phase == .ready, remoteEnabled: enabled)
            if !enabled { pendingInvitationHandoff = nil }
        }
        .onChange(of: invitations.tapReady) { _, _ in
            guard scenePhase == .active, remoteEnabled, model.phase == .ready, let handoff = invitations.prepareTapHandoff() else { return }
            pendingInvitationHandoff = handoff
            if invitationRecoveryPresented { invitationRecoveryPresented = false }
            else { finishInvitationHandoff() }
        }
        #endif
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

    private var remoteLifecycleKey: String {
        "\(model.phase)|\(model.origin)|\(model.userId ?? 0)|\(scenePhase)|\(remoteEnabled)|\(invitations.authorizationGeneration)"
    }

    #if os(iOS)
    private func finishInvitationHandoff() {
        defer { pendingInvitationHandoff = nil }
        guard scenePhase == .active, remoteEnabled, model.phase == .ready,
              let handoff = pendingInvitationHandoff, let lookup = invitations.validateTapHandoff(handoff) else { return }
        let existing = remoteClient.devices.first { $0.id == lookup.receiverID }
        remoteClient.select(CinemaRemoteDevice(receiverID: lookup.receiverID, name: existing?.name ?? "Paired screen", platform: existing?.platform ?? "tv", target: lookup.target, available: true, busy: false, paired: true))
    }
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
