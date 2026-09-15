import SwiftUI

enum HomeTab: Hashable {
    case home
    case libraries
    case liveTv
    case recordings
    case libraryChannels
    case search
    case downloads
    case settings
}

struct HomeView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @State private var selectedTab: HomeTab
    @ObservedObject private var dvr = DvrController.shared

    init(initialTab: HomeTab = .home) {
        _selectedTab = State(initialValue: initialTab)
    }

    var body: some View {
        #if os(iOS)
        if #available(iOS 18.0, *) {
            iOSTabs.tabViewStyle(.sidebarAdaptable)
        } else {
            iOSTabs
        }
        #else
        // Detail destinations replace the entire tab shell on television.
        // That keeps the Home tab from remaining visibly selected while an
        // episode or movie detail is on screen.
        NavigationStack {
            tvTabs
                .appDestinations()
        }
        #endif
    }

    #if os(iOS)
    private var iOSTabs: some View {
        TabView(selection: $selectedTab) {
            NavigationStack {
                if model.phase == .loading {
                    ProgressView().tint(Palette.accent)
                } else if model.phase == .reconnectFailed {
                    ReconnectView()
                } else {
                    HomeDashboard()
                        .appDestinations()
                }
            }
            .tabItem { Label("Home", systemImage: "house") }
            .tag(HomeTab.home)

            NavigationStack {
                LibrariesDashboard()
                    .appDestinations()
            }
            .tabItem { Label("Libraries", systemImage: "rectangle.stack") }
            .tag(HomeTab.libraries)

            NavigationStack { LiveTvView(onLeave: { selectedTab = .home }) }
                .tabItem { Label("Live TV", systemImage: "tv") }
                .tag(HomeTab.liveTv)

            NavigationStack { DvrRecordingsRootView().appDestinations() }
                .tabItem { Label("Recordings", systemImage: "record.circle") }
                .badge(dvr.overview?.counts.attention ?? 0)
                .tag(HomeTab.recordings)

            NavigationStack { LibraryChannelsView().appDestinations() }
                .tabItem { Label("Channels", systemImage: "play.rectangle.on.rectangle") }
                .tag(HomeTab.libraryChannels)

            NavigationStack {
                SearchView()
                    .appDestinations()
            }
            .tabItem { Label("Search", systemImage: "magnifyingglass") }
            .tag(HomeTab.search)

            NavigationStack {
                DownloadsView()
            }
            .tabItem { Label("Downloads", systemImage: "arrow.down.circle") }
            .tag(HomeTab.downloads)

            NavigationStack {
                SettingsView()
            }
            .tabItem { Label("Settings", systemImage: "gearshape") }
            .tag(HomeTab.settings)
        }
        .tint(Palette.accent)
        .task(id: dvrObservationIdentity) {
            guard scenePhase == .active, model.phase == .ready else { return }
            await dvr.observe(origin: model.origin, token: Session.shared.token,
                              highFrequency: selectedTab == .liveTv || selectedTab == .recordings)
        }
        .task {
            if model.phase == .ready && model.homeLoading {
                await model.loadHome()
            }
        }
        .onChange(of: model.phase) { _, phase in
            guard phase == .ready else { return }
            selectedTab = .home
            if model.homeLoading {
                Task { await model.loadHome() }
            }
        }
        .onChange(of: scenePhase) { _, phase in
            // Coming back to a foregrounded app should not show yesterday's
            // Continue Watching. An Apple TV in particular is suspended rather
            // than quit, so before this the tvOS dashboard only ever refreshed
            // by relaunching the app. `loadHome` coalesces overlapping refreshes
            // and no longer raises a spinner over content, so this is free.
            guard phase == .active, model.phase == .ready else { return }
            Task { await model.loadHome() }
        }
        .onReceive(NotificationCenter.default.publisher(for: .makeLibraryChannelFromItem)) { _ in
            selectedTab = .libraryChannels
        }
        // A reminder's *Watch* action names a channel; Live TV is the only tab
        // that can tune one, and it reads the channel from the same notice.
        .onReceive(NotificationCenter.default.publisher(for: .plurxReminderWatch)) { _ in
            selectedTab = .liveTv
        }
    }
    #else
    private var tvTabs: some View {
        TabView(selection: $selectedTab) {
            HomeDashboard()
                .tabItem { Label("Home", systemImage: "house") }
                .tag(HomeTab.home)

            LibrariesDashboard()
                .tabItem { Label("Libraries", systemImage: "rectangle.stack") }
                .tag(HomeTab.libraries)

            LiveTvView(onLeave: { selectedTab = .home })
                .tabItem { Label("Live TV", systemImage: "tv") }
                .tag(HomeTab.liveTv)

            DvrRecordingsRootView()
                .tabItem { Label(recordingsTabLabel, systemImage: "record.circle") }
                .tag(HomeTab.recordings)

            LibraryChannelsView()
                .tabItem { Label("Channels", systemImage: "play.rectangle.on.rectangle") }
                .tag(HomeTab.libraryChannels)

            SearchView()
                .tabItem { Label("Search", systemImage: "magnifyingglass") }
                .tag(HomeTab.search)

            SettingsView()
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(HomeTab.settings)
        }
        .tint(Palette.accent)
        .task(id: dvrObservationIdentity) {
            guard scenePhase == .active else { return }
            await dvr.observe(origin: model.origin, token: Session.shared.token,
                              highFrequency: selectedTab == .liveTv || selectedTab == .recordings)
        }
        .task { if model.homeLoading { await model.loadHome() } }
        .onChange(of: scenePhase) { _, phase in
            // Coming back to a foregrounded app should not show yesterday's
            // Continue Watching. An Apple TV in particular is suspended rather
            // than quit, so before this the tvOS dashboard only ever refreshed
            // by relaunching the app. `loadHome` coalesces overlapping refreshes
            // and no longer raises a spinner over content, so this is free.
            guard phase == .active else { return }
            Task { await model.loadHome() }
        }
    }
    #endif

    private var dvrObservationIdentity: String {
        "\(model.origin)|\(Session.shared.token ?? "signed-out")|\(scenePhase == .active)|\(model.phase)|\(selectedTab)"
    }

    private var recordingsTabLabel: String {
        guard let count = dvr.overview?.counts.attention, count > 0 else { return "Recordings" }
        return "Recordings · \(count)"
    }
}

private struct AppDestinations: ViewModifier {
    func body(content: Content) -> some View {
        content.navigationDestination(for: Route.self) { route in
            switch route {
            case .collection(let collection): LibraryView(collection: collection)
            case .item(let id): DetailView(itemId: id)
            }
        }
    }
}

extension View {
    fileprivate func appDestinations() -> some View { modifier(AppDestinations()) }
}

private struct HomeDashboard: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass

    @ObservedObject private var dvr = DvrController.shared

    private var recentItems: [Item] {
        let recordings = Set(model.libraries.filter { $0.kind == "recordings" }.map(\.id))
        return (model.hubs.recentlyAdded ?? []).filter { !recordings.contains($0.libraryId ?? -1) }
    }
    @State private var showAllContinuing = false

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: dashboardSpacing) {
                #if os(iOS)
                homeHeader
                #endif
                if model.homeLoading {
                    ProgressView().tint(Palette.accent)
                        .frame(maxWidth: .infinity).padding(.top, 80)
                } else if let error = model.homeError {
                    ContentUnavailableView(
                        "Server unavailable",
                        systemImage: "exclamationmark.triangle",
                        description: Text(error)
                    )
                } else {
                    homeContent
                }
            }
            .padding(.bottom, 36)
        }
        .background(Palette.bg.ignoresSafeArea())
        .task { await dvr.loadLibrary() }
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await model.loadHome() }
        #endif
    }

    private var dashboardSpacing: CGFloat {
        #if os(tvOS)
        return 12
        #else
        return 6
        #endif
    }

    #if os(iOS)
    private var homeHeader: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("Home")
                .font(.system(size: 32, weight: .bold, design: .monospaced))
                .foregroundColor(Palette.accent)
            Spacer()
            NavigationLink { SearchView().appDestinations() } label: {
                Image(systemName: "magnifyingglass").frame(width: 44, height: 44)
            }.accessibilityLabel("Search all libraries")
            Menu {
                NavigationLink { LibraryChannelsView().appDestinations() } label: { Label("Library channels", systemImage: "play.rectangle.on.rectangle") }
                NavigationLink { DownloadsView() } label: { Label("Downloads", systemImage: "arrow.down.circle") }
                NavigationLink { SettingsView() } label: { Label("Settings", systemImage: "gearshape") }
            } label: {
                Image(systemName: "ellipsis.circle").frame(width: 44, height: 44)
            }.accessibilityLabel("More destinations")
            if let username = model.username {
                Text(username)
                    .font(.system(.caption, design: .monospaced))
                    .foregroundColor(Palette.muted)
            }
        }
        .padding(.horizontal, screenHPad)
        .padding(.top, 16)
        .padding(.bottom, 8)
    }

    #endif

    @ViewBuilder
    private var homeContent: some View {
        if let label = dvr.indicatorLabel {
            NavigationLink { DvrCaptureActivityView() } label: {
                Label(label, systemImage: "record.circle")
                    .font(.headline)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(16)
                    .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
            #else
            .buttonStyle(.plain)
            #endif
            .padding(.horizontal, screenHPad)
        }
        MediaRow(title: "Recently Added", items: recentItems)
        if let continuing = model.hubs.continueWatching, !continuing.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                Text("Continue Watching").font(.headline)
                ForEach(Array(continuing.prefix(showAllContinuing ? continuing.count : 3))) { item in
                    NavigationLink(value: Route.item(item.id)) {
                        HStack(spacing: 14) {
                            AuthImage(path: item.poster ?? item.backdrop).frame(width: 48, height: 60).clipped().cornerRadius(6)
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.showTitle ?? item.title).font(.headline)
                                Text([item.seasonNumber.map { "S\($0)" }, item.episodeNumber.map { "E\($0)" }, item.kind == "episode" ? item.title : nil].compactMap { $0 }.joined(separator: " · ")).font(.caption).foregroundStyle(Palette.muted)
                                if let watch = item.watch, let duration = watch.durationMs, duration > 0 {
                                    ProgressView(value: min(1, Double(watch.positionMs ?? 0) / Double(duration))).tint(Palette.muted).frame(maxWidth: 280)
                                }
                            }
                            Spacer()
                            Image(systemName: "play.fill")
                        }.frame(maxWidth: .infinity, alignment: .leading)
                    }
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                    #else
                    .buttonStyle(.plain)
                    #endif
                }
                if continuing.count > 3 { Button(showAllContinuing ? "Show fewer" : "More in progress (\(continuing.count - 3))") { showAllContinuing.toggle() } }
            }.padding(.horizontal, screenHPad).padding(.vertical, 18)
        }
        if !dvr.library.isEmpty {
            VStack(alignment: .leading, spacing: 14) {
                NavigationLink { DvrRecordingsRootView().appDestinations() } label: { Text("Recently Recorded").font(.headline) }
                ForEach(Array(dvr.library.prefix(3))) { recording in
                    NavigationLink { DvrRecordingDetailView(recordingId: recording.id).appDestinations() } label: {
                        HStack {
                            Image(systemName: "record.circle")
                            VStack(alignment: .leading, spacing: 4) {
                                Text(recording.title).font(.headline)
                                Text(Date(timeIntervalSince1970: TimeInterval(recording.airingStart)), style: .date).font(.caption).foregroundStyle(Palette.muted)
                            }
                            Spacer()
                            Image(systemName: "chevron.right")
                        }
                    }
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                    #else
                    .buttonStyle(.plain)
                    #endif
                }
            }.padding(.horizontal, screenHPad).padding(.vertical, 18)
        }
        if recentItems.isEmpty && (model.hubs.continueWatching ?? []).isEmpty && dvr.library.isEmpty {
            Text("Your library additions and recordings will appear here.").foregroundStyle(Palette.muted).padding(screenHPad)
        }
    }

}

enum HomeLayoutPolicy {
    static let continueWatchingCopyStyle: LandscapeCardCopyStyle = .accentPanel
    #if os(iOS)
    static let topLevelTabs = ["Home", "Libraries", "Search", "Downloads", "Settings"]
    static let offlineLaunchTab: HomeTab = .downloads
    #else
    static let topLevelTabs = ["Home", "Libraries", "Search", "Settings"]
    #endif
    static let showsLibraryShelvesOnHome = false

    static func continueWatchingShelfItems(_ items: [Item]) -> [Item] {
        items
    }

    static let usesFeaturedHero = false
}

private struct LibrariesDashboard: View {
    @EnvironmentObject var model: AppModel

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                if model.homeLoading {
                    ProgressView()
                        .tint(Palette.accent)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 80)
                } else if let error = model.homeError {
                    ContentUnavailableView(
                        "Server unavailable",
                        systemImage: "exclamationmark.triangle",
                        description: Text(error)
                    )
                } else if model.libraries.isEmpty {
                    ContentUnavailableView(
                        "Your library is empty",
                        systemImage: "rectangle.stack",
                        description: Text("Add a library in the Cinema web app, then pull to refresh.")
                    )
                    .frame(maxWidth: .infinity)
                    .padding(.top, 80)
                } else {
                    libraryHeading
                    ForEach(model.libraryCollections()) { collection in
                        MediaRow(
                            title: collection.title,
                            items: model.previewItems(for: collection),
                            collection: collection,
                            destination: collection
                        )
                    }
                }
            }
            .padding(.bottom, 36)
        }
        .background(Palette.bg.ignoresSafeArea())
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await model.loadHome() }
        #endif
    }

    private var libraryHeading: some View {
        HStack {
            Text("Libraries")
                #if os(tvOS)
                .font(.title3.weight(.semibold))
                #else
                .font(.headline.weight(.semibold))
                #endif
                .foregroundColor(Palette.onBg)
            Spacer()
            Picker("Group by", selection: Binding(
                get: { model.libraryGrouping },
                set: { model.setLibraryGrouping($0) }
            )) {
                ForEach(LibraryGrouping.allCases) { grouping in
                    Text(grouping.label).tag(grouping)
                }
            }
            #if os(iOS)
            .pickerStyle(.segmented)
            .frame(maxWidth: 280)
            #endif
        }
        .padding(.horizontal, screenHPad)
        .padding(.top, 18)
    }
}

private struct EmptyLibraryCategory: View {
    let title: String
    var body: some View {
        ContentUnavailableView(
            "No \(title)",
            systemImage: "rectangle.stack",
            description: Text("No matching library shares are configured on this server.")
        )
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle(title)
    }
}

private func episodeSubtitleForHero(_ item: Item) -> String {
    var parts: [String] = []
    if let season = item.seasonNumber, let episode = item.episodeNumber {
        parts.append("S\(season) E\(episode)")
    }
    parts.append(item.title)
    return parts.joined(separator: "  ")
}
