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
        settle()
        let host = try XCTUnwrap(controller.hosts[.settings])
        let navigation = try XCTUnwrap(findNavigation(host))
        state.push = true
        settle()
        XCTAssertEqual(navigation.viewControllers.count, 2, "Fixture must create an actual implicit native push")
        let detail = try XCTUnwrap(navigation.topViewController)
        for width in [390, 1200, 700, 390] {
            host.rootView = AnyView(ImplicitNavigationFixture(driver: state, title: "Settings"))
            controller.layout = controller.layout == .standard ? .meridian : .standard
            window.frame.size.width = CGFloat(width)
            controller.view.setNeedsLayout(); controller.view.layoutIfNeeded()
            settle()
            XCTAssertTrue(controller.hosts[.settings] === host)
            XCTAssertTrue(navigation.topViewController === detail)
            XCTAssertEqual(navigation.viewControllers.count, 2)
        }
        controller.select(.home); settle()
        XCTAssertNil(host.view.superview, "Inactive roots cannot participate in focus, hit testing or accessibility")
        controller.select(.settings); settle()
        XCTAssertTrue(controller.hosts[.settings] === host)
        XCTAssertTrue(navigation.topViewController === detail)
        navigation.popViewController(animated: false)
        settle()
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

    func testProductionShellRendersMeridianWithAllEnvironmentOwners() throws { try renderFixture() }

    func testNarrowPhoneAccessibilityNavigationRendersWithoutOverflow() throws {
        #if os(iOS)
        try renderFixture(width: 320, dynamicTypeSize: .accessibility2)
        #endif
    }

    private func renderFixture(width: CGFloat? = nil, dynamicTypeSize: DynamicTypeSize = .large) throws {
        let model = AppModel(startServices: false)
        model.phase = .ready
        model.homeLoading = false
        model.layout = .meridian
        let previousTheme = model.theme
        defer { model.setTheme(previousTheme) }
        model.setTheme(.meridian)
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
        attachment.name = "Meridian native production shell — fixture titles — \(Int(window.bounds.width))x\(Int(window.bounds.height))"
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
private final class NavigationDriver: ObservableObject { @Published var push = false }
private struct ImplicitNavigationFixture: View {
    @ObservedObject var driver: NavigationDriver
    let title: String
    var body: some View {
        NavigationStack {
            NavigationLink(destination: Text("Retained nested editor"), isActive: $driver.push) { Text(title) }
                .navigationTitle(title)
        }
    }
}
