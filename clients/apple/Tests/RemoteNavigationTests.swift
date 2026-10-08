import Foundation
import XCTest
@testable import plurx

final class RemoteNavigationTests: XCTestCase {
    @MainActor
    func testRestrictedModalFencesAllActionsAndSensitiveState() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var activated = 0
        navigation.register(scope: "home", key: "movie", entry: .init(id: UUID(), label: "Private title", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        navigation.physicalFocus(scope: "home", key: "movie")
        let context = navigation.context
        navigation.setSearch(scope: "home", nonce: UUID()) { _ in XCTFail("Restricted text executed") }
        let blocker = UUID()
        navigation.setBlocked(blocker, true)
        let state = navigation.snapshot()
        XCTAssertTrue(state.blocked)
        XCTAssertNil(state.focusedLabel)
        XCTAssertNil(state.textNonce)
        for action in [RemoteNavigationCoordinator.Action.select, .back, .home, .navigate(.down)] {
            XCTAssertEqual(navigation.dispatch(action, context: context), .restrictedSurface)
        }
        XCTAssertEqual(activated, 0)
        navigation.setBlocked(blocker, false)
        XCTAssertEqual(navigation.dispatch(.select, context: context), .staleContext)
    }
    @MainActor
    func testPhysicalFocusAndReplacedRegistrationFenceSelect() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var activated = 0
        let registration = UUID()
        navigation.register(scope: "home", key: "movie", entry: .init(id: registration, label: "Movie", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        navigation.physicalFocus(scope: "home", key: "movie")
        let context = navigation.context
        navigation.physicalFocus(scope: "home", key: nil)
        XCTAssertEqual(navigation.dispatch(.select, context: context), .staleFocus)
        navigation.physicalFocus(scope: "home", key: "movie")
        let current = navigation.context
        navigation.register(scope: "home", key: "movie", entry: .init(id: UUID(), label: "Replacement", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        XCTAssertEqual(navigation.dispatch(.select, context: current), .staleFocus)
        XCTAssertEqual(activated, 0)
    }
    @MainActor
    func testOwnedChoiceBackRestoresFocusAndOldSelectCannotReplay() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var dismissed = 0
        navigation.physicalFocus(scope: "home", key: "sort")
        navigation.openModal(scope: "sort-menu", opener: "sort") { dismissed += 1 }
        navigation.setOrder(scope: "sort-menu", keys: ["title", "year"], columns: 1)
        XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .applied)
        XCTAssertNil(navigation.focusedKey)
        XCTAssertEqual(navigation.requestedFocus, "title")
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .applied)
        XCTAssertEqual(dismissed, 1)
        XCTAssertNil(navigation.focusedKey)
        XCTAssertEqual(navigation.requestedFocus, "sort")
        var activated = 0
        navigation.register(scope: "home", key: "sort", entry: .init(id: UUID(), label: "Sort", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        navigation.physicalFocus(scope: "home", key: "sort")
        let context = navigation.context
        XCTAssertEqual(navigation.dispatch(.select, context: context), .applied)
        XCTAssertEqual(navigation.dispatch(.select, context: context), .staleFocus)
        XCTAssertEqual(activated, 1)
    }
    @MainActor
    func testSearchNonceEpochAndRestrictedRouteAreFailClosed() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.selectedTab = .search
        navigation.synchronizeRoute()
        let nonce = UUID()
        var query = ""
        navigation.setSearch(scope: "search", nonce: nonce) { query = $0 }
        XCTAssertEqual(navigation.dispatch(.textReplace(nonce: UUID(), text: "wrong"), context: navigation.context), .staleContext)
        XCTAssertEqual(navigation.dispatch(.textReplace(nonce: nonce, text: "movie"), context: navigation.context), .applied)
        XCTAssertEqual(query, "movie")
        let old = navigation.context
        navigation.invalidate()
        XCTAssertEqual(navigation.dispatch(.select, context: old), .staleContext)
        navigation.selectedTab = .settings
        XCTAssertEqual(navigation.dispatch(.home, context: navigation.context), .restrictedSurface)
        XCTAssertTrue(navigation.snapshot().blocked)
    }
    @MainActor
    func testLazyLogicalTargetNeedsRealRegistrationBeforeActivation() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["visible", "unrealized"], columns: 1)
        navigation.physicalFocus(scope: "home", key: "visible")
        XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .applied)
        XCTAssertEqual(navigation.requestedFocus, "unrealized")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
        var activated = 0
        navigation.register(scope: "home", key: "unrealized", entry: .init(id: UUID(), label: "Next movie", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
        navigation.physicalFocus(scope: "home", key: "unrealized")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(activated, 1)
    }
    @MainActor
    func testSpatialDirectionsUseRealizedGeometryWithDeterministicTies() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["origin", "right", "below"], columns: 1)
        for (key, x, y) in [("origin", 0, 0), ("right", 200, 0), ("below", 0, 200)] {
            navigation.register(scope: "home", key: key, entry: .init(id: UUID(), label: key, frame: .init(x: x, y: y, width: 100, height: 60)) {})
        }
        navigation.physicalFocus(scope: "home", key: "origin")
        XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .applied)
        XCTAssertEqual(navigation.requestedFocus, "below")
        navigation.physicalFocus(scope: "home", key: "origin")
        XCTAssertEqual(navigation.dispatch(.navigate(.right), context: navigation.context), .applied)
        XCTAssertEqual(navigation.requestedFocus, "right")
    }
    @MainActor
    func testIdentityResetClearsRoutesClosuresAndSearch() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var activated = 0
        navigation.register(scope: "home", key: "old", entry: .init(id: UUID(), label: "Old account", frame: .init(x: 0, y: 0, width: 100, height: 60)) { activated += 1 })
        navigation.setOrder(scope: "home", keys: ["old"], columns: 1)
        navigation.setSearch(scope: "home", nonce: UUID()) { _ in XCTFail("Old account search") }
        navigation.navigate(to: .item(42))
        navigation.resetIdentity()
        XCTAssertEqual(navigation.selectedTab, .home)
        XCTAssertTrue(navigation.paths.isEmpty)
        XCTAssertNil(navigation.snapshot().textNonce)
        navigation.physicalFocus(scope: "home", key: "old")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
        XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .unsupported)
        XCTAssertEqual(activated, 0)
    }
}
