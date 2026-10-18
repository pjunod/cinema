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

extension HomeTab {
    /// The one rule for which tabs exist: Live TV, Recordings and Library
    /// channels are top-level tabs only while the server has them switched on.
    /// The screens themselves are not gated — reached any other way, they show
    /// their own disabled state.
    func isVisible(_ features: ServerFeatures) -> Bool {
        switch self {
        case .liveTv: return features.liveTv
        case .recordings: return features.recordings
        case .libraryChannels: return features.libraryChannels
        case .home, .libraries, .search, .downloads, .settings: return true
        }
    }

    /// Where selection lands when the selected tab is not shown: an admin
    /// switched it off, or a deep link or remote command asked for it.
    func visibleSelection(_ features: ServerFeatures) -> HomeTab {
        isVisible(features) ? self : .home
    }
}

struct HomeView: View {
    @EnvironmentObject var model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    private var selectedTab: HomeTab {
        get { remoteNavigation.selectedTab }
        nonmutating set { remoteNavigation.selectedTab = newValue; remoteNavigation.synchronizeRoute() }
    }
    private let initialTab: HomeTab
    @State private var initializedShell = false
    @ObservedObject private var dvr = DvrController.shared

    init(initialTab: HomeTab = .home) {
        self.initialTab = initialTab
    }

    var body: some View {
        shell
            .onAppear {
                if !initializedShell { selectedTab = initialTab; initializedShell = true }
                keepSelectionVisible()
                remoteNavigation.synchronizeRoute()
            }
            .onChange(of: remoteNavigation.selectedTab) { _, _ in
                keepSelectionVisible()
                remoteNavigation.synchronizeRoute()
            }
            .onChange(of: model.features) { _, _ in keepSelectionVisible() }
            .onChange(of: remoteNavigation.paths) { _, _ in remoteNavigation.synchronizeRoute() }
    }

    /// The single owner of "a hidden tab is never selected", for both shells.
    private func keepSelectionVisible() {
        let visible = selectedTab.visibleSelection(model.features)
        if visible != selectedTab { selectedTab = visible }
    }

    /// Switches only to a tab that is shown; a request for a hidden one is
    /// ignored rather than flashed and then snapped back to Home.
    private func select(_ tab: HomeTab) {
        guard tab.isVisible(model.features) else { return }
        selectedTab = tab
    }

    @ViewBuilder
    private var shell: some View {
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
        NavigationStack(path: pathBinding(remoteNavigation.selectedTab)) {
            tvTabs
                .appDestinations()
        }
        #endif
    }

    #if os(iOS)
    private var iOSTabs: some View {
        TabView(selection: $remoteNavigation.selectedTab) {
            NavigationStack(path: pathBinding(.home)) {
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

            NavigationStack(path: pathBinding(.libraries)) {
                LibrariesDashboard()
                    .appDestinations()
            }
            .tabItem { Label("Libraries", systemImage: "rectangle.stack") }
            .tag(HomeTab.libraries)

            if HomeTab.liveTv.isVisible(model.features) {
                NavigationStack { LiveTvView(onLeave: { selectedTab = .home }) }
                    .tabItem { Label("Live TV", systemImage: "tv") }
                    .tag(HomeTab.liveTv)
            }

            if HomeTab.recordings.isVisible(model.features) {
                NavigationStack { DvrRecordingsRootView().appDestinations() }
                    .tabItem { Label("Recordings", systemImage: "record.circle") }
                    .badge(dvr.overview?.counts.attention ?? 0)
                    .tag(HomeTab.recordings)
            }

            if HomeTab.libraryChannels.isVisible(model.features) {
                NavigationStack { LibraryChannelsView().appDestinations() }
                    .tabItem { Label("Channels", systemImage: "play.rectangle.on.rectangle") }
                    .tag(HomeTab.libraryChannels)
            }

            NavigationStack(path: pathBinding(.search)) {
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
            await dvr.observe(origin: model.origin, token: Session.shared.credentials.token,
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
            select(.libraryChannels)
        }
        // A reminder's *Watch* action names a channel; Live TV is the only tab
        // that can tune one, and it reads the channel from the same notice.
        .onReceive(NotificationCenter.default.publisher(for: .plurxReminderWatch)) { _ in
            select(.liveTv)
        }
    }
    #else
    private var tvTabs: some View {
        TabView(selection: $remoteNavigation.selectedTab) {
            HomeDashboard()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Home")
                .tabItem { Label("Home", systemImage: "house") }
                .tag(HomeTab.home)

            LibrariesDashboard()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Libraries")
                .tabItem { Label("Libraries", systemImage: "rectangle.stack") }
                .tag(HomeTab.libraries)

            if HomeTab.liveTv.isVisible(model.features) {
                LiveTvView(onLeave: { selectedTab = .home })
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Live TV")
                    .tabItem { Label("Live TV", systemImage: "tv") }
                    .tag(HomeTab.liveTv)
            }

            if HomeTab.recordings.isVisible(model.features) {
                DvrRecordingsRootView()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Recordings")
                    .tabItem { Label(recordingsTabLabel, systemImage: "record.circle") }
                    .tag(HomeTab.recordings)
            }

            if HomeTab.libraryChannels.isVisible(model.features) {
                LibraryChannelsView()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Channels")
                    .tabItem { Label("Channels", systemImage: "play.rectangle.on.rectangle") }
                    .tag(HomeTab.libraryChannels)
            }

            SearchView()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Search")
                .tabItem { Label("Search", systemImage: "magnifyingglass") }
                .tag(HomeTab.search)

            SettingsView()
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("root-screen-Settings")
                .tabItem { Label("Settings", systemImage: "gearshape") }
                .tag(HomeTab.settings)
        }
        .tint(Palette.accent)
        .task(id: dvrObservationIdentity) {
            guard scenePhase == .active else { return }
            await dvr.observe(origin: model.origin, token: Session.shared.credentials.token,
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

    private func pathBinding(_ tab: HomeTab) -> Binding<[Route]> {
        Binding(get: { remoteNavigation.paths[tab] ?? [] }, set: { remoteNavigation.paths[tab] = $0; remoteNavigation.synchronizeRoute() })
    }

    private var dvrObservationIdentity: String {
        "\(model.origin)|\(Session.shared.credentials.token ?? "signed-out")|\(scenePhase == .active)|\(model.phase)|\(selectedTab)"
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
            case .collection(let collection): LibraryView(collection: collection).remoteScope(RemoteNavigationCoordinator.routeScope(route))
            case .item(let id): DetailView(itemId: id).remoteScope(RemoteNavigationCoordinator.routeScope(route))
            case .sharedLibraries: SharedLibrariesView().remoteScope(RemoteNavigationCoordinator.routeScope(route))
            case .sharedLibrary(let library, let parent, let title): SharedLibraryItemsView(library: library, parent: parent, title: title).remoteScope(RemoteNavigationCoordinator.routeScope(route))
            case .sharedItem(let reference, let library): SharedLibraryDetailView(reference: reference, library: library).remoteScope(RemoteNavigationCoordinator.routeScope(route))
            }
        }
    }
}

extension View {
    fileprivate func appDestinations() -> some View { modifier(AppDestinations()) }
}

private struct HomeRecoveryRow: View {
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let error: String
    let scope: String
    var returnFocus: String?
    var body: some View {
        HStack(spacing: 24) {
            Text("Couldn't refresh: \(error)").foregroundStyle(Palette.muted)
                .lineLimit(nil).fixedSize(horizontal: false, vertical: true)
            Button("Retry") { navigation.performPhysicalAction(retry) }
                .shelfActionButtonStyle()
                .accessibilityIdentifier(scope + ":retry")
                .remoteControl(scope + ":retry", label: "Retry", observesPhysicalActivation: false, activate: retry)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(.horizontal, screenHPad)
        #if os(tvOS)
        .focusSection()
        #endif
    }
    private func retry() {
        // The recovery row is removed as soon as loading starts. Move its
        // focus to surviving content before that removal, not after the
        // network response when the viewer may have already moved elsewhere.
        if let returnFocus { navigation.focusControl(scope: scope, key: returnFocus) }
        Task { await model.loadHome() }
    }
}

private struct HomeDashboard: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #if os(tvOS)
    @AppStorage("plurx.tvHomeLayout") private var tvHomeLayoutRaw = TvHomeLayout.classic.rawValue

    private var theaterFeature: TvTheaterFeature? {
        TvHomeLayoutPolicy.feature(
            layout: TvHomeLayout(rawValue: tvHomeLayoutRaw) ?? .classic,
            continueWatching: model.hubs.continueWatching ?? [],
            recentlyAdded: model.hubs.recentlyAdded ?? []
        )
    }
    #endif

    private var featured: Item? {
        (model.hubs.continueWatching ?? []).first
            ?? (model.hubs.nextUp ?? []).first
            ?? (model.hubs.recentlyAdded ?? []).first
    }

    var body: some View {
        ScrollViewReader { proxy in
        ScrollView {
            LazyVStack(alignment: .leading, spacing: dashboardSpacing) {
                #if os(iOS)
                homeHeader
                #endif
                #if os(tvOS)
                HStack {
                    Button("Libraries") { remoteNavigation.selectedTab = .libraries; remoteNavigation.synchronizeRoute() }
                        .remoteControl("tab:libraries", label: "Libraries") { remoteNavigation.selectedTab = .libraries; remoteNavigation.synchronizeRoute() }
                    Button("Search") { remoteNavigation.selectedTab = .search; remoteNavigation.synchronizeRoute() }
                        .remoteControl("tab:search", label: "Search") { remoteNavigation.selectedTab = .search; remoteNavigation.synchronizeRoute() }
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                .padding(.horizontal, screenHPad)
                #endif
                if let error = model.homeError {
                    HomeRecoveryRow(error: error, scope: "home", returnFocus: remoteKeys.first { $0.hasPrefix("shelf:") } ?? "tab:libraries")
                }
                if model.homeLoading && featured == nil {
                    ProgressView().tint(Palette.accent)
                        .frame(maxWidth: .infinity).padding(.top, 80)
                } else {
                    homeContent
                }
            }
            .padding(.bottom, 36)
        }
        .background(Palette.bg.ignoresSafeArea())
        .remoteScope("home")
        .onAppear { updateRemoteOrder() }
        .onChange(of: remoteKeys) { _, _ in updateRemoteOrder() }
        .onChange(of: remoteNavigation.focusRequestRevision) { _, _ in
            let key = remoteNavigation.requestedFocus
            guard remoteNavigation.activeScope == "home", let key, key.hasPrefix("shelf:"),
                  let shelf = key.split(separator: ":").dropFirst().first else { return }
            proxy.scrollTo("shelf:" + shelf, anchor: .center)
        }
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await model.loadHome() }
        #endif
        }
    }

    private var remoteKeys: [String] {
        let shelves = [HomeLayoutPolicy.continueWatchingShelfItems(model.hubs.continueWatching ?? []), model.hubs.nextUp ?? [], model.hubs.recentlyAdded ?? []]
        #if os(tvOS)
        let tabs = ["tab:libraries", "tab:search"]
        #else
        let tabs: [String] = []
        #endif
        return tabs + (model.homeError == nil ? [] : ["home:retry"]) + shelves.enumerated().flatMap { index, items in
            items.map { "shelf:\(index):item:\($0.id)" }
        } + model.comingSoon.filter { $0.itemId != nil }.map { "shelf:3:comingsoon:" + $0.id }
    }
    private func updateRemoteOrder() { remoteNavigation.setOrder(scope: "home", keys: remoteKeys, columns: 1) }

    private var dashboardSpacing: CGFloat {
        #if os(tvOS)
        return 12
        #else
        return 6
        #endif
    }

    private var homeHeader: some View {
        HStack(alignment: .firstTextBaseline) {
            Text("cinema")
                .font(.system(size: 32, weight: .bold, design: .monospaced))
                .foregroundColor(Palette.accent)
            Spacer()
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

    @ViewBuilder
    private var homeContent: some View {
        #if os(tvOS)
        if let theaterFeature {
            TvTheaterHero(feature: theaterFeature)
                .padding(.bottom, 12)
        }
        #endif
        if HomeLayoutPolicy.usesFeaturedHero, let featured {
            FeaturedHero(item: featured, compact: horizontalSizeClass == .compact)
                #if os(tvOS)
                .padding(.horizontal, screenHPad)
                #endif
                .padding(.bottom, 12)
        }

        MediaRow(
            title: "Continue Watching",
            remoteShelf: "shelf:0",
            items: continueWatchingItems,
            style: .landscape,
            landscapeCopyStyle: HomeLayoutPolicy.continueWatchingCopyStyle
        )
        .id("shelf:0")
        MediaRow(
            title: "Next Up",
            remoteShelf: "shelf:1",
            items: model.hubs.nextUp ?? [],
            style: .landscape
        )
        .id("shelf:1")
        MediaRow(
            title: "Recently Added",
            remoteShelf: "shelf:2",
            items: model.hubs.recentlyAdded ?? []
        )
        .id("shelf:2")
        ComingSoonRow(entries: model.comingSoon)
            .id("shelf:3")

        if featured == nil,
           (model.hubs.nextUp ?? []).isEmpty,
           model.comingSoon.isEmpty {
            ContentUnavailableView(
                "Nothing new yet",
                systemImage: "house",
                description: Text("Continue watching, recently added titles, and upcoming releases will appear here.")
            )
            .frame(maxWidth: .infinity)
            .padding(.top, 80)
        }
    }

    private var continueWatchingItems: [Item] {
        #if os(tvOS)
        return TvHomeLayoutPolicy.shelfItems(
            model.hubs.continueWatching ?? [], feature: theaterFeature
        )
        #else
        return HomeLayoutPolicy.continueWatchingShelfItems(model.hubs.continueWatching ?? [])
        #endif
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
        usesFeaturedHero ? Array(items.dropFirst()) : items
    }

    #if os(tvOS)
    static let usesFeaturedHero = false
    #else
    static let usesFeaturedHero = true
    #endif
}

private struct LibrariesDashboard: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    @State private var groupingMenu = false

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                if model.features.sharedLibraries {
                    SharedLibrariesEntry().padding(.horizontal, screenHPad).padding(.top, 12)
                }
                if let error = model.homeError {
                    HomeRecoveryRow(error: error, scope: "libraries", returnFocus: "libraries:grouping")
                }
                if model.homeLoading && model.libraries.isEmpty {
                    ProgressView()
                        .tint(Palette.accent)
                        .frame(maxWidth: .infinity)
                        .padding(.top, 80)
                } else if model.libraries.isEmpty {
                    ContentUnavailableView(
                        "Your library is empty",
                        systemImage: "rectangle.stack",
                        description: Text("Add a library in the Cinema web app, then refresh.")
                    )
                    .frame(maxWidth: .infinity)
                    .padding(.top, 80)
                    Button("Refresh") { Task { await model.loadHome() } }
                        .shelfActionButtonStyle()
                        .remoteControl("libraries:refresh", label: "Refresh libraries") { Task { await model.loadHome() } }
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
        .remoteScope("libraries")
        .remoteChoicePresentation(isPresented: $groupingMenu, scope: "libraries:grouping") {
            if groupingMenu {
                RemoteChoicePanel(scope: "libraries:grouping", title: "Group by", choices: LibraryGrouping.allCases.map { grouping in
                    RemoteChoice(id: grouping.rawValue, label: grouping.label, selected: model.libraryGrouping == grouping) { model.setLibraryGrouping(grouping) }
                })
            }
        }
        .onAppear { updateRemoteOrder() }
        .onChange(of: model.libraryCollections().map(\.id)) { _, _ in updateRemoteOrder() }
        .onChange(of: model.features.sharedLibraries) { _, _ in updateRemoteOrder() }
        .onChange(of: model.homeError) { _, _ in updateRemoteOrder() }
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        .refreshable { await model.loadHome() }
        #endif
    }

    private func updateRemoteOrder() {
        #if os(tvOS)
        let grouping = ["libraries:grouping"]
        #else
        let grouping: [String] = []
        #endif
        // A Cinema remote must not be offered an entry that is not on screen.
        let shared = model.features.sharedLibraries ? ["libraries:shared"] : []
        remoteNavigation.setOrder(scope: "libraries", keys: grouping + shared + (model.homeError == nil ? [] : ["libraries:retry"]) + (model.libraries.isEmpty ? ["libraries:refresh"] : []) + model.libraryCollections().map { "collection:" + $0.id }, columns: 1)
    }
    private func openGrouping() {
        remoteNavigation.openModal(scope: "libraries:grouping", opener: "libraries:grouping") { groupingMenu = false }
        groupingMenu = remoteNavigation.activeScope == "libraries:grouping"
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
            #if os(tvOS)
            Button("Group by") { openGrouping() }
                .shelfActionButtonStyle()
                .remoteControl("libraries:grouping", label: "Group by") { openGrouping() }
            Spacer()
            #else
            Spacer()
            Picker("Group by", selection: Binding(get: { model.libraryGrouping }, set: { model.setLibraryGrouping($0) })) {
                ForEach(LibraryGrouping.allCases) { grouping in Text(grouping.label).tag(grouping) }
            }
            .pickerStyle(.segmented)
            .frame(maxWidth: 280)
            #endif

        }
        .padding(.horizontal, screenHPad)
        .padding(.top, 18)
    }
}

private struct FeaturedHero: View {
    let item: Item
    let compact: Bool

    var body: some View {
        NavigationLink(value: Route.item(item.id)) {
            #if os(iOS)
            if compact {
                compactHero
            } else {
                expansiveHero
            }
            #else
            expansiveHero
            #endif
        }
        .featuredButtonStyle()
        .accessibilityLabel("\(heroAction) \(item.title)")
        #if os(iOS)
        // The outer inset is applied to the link. The card itself separately
        // accepts that finite width before its loaded artwork is clipped.
        .modifier(IOSHomeHeroLayout(compact: compact))
        #endif
    }

    #if os(iOS)
    private var compactHero: some View {
        ZStack(alignment: .bottomLeading) {
            AuthImage(
                path: item.backdrop ?? item.poster,
                targetSize: CGSize(
                    width: 430,
                    height: HomeHeroMetrics.compactHeight
                )
            )
            .modifier(IOSHomeHeroArtworkLayout())

            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0.18),
                    .init(color: .black.opacity(0.24), location: 0.5),
                    .init(color: Palette.bg.opacity(0.92), location: 1)
                ],
                startPoint: .top,
                endPoint: .bottom
            )

            VStack(alignment: .leading, spacing: 7) {
                Text("CONTINUE")
                    .font(.system(size: 10, weight: .bold, design: .rounded))
                    .tracking(1.7)
                    .foregroundStyle(Palette.accent)

                Text(item.showTitle ?? item.title)
                    .font(.system(size: 25, weight: .heavy, design: .rounded))
                    .foregroundStyle(.white)
                    .lineLimit(1)
                    .shadow(color: .black.opacity(0.6), radius: 8, y: 2)

                if item.showTitle != nil {
                    Text(episodeSubtitleForHero(item))
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.white.opacity(0.78))
                        .lineLimit(1)
                }

                HStack(spacing: 12) {
                    ForEach(compactFacts, id: \.self) { fact in
                        Text(fact)
                    }
                    if let resolutionBadge {
                        IOSWebMediaBadge(badge: resolutionBadge)
                    }
                }
                .font(.system(size: 12, weight: .medium, design: .rounded))
                .foregroundStyle(.white.opacity(0.7))

                if heroProgress > 0 {
                    GeometryReader { geometry in
                        ZStack(alignment: .leading) {
                            Capsule().fill(.white.opacity(0.2))
                            Capsule()
                                .fill(Palette.accent)
                                .frame(width: geometry.size.width * heroProgress)
                        }
                    }
                    .frame(height: 3)
                }

                HStack(spacing: 10) {
                    Label(heroAction, systemImage: "play.fill")
                        .font(.system(size: 13, weight: .bold, design: .rounded))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 14)
                        .frame(height: 38)
                        .background(Palette.accent, in: Capsule())

                    if let remaining = continueWatchingTimeRemaining(item) {
                        Text(remaining)
                            .font(.system(size: 11, weight: .medium, design: .rounded))
                            .foregroundStyle(.white.opacity(0.58))
                    }
                }
            }
            .padding(16)
        }
        .modifier(IOSHomeHeroCardLayout())
    }

    private var compactFacts: [String] {
        var facts: [String] = []
        if let year = item.year { facts.append(String(year)) }
        if let runtime = item.runtimeMs, runtime > 0 {
            facts.append(DetailView.compactRuntimeLabel(runtime))
        }
        return facts
    }

    private var resolutionBadge: ItemMetadataBadge? {
        guard let resolution = resolutionLabel(item.resolution) else { return nil }
        return ItemMetadataBadge(
            kind: .resolution,
            symbol: "",
            mark: resolution,
            accessibilityLabel: resolution
        )
    }

    private var heroProgress: CGFloat {
        CGFloat(progressFraction(item.watch, runtimeMs: item.runtimeMs))
    }
    #endif

    private var expansiveHero: some View {
        ZStack(alignment: .bottomLeading) {
            AuthImage(path: item.backdrop ?? item.poster)
                .frame(maxWidth: .infinity)
                .frame(height: heroHeight)
                .clipped()
            LinearGradient(
                colors: [.clear, Palette.bg.opacity(0.18), Palette.bg],
                startPoint: .top,
                endPoint: .bottom
            )
            VStack(alignment: .leading, spacing: 8) {
                Text(item.showTitle ?? item.title)
                    #if os(tvOS)
                    .font(.system(size: 52, weight: .bold))
                    #else
                    .font(compact ? .title.bold() : .largeTitle.bold())
                    #endif
                    .foregroundColor(.white)
                    .lineLimit(2)
                if item.showTitle != nil {
                    Text(episodeSubtitleForHero(item))
                        .font(.headline)
                        .foregroundColor(.white.opacity(0.82))
                        .lineLimit(1)
                }
                if !heroMetadata.isEmpty {
                    Text(heroMetadata)
                        .font(.system(.callout, design: .monospaced).weight(.semibold))
                        .foregroundColor(.white.opacity(0.72))
                        .lineLimit(1)
                }
                Label(heroAction, systemImage: "play.fill")
                    .font(.system(.headline, design: .monospaced).weight(.bold))
                    .foregroundColor(.white)
                    .padding(.horizontal, 16).padding(.vertical, 10)
                    .background(Palette.accent, in: Capsule())
            }
            .padding(.horizontal, screenHPad)
            .padding(.bottom, 24)
        }
        .clipShape(RoundedRectangle(cornerRadius: heroCornerRadius, style: .continuous))
    }

    private var heroHeight: CGFloat {
        #if os(tvOS)
        return 470
        #else
        return compact ? 290 : 430
        #endif
    }

    private var heroCornerRadius: CGFloat {
        #if os(tvOS)
        return 24
        #else
        return 0
        #endif
    }

    private var heroMetadata: String {
        var parts: [String] = []
        if let year = item.year { parts.append(String(year)) }
        if let runtime = item.runtimeMs, runtime > 0 {
            let totalMinutes = runtime / 60_000
            let hours = totalMinutes / 60
            let minutes = totalMinutes % 60
            parts.append(hours > 0 ? "\(hours)h \(minutes)m" : "\(minutes)m")
        }
        if let resolution = resolutionLabel(item.resolution) { parts.append(resolution) }
        return parts.joined(separator: "   ·   ")
    }

    private var heroAction: String {
        progressFraction(item.watch, runtimeMs: item.runtimeMs) > 0 ? "Resume" : "View details"
    }
}

#if os(iOS)
enum HomeHeroMetrics {
    static let compactHeight: CGFloat = 238
    static let cornerRadius: CGFloat = 18
    static let horizontalInset = screenHPad
}

/// Aspect-fill artwork must not enlarge the stack that also positions the
/// title and controls. Crop it inside the card's proposed width first.
struct IOSHomeHeroArtworkLayout: ViewModifier {
    func body(content: Content) -> some View {
        GeometryReader { geometry in
            content
                .frame(width: geometry.size.width, height: geometry.size.height)
                .clipped()
        }
        .frame(height: HomeHeroMetrics.compactHeight)
    }
}

struct IOSHomeHeroCardLayout: ViewModifier {
    func body(content: Content) -> some View {
        GeometryReader { geometry in
            content
                .frame(
                    width: geometry.size.width,
                    height: HomeHeroMetrics.compactHeight
                )
                .clipShape(RoundedRectangle(
                    cornerRadius: HomeHeroMetrics.cornerRadius,
                    style: .continuous
                ))
                .overlay {
                    RoundedRectangle(
                        cornerRadius: HomeHeroMetrics.cornerRadius,
                        style: .continuous
                    )
                    .stroke(.white.opacity(0.08), lineWidth: 0.5)
                }
        }
        .frame(height: HomeHeroMetrics.compactHeight)
    }
}

struct IOSHomeHeroLayout: ViewModifier {
    let compact: Bool

    func body(content: Content) -> some View {
        content.padding(.horizontal, compact ? HomeHeroMetrics.horizontalInset : 0)
    }
}
#endif

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
