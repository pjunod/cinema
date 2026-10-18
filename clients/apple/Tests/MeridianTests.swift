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
        let state = NavigationDriver()
        let controller = ViewerDestinationController()
        controller.makeRoot = { tab in AnyView(ImplicitNavigationFixture(driver: state, title: tab.title)) }
        let window = try makeWindow()
        window.frame = CGRect(x: 0, y: 0, width: 1024, height: 768)
        window.rootViewController = controller
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        controller.select(.settings)
        settle(0.6)
        let host = try XCTUnwrap(controller.hosts[.settings])
        let navigation = try XCTUnwrap(findNavigation(host))
        state.push = true
        settle(0.6)
        XCTAssertEqual(navigation.viewControllers.count, 2, "Fixture must create an actual implicit native push")
        let detail = try XCTUnwrap(navigation.topViewController)
        let disappearances = state.disappearances
        for width in [390, 1200, 700, 390] {
            host.rootView = AnyView(ImplicitNavigationFixture(driver: state, title: "Settings"))
            controller.layout = controller.layout == .standard ? .meridian : .standard
            window.frame.size.width = CGFloat(width)
            controller.view.setNeedsLayout(); controller.view.layoutIfNeeded()
            settle(0.6)
            XCTAssertTrue(controller.hosts[.settings] === host)
            XCTAssertTrue(navigation.topViewController === detail)
            XCTAssertEqual(navigation.viewControllers.count, 2)
            XCTAssertEqual(state.disappearances, disappearances, "Changing chrome must not trigger playback-owner disappearance")
        }
        controller.select(.home); settle(0.6)
        var ancestor: UIView? = host.view
        var hidden = host.view.window == nil
        while let view = ancestor {
            hidden = hidden || view.isHidden || view.alpha == 0
            ancestor = view.superview
        }
        XCTAssertTrue(hidden, "Inactive roots must be detached or hidden by their native container")
        XCTAssertEqual(state.disappearances, disappearances + 1, "Leaving a root must notify its playback owners")
        controller.select(.settings); settle(0.6)
        XCTAssertTrue(controller.hosts[.settings] === host)
        XCTAssertTrue(navigation.topViewController === detail)
        navigation.popViewController(animated: false)
        settle(0.6)
        XCTAssertEqual(navigation.viewControllers.count, 1, "Back returns to the original implicit root")
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
        window.rootViewController = hosting
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle(1)
        let destination = try XCTUnwrap(findDestination(hosting))
        let settings = try XCTUnwrap(destination.hosts[.settings])
        let tabs = try XCTUnwrap(destination.nativeTabs)
        XCTAssertEqual(tabs.mode, .tabSidebar)
        XCTAssertFalse(tabs.isTabBarHidden)
        XCTAssertTrue(tabs.sidebar.isHidden, "Standard starts with the original floating top tabs")
        XCTAssertEqual(tabs.tabs.map(\.title), HomeTab.viewerTabs.map(\.title))
        XCTAssertEqual(tabs.view.frame, destination.view.bounds, "Standard must not reserve Meridian's sidebar width")
        XCTAssertTrue(try XCTUnwrap(destination.chrome).view.isHidden)
        let theme = model.theme
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in window.drawHierarchy(in: window.bounds, afterScreenUpdates: true) }
        let attachment = XCTAttachment(image: image)
        attachment.name = "Restored Standard iPad — native adaptive tabs"
        attachment.lifetime = .keepAlways
        add(attachment)
        tabs.sidebar.isHidden = false
        settle()
        model.layout = .meridian
        settle()
        XCTAssertTrue(tabs.isTabBarHidden)
        XCTAssertTrue(tabs.sidebar.isHidden)
        XCTAssertEqual(model.theme, theme)
        XCTAssertTrue(destination.hosts[.settings] === settings)
        window.frame.size.width = 390
        hosting.view.setNeedsLayout(); hosting.view.layoutIfNeeded()
        settle()
        XCTAssertTrue(destination.hosts[.settings] === settings)
        model.layout = .standard
        window.frame.size.width = 1024
        hosting.view.setNeedsLayout(); hosting.view.layoutIfNeeded()
        settle()
        XCTAssertTrue(destination.hosts[.settings] === settings)
        XCTAssertTrue(destination.nativeTabs === tabs)
        XCTAssertEqual(tabs.mode, .tabSidebar)
        XCTAssertFalse(tabs.isTabBarHidden)
        XCTAssertFalse(tabs.sidebar.isHidden, "Returning to Standard restores the viewer's native sidebar choice")
        XCTAssertEqual(model.theme, theme)
        tabs.sidebar.isHidden = true
        model.features.recordings = false
        settle()
        XCTAssertFalse(tabs.tabs.contains { $0.identifier == HomeTab.recordings.title })
        XCTAssertEqual(tabs.selectedTab?.identifier, HomeTab.settings.title)
    }

    private func findAttentionBadge(_ view: UIView) -> UILabel? {
        if let label = view as? UILabel, label.accessibilityIdentifier == "viewer-recording-attention-badge" { return label }
        return view.subviews.compactMap(findAttentionBadge).first
    }
    #endif

    func testProductionShellRendersMeridianWithAllEnvironmentOwners() throws { try renderFixture() }

    #if os(iOS)
    func testStandardHomeRendersWithOriginalTheme() throws { try renderFixture(layout: .standard) }

    func testMeridianCompactSettingsDoesNotAddMoreNavigation() throws {
        guard #available(iOS 18.0, *) else { throw XCTSkip("Native retained tabs require iOS 18") }
        let controller = ViewerDestinationController()
        let state = NavigationDriver()
        controller.layout = .meridian
        controller.makeRoot = { tab in AnyView(ImplicitNavigationFixture(driver: state, title: tab.title)) }
        let window = try makeWindow()
        window.frame.size.width = 390
        window.rootViewController = controller
        controller.select(.settings)
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        settle()
        let tabs = try XCTUnwrap(controller.nativeTabs)
        let settings = try XCTUnwrap(controller.hosts[.settings])
        XCTAssertTrue(tabs.isTabBarHidden)
        XCTAssertFalse(settings.parent is UINavigationController, "Meridian must not acquire UIKit's overflow More navigation")
        XCTAssertEqual(findNavigation(settings)?.viewControllers.count, 1)
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

    private func renderFixture(width: CGFloat? = nil, dynamicTypeSize: DynamicTypeSize = .large, layout: ViewerLayout = .meridian) throws {
        let model = AppModel(startServices: false)
        model.phase = .ready
        model.homeLoading = false
        model.layout = layout
        let previousTheme = model.theme
        defer { model.setTheme(previousTheme) }
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
        XCTAssertNotNil(findDestination(controller), "Screenshot must exercise the shipping persistent host")
        let image = UIGraphicsImageRenderer(bounds: window.bounds).image { _ in
            window.drawHierarchy(in: window.bounds, afterScreenUpdates: true)
        }
        let attachment = XCTAttachment(image: image)
        attachment.name = "\(layout.rawValue) native production shell — fixture titles — \(Int(window.bounds.width))x\(Int(window.bounds.height))"
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
    private func findNavigation(_ controller: UIViewController) -> UINavigationController? {
        if let navigation = controller as? UINavigationController { return navigation }
        return controller.children.compactMap(findNavigation).first
    }
    private func findDestination(_ controller: UIViewController) -> ViewerDestinationController? {
        if let destination = controller as? ViewerDestinationController { return destination }
        return controller.children.compactMap(findDestination).first
    }
}

@MainActor
private final class NavigationDriver: ObservableObject {
    @Published var push = false
    var disappearances = 0
}
private struct ImplicitNavigationFixture: View {
    @ObservedObject var driver: NavigationDriver
    let title: String
    var body: some View {
        NavigationStack {
            NavigationLink(destination: Text("Retained nested editor"), isActive: $driver.push) { Text(title) }
                .navigationTitle(title)
        }
        .onDisappear { driver.disappearances += 1 }
    }
}
