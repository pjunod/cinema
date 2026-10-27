import XCTest
import SwiftUI
import UIKit
@testable import plurx

@MainActor
final class MeridianTests: XCTestCase {
    func testLayoutUpgradeUnknownRestartAndIndependentPreferences() throws {
        let name = "MeridianTests.\(UUID())"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        let store = SettingsStore(defaults: defaults)
        XCTAssertEqual(store.layout, .standard)
        defaults.set("future", forKey: "plurx.layout")
        XCTAssertEqual(store.layout, .standard)
        store.theme = .terminal
        store.appearance = .light
        store.tvHomeLayout = .theater
        store.libraryGrouping = .share
        store.posterSize = .large
        store.layout = .meridian
        let restarted = SettingsStore(defaults: defaults)
        XCTAssertEqual(restarted.layout, .meridian)
        XCTAssertEqual(restarted.theme, .terminal)
        XCTAssertEqual(restarted.appearance, .light)
        XCTAssertEqual(restarted.tvHomeLayout, .theater)
        XCTAssertEqual(restarted.libraryGrouping, .share)
        XCTAssertEqual(restarted.posterSize, .large)
        store.theme = .meridian
        store.layout = .standard
        XCTAssertEqual(store.theme, .meridian)
        XCTAssertEqual(store.tvHomeLayout, .theater)
    }

    func testPersistentHostRetainsImplicitStackAndStateAcrossGeometryAndLayout() throws {
        let hostDisappearances = try checkPersistentHost(nativeBaseline: false)
        if #available(iOS 18.0, tvOS 18.0, *) {
            // SwiftUI's retained tabs do not promise an extra onDisappear for
            // a pushed detail in this secondary UIWindow. Match the native
            // control while requiring identical state and visibility below.
            let nativeDisappearances = try checkPersistentHost(nativeBaseline: true)
            XCTAssertEqual(hostDisappearances, nativeDisappearances,
                "The shell must preserve the native tab's appearance lifecycle")
        } else {
            XCTAssertEqual(hostDisappearances, 1, "The legacy container forwards disappearance")
        }
    }

    private func checkPersistentHost(nativeBaseline: Bool) throws -> Int {
        let state = NavigationDriver()
        let controller = UIHostingController(rootView: ImplicitDestinationHostFixture(driver: state, nativeBaseline: nativeBaseline))
        let window = try makeWindow()
        window.frame = CGRect(x: 0, y: 0, width: 1024, height: 768)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(0.6)
        if #available(iOS 18.0, tvOS 18.0, *) {
            XCTAssertNotNil(findTabs(controller))
        } else {
            XCTAssertNotNil(findLegacyDestination(controller))
        }
        let host = try XCTUnwrap(selectedDestination(controller))
        let navigation = try XCTUnwrap(findNavigation(host))
        state.push = true
        settle(0.6)
        XCTAssertEqual(navigation.viewControllers.count, 2, "Fixture must create an actual implicit native push")
        let detail = try XCTUnwrap(navigation.topViewController)
        let disappearances = state.disappearances
        for width in [390, 1200, 700, 390] {
            state.layout = state.layout == .standard ? .meridian : .standard
            window.frame.size.width = CGFloat(width)
            controller.view.setNeedsLayout(); controller.view.layoutIfNeeded()
            settle(0.6)
            XCTAssertTrue(selectedDestination(controller) === host)
            XCTAssertTrue(navigation.topViewController === detail)
            XCTAssertEqual(navigation.viewControllers.count, 2)
            XCTAssertEqual(state.disappearances, disappearances, "Changing chrome must not trigger playback-owner disappearance")
        }
        state.selection = .home; settle(0.6)
        if #available(iOS 18.0, tvOS 18.0, *) {
            XCTAssertEqual(findTabs(controller)?.selectedViewController?.tabBarItem.title, HomeTab.home.title)
        } else {
            XCTAssertEqual(findLegacyDestination(controller)?.activeTab, .home)
        }
        var ancestor: UIView? = host.view
        var hidden = host.view.window == nil
        while let view = ancestor {
            hidden = hidden || view.isHidden || view.alpha == 0
            ancestor = view.superview
        }
        XCTAssertTrue(hidden, "Inactive roots must be detached or hidden by their native container")
        let leavingDisappearances = state.disappearances - disappearances
        state.selection = .settings; settle(0.6)
        XCTAssertTrue(selectedDestination(controller) === host)
        XCTAssertTrue(navigation.topViewController === detail)
        navigation.popViewController(animated: false)
        settle(0.6)
        XCTAssertEqual(navigation.viewControllers.count, 1, "Back returns to the original implicit root")
        return leavingDisappearances
    }

    func testMeridianPanelIsScopedAndBackRestoresWithoutClearingAnyHistory() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.paths[.home] = [.item(7)]
        navigation.paths[.libraries] = [.item(8)]
        navigation.synchronizeRoute()
        let current = navigation.paths
        navigation.setViewerNavigationAvailable(true)
        navigation.setOrder(scope: navigation.rootScope, keys: ["content"], columns: 1)
        navigation.openModal(scope: RemoteNavigationCoordinator.viewerNavigationScope,
            opener: RemoteNavigationCoordinator.viewerNavigationOpener) {}
        let key = "choice:tab:Libraries"
        navigation.register(scope: RemoteNavigationCoordinator.viewerNavigationScope, key: key,
            entry: .init(id: UUID(), label: "Libraries", frame: CGRect(x: 0, y: 0, width: 100, height: 48)) {})
        navigation.enterChoicePanel(scope: RemoteNavigationCoordinator.viewerNavigationScope, keys: [key], selected: key)
        XCTAssertEqual(navigation.activeScope, RemoteNavigationCoordinator.viewerNavigationScope)
        XCTAssertNotEqual(navigation.dispatch(.select, context: navigation.context), .applied,
            "A requested key cannot authorize a companion Select")
        navigation.nativeControlFocus(scope: RemoteNavigationCoordinator.viewerNavigationScope, key: key, focused: true)
        XCTAssertTrue(navigation.choiceActionAdmitted(scope: RemoteNavigationCoordinator.viewerNavigationScope, key: key))
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .applied)
        navigation.modalPresentationDismissed(scope: RemoteNavigationCoordinator.viewerNavigationScope)
        XCTAssertEqual(navigation.paths, current)
        XCTAssertEqual(navigation.selectedTab, .home)
        XCTAssertFalse(navigation.hasOwnedModal)
        XCTAssertEqual(navigation.requestedFocus, RemoteNavigationCoordinator.viewerNavigationOpener)
    }

    #if os(tvOS)
    func testMeridianOpenerFocusPrefersChromeUntilContentIsRequested() {
        let controller = ViewerDestinationController()
        controller.loadViewIfNeeded()
        controller.makeRoot = { _ in AnyView(Text("Content")) }
        controller.select(.libraries)
        controller.updateChrome(AnyView(Text("Navigation")), header: AnyView(EmptyView()))
        controller.layout = .meridian
        controller.chromeFocusRequested = true
        XCTAssertTrue(controller.preferredFocusEnvironments.first === controller.chrome)
        controller.chromeFocusRequested = false
        XCTAssertTrue(controller.preferredFocusEnvironments.first === controller.hosts[.libraries])
        controller.layout = .standard
        controller.chromeFocusRequested = true
        XCTAssertTrue(controller.preferredFocusEnvironments.first === controller.hosts[.libraries])
        controller.expanded = true
        XCTAssertTrue(controller.preferredFocusEnvironments.first === controller.chrome)
    }
    #endif

    func testIdentityResetKeepsCurrentMeridianOpenerAndClearsAccountActions() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setViewerNavigationAvailable(true)
        var oldActions = 0
        navigation.register(scope: navigation.rootScope, key: RemoteNavigationCoordinator.viewerNavigationOpener,
            entry: .init(id: UUID(), label: "Old opener", frame: CGRect(x: 0, y: 0, width: 100, height: 48)) { oldActions += 1 })
        navigation.resetIdentity()
        navigation.selectedTab = .libraries
        navigation.paths[.libraries] = [.item(8)]
        navigation.synchronizeRoute()
        let currentPaths = navigation.paths
        let scope = navigation.rootScope
        navigation.setOrder(scope: scope, keys: ["library:view"], columns: 1)
        var currentActions = 0
        navigation.register(scope: scope, key: RemoteNavigationCoordinator.viewerNavigationOpener,
            entry: .init(id: UUID(), label: "Current opener", frame: CGRect(x: 0, y: 0, width: 100, height: 48)) { currentActions += 1 })
        navigation.openModal(scope: RemoteNavigationCoordinator.viewerNavigationScope,
            opener: RemoteNavigationCoordinator.viewerNavigationOpener) {}
        let identity = navigation.ownedModalIdentity
        navigation.closeModal()
        navigation.modalPresentationDismissed(scope: RemoteNavigationCoordinator.viewerNavigationScope, identity: identity)
        XCTAssertEqual(navigation.requestedFocus, RemoteNavigationCoordinator.viewerNavigationOpener)
        XCTAssertEqual(navigation.paths, currentPaths)
        XCTAssertNotEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(currentActions, 0, "A requested focus cannot authorize Select")
        navigation.nativeControlFocus(scope: scope, key: RemoteNavigationCoordinator.viewerNavigationOpener, focused: true)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(currentActions, 1)
        XCTAssertEqual(oldActions, 0, "Reset must still remove the previous account's action")
        navigation.setViewerNavigationAvailable(false)
        navigation.setOrder(scope: scope, keys: ["library:view"], columns: 1)
        navigation.openModal(scope: RemoteNavigationCoordinator.viewerNavigationScope,
            opener: RemoteNavigationCoordinator.viewerNavigationOpener) {}
        navigation.closeModal()
        navigation.modalPresentationDismissed(scope: RemoteNavigationCoordinator.viewerNavigationScope)
        XCTAssertEqual(navigation.requestedFocus, "library:view", "Standard layout must retire the Meridian opener")
    }

    func testRetiredPanelCompletionCannotDismissAReopenedPanel() throws {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        let scope = RemoteNavigationCoordinator.viewerNavigationScope
        navigation.openModal(scope: scope, opener: RemoteNavigationCoordinator.viewerNavigationOpener) {}
        let first = try XCTUnwrap(navigation.ownedModalIdentity)
        navigation.closeModal()
        navigation.openModal(scope: scope, opener: RemoteNavigationCoordinator.viewerNavigationOpener) {}
        let second = try XCTUnwrap(navigation.ownedModalIdentity)
        navigation.modalPresentationDismissed(scope: scope, identity: first)
        XCTAssertEqual(navigation.ownedModalIdentity, second)
        navigation.resetIdentity()
        XCTAssertFalse(navigation.hasOwnedModal)
        XCTAssertTrue(navigation.paths.isEmpty)
    }

    #if os(iOS)
    func testCompactMoreRendersNativeRecordingAttentionBadge() throws {
        let model = AppModel(startServices: false)
        model.layout = .meridian
        let navigation = RemoteNavigationCoordinator()
        func chrome(_ count: Int) -> AnyView {
            AnyView(ViewerNavigationChrome(expanded: false, recordingsAttention: count,
                togglePanel: {}, select: { _ in }, selectSource: { _ in })
                .environmentObject(model).environmentObject(navigation).frame(width: 320, height: 72))
        }
        let hosting = UIHostingController(rootView: chrome(5))
        let window = try makeWindow()
        window.frame.size.width = 320
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(0.5)
        let badge = try XCTUnwrap(findAttentionBadge(hosting.view))
        XCTAssertEqual(badge.text, "5", "The compact More opener must render the positive DVR count")
        XCTAssertEqual(badge.accessibilityLabel, "5 recordings need attention")
        hosting.rootView = chrome(0)
        settle()
        XCTAssertNil(findAttentionBadge(hosting.view))
        model.features = ServerFeatures(liveTv: true, recordings: false, sharedLibraries: true, libraryChannels: true)
        hosting.rootView = chrome(5)
        settle()
        XCTAssertNil(findAttentionBadge(hosting.view), "A hidden destination cannot leave a stale More badge")
    }

    func testStandardRestoresAdaptiveNativeTabsAndRetainsSettingsAcrossLayouts() throws {
        guard #available(iOS 18.0, *) else { throw XCTSkip("Adaptive iPad tabs require iOS 18") }
        let model = AppModel(startServices: false)
        model.phase = .ready
        model.homeLoading = false
        model.layout = .standard
        let root = HomeView(initialTab: .settings).environmentObject(model)
            .environmentObject(RemoteNavigationCoordinator()).environmentObject(RemotePlaybackAdapter())
            .environmentObject(RemoteClientModel()).environmentObject(InvitationClientModel())
        let hosting = UIHostingController(rootView: root)
        let window = try makeWindow()
        window.frame = CGRect(x: 0, y: 0, width: 1024, height: 768)
        // UIKit chooses the initial adaptive sidebar for this device's traits.
        // Compare Standard with the same native control in the same window.
        let native = UIHostingController(rootView: TabView(selection: .constant(HomeTab.settings)) {
            ForEach(HomeTab.viewerTabs, id: \.self) { tab in
                Tab(tab.title, systemImage: tab.symbol, value: tab) {
                    Text(tab.title).toolbarVisibility(.visible, for: .tabBar)
                }
                .badge(0)
                .hidden(false)
            }
        }.tabViewStyle(.sidebarAdaptable))
        window.rootViewController = native
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(1)
        let nativeTabs = try XCTUnwrap(findTabs(native))
        let nativeSidebarHidden = nativeTabs.sidebar.isHidden
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        settle(1)
        let tabs = try XCTUnwrap(findTabs(hosting))
        let settings = try XCTUnwrap(tabs.selectedViewController)
        XCTAssertEqual(tabs.mode, .tabSidebar)
        XCTAssertFalse(tabs.isTabBarHidden)
        XCTAssertEqual(tabs.sidebar.isHidden, nativeSidebarHidden,
            "Standard preserves the native adaptive sidebar choice for this device and geometry")
        XCTAssertEqual((tabs.viewControllers ?? []).compactMap { $0.tabBarItem.title }, HomeTab.viewerTabs.map(\.title))
        XCTAssertEqual(tabs.view.frame, try XCTUnwrap(tabs.view.superview).bounds)
        XCTAssertEqual(tabs.view.convert(tabs.view.bounds, to: hosting.view).width, hosting.view.bounds.width,
            accuracy: 0.5, "Standard must not reserve Meridian's sidebar width")
        let theme = model.theme
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        let attachment = XCTAttachment(image: image)
        attachment.name = "Restored Standard iPad — native adaptive tabs"
        attachment.lifetime = .keepAlways
        add(attachment)
        func settleChrome(until condition: () -> Bool) {
            let deadline = Date().addingTimeInterval(5)
            settle(until: condition, timeout: max(0, deadline.timeIntervalSinceNow))
            // SwiftUI can expose the bar before the chrome coordinator's
            // queued restoration. Finish that hop before changing the choice.
            let restored = expectation(description: "Queued native chrome restoration")
            DispatchQueue.main.async { restored.fulfill() }
            wait(for: [restored], timeout: max(0, deadline.timeIntervalSinceNow))
        }
        for sidebarHidden in [true, false] {
            tabs.sidebar.isHidden = sidebarHidden
            settle()
            model.layout = .meridian
            settleChrome(until: { tabs.isTabBarHidden && tabs.sidebar.isHidden })
            XCTAssertTrue(tabs.isTabBarHidden)
            XCTAssertTrue(tabs.sidebar.isHidden)
            XCTAssertEqual(model.theme, theme)
            XCTAssertTrue(tabs.selectedViewController === settings)
            window.frame.size.width = 390
            hosting.view.setNeedsLayout(); hosting.view.layoutIfNeeded()
            settle()
            XCTAssertTrue(tabs.selectedViewController === settings)
            model.layout = .standard
            window.frame.size.width = 1024
            hosting.view.setNeedsLayout(); hosting.view.layoutIfNeeded()
            settleChrome(until: { !tabs.isTabBarHidden && tabs.sidebar.isHidden == sidebarHidden })
            XCTAssertTrue(tabs.selectedViewController === settings)
            XCTAssertTrue(findTabs(hosting) === tabs)
            XCTAssertEqual(tabs.mode, .tabSidebar)
            XCTAssertFalse(tabs.isTabBarHidden)
            XCTAssertEqual(tabs.sidebar.isHidden, sidebarHidden,
                "Returning to Standard restores the viewer's native sidebar choice")
            XCTAssertEqual(model.theme, theme)
        }
        tabs.sidebar.isHidden = true
        model.features.recordings = false
        settle()
        XCTAssertFalse((tabs.viewControllers ?? []).contains { $0.tabBarItem.title == HomeTab.recordings.title })
        XCTAssertEqual(tabs.selectedViewController?.tabBarItem.title, HomeTab.settings.title)
    }

    private func findAttentionBadge(_ view: UIView) -> UILabel? {
        if let label = view as? UILabel, label.accessibilityIdentifier == "viewer-recording-attention-badge" { return label }
        return view.subviews.compactMap(findAttentionBadge).first
    }
    #endif

    func testProductionShellRendersMeridianWithAllEnvironmentOwners() throws { try renderFixture() }

    #if os(tvOS)
    func testStandardTVClassicRendersWithOriginalTheme() throws { try renderFixture(layout: .standard) }
    func testStandardTVTheaterRendersWithOriginalTheme() throws { try renderFixture(layout: .standard, tvHomeLayout: .theater) }

    func testStandardNativeTabFullscreenRetainsContentAfterAnotherTabsPushedChoice() throws {
        guard #available(tvOS 18.0, *) else { throw XCTSkip("Native retained tabs require tvOS 18") }
        let navigation = RemoteNavigationCoordinator()
        navigation.selectedTab = .libraries
        let drivers = Dictionary(uniqueKeysWithValues: HomeTab.viewerTabs.map { ($0, FullscreenLifecycleDriver()) })
        let library = try XCTUnwrap(drivers[.libraries])
        let live = try XCTUnwrap(drivers[.liveTv])
        let hosting = UIHostingController(rootView: FullscreenLifecycleShell(navigation: navigation, drivers: drivers))
        let window = try makeWindow()
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(0.5)
        let nativeTabs = try XCTUnwrap(findTabs(hosting))
        let libraryRoot = try XCTUnwrap(nativeTabs.selectedViewController)

        navigation.paths[.libraries] = [.sharedLibraries]
        settle(0.6)
        XCTAssertEqual(findNavigation(libraryRoot)?.viewControllers.count, 2)
        navigation.openModal(scope: "fixture-choice", opener: "fixture-open") { library.presented = false }
        library.presented = true
        settle(1)
        XCTAssertEqual(library.appearances, 1)
        XCTAssertTrue(navigation.focusControl(scope: "fixture-choice", key: "choice:second"))
        settle(0.2)
        navigation.closeModal()
        settle(0.6)
        XCTAssertEqual(library.disappearances, 1)
        navigation.paths[.libraries] = []
        settle(0.6)
        XCTAssertEqual(findNavigation(libraryRoot)?.viewControllers.count, 1)

        navigation.selectedTab = .liveTv
        settle(0.6)
        XCTAssertEqual(nativeTabs.selectedViewController?.tabBarItem.title, HomeTab.liveTv.title)
        live.presented = true
        settle(1)
        XCTAssertNotNil(findPresentation(hosting))
        let focusRevision = navigation.focusRevision
        for _ in 0..<12 { navigation.physicalInput(); settle(0.2) }
        XCTAssertEqual(navigation.focusRevision, focusRevision + 12, "Every unrelated focus publication must be exercised")
        XCTAssertEqual(live.appearances, 1, "A previously visited tab must not remount the active tab's fullscreen content")
        XCTAssertEqual(live.disappearances, 0)
        XCTAssertFalse(library.presented, "The hidden tab must not present concurrently")
        XCTAssertEqual(library.appearances, 1)
        live.presented = false
        settle(0.6)
        XCTAssertEqual(live.disappearances, 1)
    }

    func testStandardTVNativeTabsDisappearForPushedRoutes() throws {
        guard #available(tvOS 18.0, *) else { throw XCTSkip("Native retained tabs require tvOS 18") }
        let model = AppModel(startServices: false)
        model.phase = .ready
        model.homeLoading = false
        model.layout = .standard
        let navigation = RemoteNavigationCoordinator()
        let hosting = UIHostingController(rootView: HomeView(initialTab: .settings).environmentObject(model)
            .environmentObject(navigation).environmentObject(RemotePlaybackAdapter())
            .environmentObject(RemoteClientModel()).environmentObject(InvitationClientModel()))
        let window = try makeWindow()
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(0.6)
        let tabs = try XCTUnwrap(findTabs(hosting))
        let settings = try XCTUnwrap(tabs.selectedViewController)
        XCTAssertFalse(tabs.isTabBarHidden)
        XCTAssertEqual((tabs.viewControllers ?? []).compactMap { $0.tabBarItem.title }, HomeTab.viewerTabs.map(\.title))
        XCTAssertEqual(tabs.view.convert(tabs.view.bounds, to: hosting.view).width, hosting.view.bounds.width,
            accuracy: 0.5, "Standard must not reserve Meridian rail width")
        navigation.paths[.settings] = [.sharedLibraries]
        settle(0.6)
        XCTAssertTrue(tabs.isTabBarHidden, "Standard detail replaces the entire tab shell")
        XCTAssertEqual(findNavigation(settings)?.viewControllers.count, 2)
        navigation.paths[.settings] = []
        settle(0.6)
        XCTAssertFalse(tabs.isTabBarHidden, "Back restores native tabs")
        let stack = try XCTUnwrap(findNavigation(settings))
        stack.pushViewController(UIHostingController(rootView: Text("Implicit settings destination")), animated: false)
        settle(0.6)
        XCTAssertTrue(tabs.isTabBarHidden, "Implicit pushes also replace the Standard shell")
        stack.popViewController(animated: false)
        settle(0.6)
        XCTAssertFalse(tabs.isTabBarHidden, "Implicit Back restores native tabs")
        model.layout = .meridian
        settle(0.6)
        XCTAssertTrue(tabs.isTabBarHidden)
        XCTAssertLessThan(tabs.view.convert(tabs.view.bounds, to: hosting.view).width, hosting.view.bounds.width,
            "Meridian must reserve its visible rail width")
    }
    #endif

    #if os(iOS)
    func testStandardHomeRendersWithOriginalTheme() throws { try renderFixture(layout: .standard) }

    func testMeridianCompactSettingsDoesNotAddMoreNavigation() throws {
        guard #available(iOS 18.0, *) else { throw XCTSkip("Native retained tabs require iOS 18") }
        let state = NavigationDriver()
        state.layout = .meridian
        let controller = UIHostingController(rootView: ImplicitDestinationHostFixture(driver: state))
        let window = try makeWindow()
        window.frame.size.width = 390
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle()
        let tabs = try XCTUnwrap(findTabs(controller))
        let settings = try XCTUnwrap(tabs.selectedViewController)
        XCTAssertTrue(tabs.isTabBarHidden)
        XCTAssertFalse(settings.parent is UINavigationController, "Meridian must not acquire UIKit's overflow More navigation")
        let stack = try XCTUnwrap(findNavigation(settings))
        XCTAssertEqual(stack.viewControllers.count, 1)
        state.push = true
        settle(0.6)
        XCTAssertEqual(stack.viewControllers.count, 2)
        let detail = try XCTUnwrap(stack.topViewController)
        let disappearances = state.disappearances
        state.selection = .downloads
        settle(0.6)
        let downloads = try XCTUnwrap(tabs.selectedViewController)
        XCTAssertEqual(downloads.tabBarItem.title, HomeTab.downloads.title)
        XCTAssertFalse(downloads.parent is UINavigationController, "Meridian destinations must not enter More navigation")
        XCTAssertEqual(state.disappearances, disappearances + 1)
        state.selection = .settings
        settle(0.6)
        XCTAssertTrue(tabs.selectedViewController === settings)
        XCTAssertFalse(settings.parent is UINavigationController)
        XCTAssertTrue(stack.topViewController === detail)
        XCTAssertEqual(stack.viewControllers.count, 2)
        XCTAssertEqual(state.disappearances, disappearances + 1, "Returning must not remount the retained editor")
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        let attachment = XCTAttachment(image: image)
        attachment.name = "Meridian compact Settings — retained native destination"
        attachment.lifetime = .keepAlways
        add(attachment)
    }
    #endif

    func testNarrowPhoneAccessibilityNavigationRendersWithoutOverflow() throws {
        #if os(iOS)
        try renderFixture(width: 320, dynamicTypeSize: .accessibility2)
        #endif
    }

    private func renderFixture(width: CGFloat? = nil, dynamicTypeSize: DynamicTypeSize = .large, layout: ViewerLayout = .meridian, tvHomeLayout: TvHomeLayout = .classic) throws {
        let model = AppModel(startServices: false)
        model.phase = .ready
        model.homeLoading = false
        model.layout = layout
        let previousTheme = model.theme
        defer { model.setTheme(previousTheme) }
        let settings = SettingsStore()
        let previousTVLayout = settings.tvHomeLayout
        settings.tvHomeLayout = tvHomeLayout
        defer { settings.tvHomeLayout = previousTVLayout }
        model.setTheme(layout == .standard ? .noirr : .meridian)
        model.appearance = .dark
        model.libraries = [Library(id: 1, name: "Fixture Movies", kind: "movie"), Library(id: 2, name: "Fixture Series", kind: "show")]
        model.hubs = Hubs(
            continueWatching: [Item(id: 7, kind: "movie", title: "The Meridian Signal", year: 2026,
                overview: "An engineer follows a mysterious transmission across the midnight city, finding an unlikely ally at the last station before dawn.",
                poster: "meridian-fixture:signal:poster", backdrop: "meridian-fixture:signal:backdrop", runtimeMs: 7_200_000, watch: Watch(positionMs: 900_000, watched: false))],
            nextUp: [Item(id: 8, kind: "episode", title: "After the Crossing", poster: "meridian-fixture:night:poster", showTitle: "Night Routes")],
            recentlyAdded: [Item(id: 9, kind: "movie", title: "Beyond the Horizon", poster: "meridian-fixture:horizon:poster")]
        )
        let root = HomeView().environmentObject(model)
            .environmentObject(RemoteNavigationCoordinator())
            .environmentObject(RemotePlaybackAdapter())
            .environmentObject(RemoteClientModel())
            .environmentObject(InvitationClientModel())
            .dynamicTypeSize(dynamicTypeSize)
        let controller = UIHostingController(rootView: root)
        let window = try makeWindow()
        if let width { window.frame.size.width = width }
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(3)
        if #available(iOS 18.0, tvOS 18.0, *) {
            XCTAssertNotNil(findTabs(controller), "Screenshot must exercise the shipping native tab host")
        } else {
            XCTAssertNotNil(findLegacyDestination(controller), "Screenshot must exercise the shipping legacy host")
        }
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        let attachment = XCTAttachment(image: image)
        attachment.name = "\(layout.rawValue) \(tvHomeLayout.rawValue) native production shell — fixture titles — \(Int(window.bounds.width))x\(Int(window.bounds.height))"
        attachment.lifetime = .keepAlways
        add(attachment)
        // A bounded window permits an independent simctl framebuffer capture.
        settle(5)
    }

    private func makeWindow() throws -> UIWindow {
        let scene = try XCTUnwrap(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.frame = scene.coordinateSpace.bounds
        window.windowLevel = .alert + 1
        return window
    }
    private func settle(_ seconds: TimeInterval = 0.2) {
        RunLoop.main.run(until: Date().addingTimeInterval(seconds))
    }
    /// Layout changes reach the native chrome after a SwiftUI update and one
    /// main-queue hop; wait for the asserted state instead of a fixed delay.
    private func settle(until condition: () -> Bool, timeout: TimeInterval = 5) {
        let deadline = Date().addingTimeInterval(timeout)
        while !condition() && Date() < deadline {
            RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        }
    }
    private func findNavigation(_ controller: UIViewController) -> UINavigationController? {
        if let navigation = controller as? UINavigationController { return navigation }
        return controller.children.compactMap(findNavigation).first
    }
    private func selectedDestination(_ controller: UIViewController) -> UIViewController? {
        if #available(iOS 18.0, tvOS 18.0, *) { return findTabs(controller)?.selectedViewController }
        guard let legacy = findLegacyDestination(controller), let tab = legacy.activeTab else { return nil }
        return legacy.hosts[tab]
    }
    private func findTabs(_ controller: UIViewController) -> UITabBarController? {
        if let tabs = controller as? UITabBarController { return tabs }
        return controller.children.compactMap(findTabs).first
    }
    private func findLegacyDestination(_ controller: UIViewController) -> ViewerDestinationController? {
        if let destination = controller as? ViewerDestinationController { return destination }
        return controller.children.compactMap(findLegacyDestination).first
    }
    private func findPresentation(_ controller: UIViewController) -> UIViewController? {
        if let presented = controller.presentedViewController { return presented }
        return controller.children.compactMap(findPresentation).first
    }
}

@MainActor
private final class NavigationDriver: ObservableObject {
    @Published var push = false
    @Published var layout = ViewerLayout.standard
    @Published var selection = HomeTab.settings
    var disappearances = 0
}
private struct ImplicitDestinationHostFixture: View {
    @ObservedObject var driver: NavigationDriver
    var nativeBaseline = false
    var body: some View {
        if nativeBaseline {
            TabView(selection: $driver.selection) {
                ForEach(HomeTab.viewerTabs, id: \.self) { tab in
                    ImplicitNavigationFixture(driver: driver, title: tab.title)
                        .tabItem { Label(tab.title, systemImage: tab.symbol) }.tag(tab)
                }
            }
        } else {
            ViewerDestinationHost(selection: driver.selection, layout: driver.layout,
                expanded: false, chromeFocusRequested: false, accessibilityNavigation: false, visibleTabs: HomeTab.viewerTabs,
                recordingsAttention: 0, selectionChanged: { driver.selection = $0 },
                standardNavigationIsPushed: driver.push, panelDismissed: {},
                root: { AnyView(ImplicitNavigationFixture(driver: driver, title: $0.title)) },
                chrome: AnyView(Text("Navigation chrome")), header: AnyView(Text("Navigation header")))
        }
    }
}
private struct ImplicitNavigationFixture: View {
    @ObservedObject var driver: NavigationDriver
    let title: String
    var body: some View {
        NavigationStack {
            NavigationLink(isActive: Binding(
                get: { title == HomeTab.settings.title && driver.push },
                set: { if title == HomeTab.settings.title { driver.push = $0 } }
            )) {
                Text("Retained nested editor")
                    .onDisappear { if title == HomeTab.settings.title { driver.disappearances += 1 } }
            } label: { Text(title) }
                .navigationTitle(title)
                .onDisappear { if title == HomeTab.settings.title { driver.disappearances += 1 } }
        }
    }
}

#if os(tvOS)
@MainActor
private final class FullscreenLifecycleDriver: ObservableObject {
    @Published var presented = false
    var appearances = 0
    var disappearances = 0
}

private struct FullscreenLifecycleShell: View {
    @ObservedObject var navigation: RemoteNavigationCoordinator
    let drivers: [HomeTab: FullscreenLifecycleDriver]

    var body: some View {
        ViewerDestinationHost(selection: navigation.selectedTab, layout: .standard,
            expanded: false, chromeFocusRequested: false, accessibilityNavigation: false, visibleTabs: HomeTab.viewerTabs,
            recordingsAttention: 0, selectionChanged: { navigation.selectedTab = $0 },
            standardNavigationIsPushed: !(navigation.paths[navigation.selectedTab] ?? []).isEmpty,
            panelDismissed: {},
            root: { tab in
                guard let driver = drivers[tab] else { return AnyView(EmptyView()) }
                return AnyView(FullscreenLifecycleRoot(navigation: navigation, driver: driver, tab: tab))
            },
            chrome: AnyView(EmptyView()), header: AnyView(EmptyView()))
            .diagnosticReportHost()
            .tint(Palette.accent)
            .background(Palette.bg.ignoresSafeArea())
    }
}

private struct FullscreenLifecycleRoot: View {
    @ObservedObject var navigation: RemoteNavigationCoordinator
    @ObservedObject var driver: FullscreenLifecycleDriver
    @FocusState private var coverFocused: Bool
    let tab: HomeTab

    var body: some View {
        NavigationStack(path: Binding(get: { navigation.paths[tab] ?? [] }, set: { navigation.paths[tab] = $0 })) {
            Group {
                if tab == .libraries { Text(tab.title) }
                else {
                    Text(tab.title)
                        .fullScreenCover(isPresented: $driver.presented) { fullscreenContent }
                }
            }
            .navigationDestination(for: Route.self) { _ in
                Text("Pushed choices")
                    .remoteChoicePresentation(isPresented: $driver.presented, scope: "fixture-choice") {
                        RemoteChoicePanel(scope: "fixture-choice", title: "Fixture choices", choices: [
                            RemoteChoice(id: "first", label: "First", selected: true) {},
                            RemoteChoice(id: "second", label: "Second", selected: false) {}
                        ])
                        .onAppear { driver.appearances += 1 }
                        .onDisappear { driver.disappearances += 1 }
                    }
            }
        }
        .environmentObject(navigation)
        .preferredColorScheme(.dark)
        .fontDesign(.default)
        .tint(Palette.accent)
        .dynamicTypeSize(.large)
    }

    private var fullscreenContent: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            VStack {
                Text("Focus revision \(navigation.focusRevision)")
                Button("Keep presentation") {}
                    .focused($coverFocused)
            }
        }
        .onAppear { driver.appearances += 1; coverFocused = true }
        .onDisappear { driver.disappearances += 1 }
    }
}
#endif
