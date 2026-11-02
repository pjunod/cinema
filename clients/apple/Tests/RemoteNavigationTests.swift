import Foundation
import SwiftUI
import UIKit
import Combine
import XCTest
@testable import plurx

final class RemoteNavigationTests: XCTestCase {
    @MainActor
    func testPhysicalFocusKeepsShellQuietAndRetiresStaleSelect() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var selections: [String] = []
        for key in ["first", "second"] {
            navigation.register(scope: "home", key: key,
                entry: .init(id: UUID(), label: key, frame: CGRect(x: 0, y: 0, width: 100, height: 60)) { selections.append(key) })
        }
        navigation.physicalFocus(scope: "home", key: "first")
        let previous = navigation.context
        var shellUpdates = 0
        let observation = navigation.objectWillChange.sink { shellUpdates += 1 }
        defer { observation.cancel() }

        for index in 0..<100 {
            navigation.physicalInput()
            navigation.physicalFocus(scope: "home", key: index.isMultiple(of: 2) ? "first" : "second")
        }

        XCTAssertEqual(shellUpdates, 0, "Native focus must not invalidate every navigation observer and tab root")
        XCTAssertEqual(navigation.snapshot().focusedLabel, "second")
        XCTAssertEqual(navigation.dispatch(.select, context: previous), .staleFocus)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(selections, ["second"])
    }

    @MainActor
    func testPhysicalFocusObserversSeeSettledStateWithoutRebuildingParent() async {
        let navigation = RemoteNavigationCoordinator()
        let updates = RemoteFocusUpdates()
        let host = UIHostingController(rootView: RemoteFocusUpdateFixture(updates: updates).environmentObject(navigation))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 400, height: 400))
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer { window.isHidden = true; window.rootViewController = nil }
        host.view.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(150))
        let previousRenders = updates.renders

        navigation.physicalFocus(scope: "home", key: "first")
        navigation.physicalInput()
        navigation.physicalFocus(scope: "home", key: "second")
        try? await Task.sleep(for: .milliseconds(150))

        XCTAssertEqual(updates.focusedKeys, ["second"], "Observe the settled focus, not intermediate authority retirement")
        XCTAssertEqual(updates.inputRevisions, [navigation.physicalInputRevision])
        XCTAssertEqual(updates.renders, previousRenders, "The observer modifier must not rebuild its parent")
    }

    @MainActor
    func testNavigationOpenerOnlyPublishesWhenChromeFocusPreferenceChanges() {
        let navigation = RemoteNavigationCoordinator()
        let opener = RemoteNavigationCoordinator.viewerNavigationOpener
        navigation.setOrder(scope: "home", keys: [opener, "movie"], columns: 1)
        XCTAssertTrue(navigation.focusControl(scope: "home", key: opener))
        XCTAssertTrue(navigation.viewerNavigationFocusRequested)
        navigation.physicalFocus(scope: "home", key: opener)
        navigation.physicalInput()
        navigation.physicalFocus(scope: "home", key: "movie")
        XCTAssertFalse(navigation.viewerNavigationFocusRequested)
        XCTAssertTrue(navigation.focusControl(scope: "home", key: opener))
        navigation.invalidate()
        XCTAssertFalse(navigation.viewerNavigationFocusRequested)
    }

    @MainActor
    func testCoveredParentOrderValidatesLazyReturnWithoutRetainingAuthorization() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["jump", "item:1"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.releaseScope("home")
        navigation.closeModal(returnFocus: "item:1")
        XCTAssertEqual(navigation.requestedFocus, "item:1")
        XCTAssertNil(navigation.focusedKey)
        XCTAssertNotEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "item:1")

        navigation.setOrder(scope: "home", keys: ["jump", "item:1"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.releaseScope("home")
        navigation.closeModal(returnFocus: "unknown")
        XCTAssertEqual(navigation.requestedFocus, "jump")

        navigation.setOrder(scope: "home", keys: ["jump", "item:1"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.setOrder(scope: "home", keys: [], columns: 1)
        navigation.closeModal(returnFocus: "item:1")
        XCTAssertNotEqual(navigation.requestedFocus, "item:1")
    }
    @MainActor
    func testActualStyleFocusKeepsPendingEntryInertUntilNativeAcknowledgment() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var retired = 0
        navigation.onPhysicalInput = { retired += 1 }
        navigation.openModal(scope: "view", opener: "movie") {}
        for key in ["rows", "grid"] {
            navigation.register(scope: "view", key: key, entry: .init(id: UUID(), label: key, frame: .init(x: 0, y: 0, width: 100, height: 60)) {})
        }
        navigation.enterChoicePanel(scope: "view", keys: ["rows", "grid"], selected: "grid")
        navigation.nativeControlFocus(scope: "view", key: "rows", focused: true)
        XCTAssertEqual(navigation.requestedFocus, "grid")
        XCTAssertFalse(navigation.choiceActionAdmitted(scope: "view", key: "rows"))
        navigation.physicalInput()
        XCTAssertFalse(navigation.choiceActionAdmitted(scope: "view", key: "rows"))
        navigation.nativeControlFocus(scope: "view", key: "grid", focused: true)
        XCTAssertTrue(navigation.choiceActionAdmitted(scope: "view", key: "grid"))
        XCTAssertFalse(navigation.choiceActionAdmitted(scope: "view", key: "rows"))
        XCTAssertGreaterThan(retired, 0)
    }
    @MainActor
    func testLateControlTapRetiresAuthorityWithoutCancelingNewPresentationIntent() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var retired = 0
        navigation.onPhysicalInput = { retired += 1 }
        navigation.setOrder(scope: "home", keys: ["jump", "item:416"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.enterChoicePanel(scope: "jump-menu", keys: ["jump:A"], selected: "jump:A")
        let entryRevision = navigation.focusRevision
        navigation.physicalControlActivation(scope: "home")
        XCTAssertEqual(navigation.requestedFocus, "jump:A")
        XCTAssertEqual(navigation.focusRevision, entryRevision)
        navigation.closeModal(returnFocus: "item:416")
        let returnRevision = navigation.focusRevision
        navigation.physicalControlActivation(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "item:416")
        XCTAssertEqual(navigation.focusRevision, returnRevision)
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "item:416")
        XCTAssertEqual(retired, 2)
    }
    #if os(tvOS)
    @MainActor
    func testTabMovementDefersContentFocusWhileSameTabBackRestoresBookmark() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.physicalFocus(scope: "home", key: "movie")
        navigation.navigate(to: .item(42))
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .applied)
        XCTAssertEqual(navigation.requestedFocus, "movie")
        navigation.selectedTab = .libraries
        navigation.synchronizeRoute()
        XCTAssertNil(navigation.requestedFocus)
        navigation.physicalFocus(scope: "libraries", key: "see-all")
        navigation.selectedTab = .home
        navigation.synchronizeRoute()
        XCTAssertNil(navigation.requestedFocus)
        XCTAssertNil(navigation.focusedKey)
    }
    #endif
    @MainActor
    func testEarlyNativeTargetAcknowledgmentCannotRetirePostDismissalReturn() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["jump", "item:1"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.closeModal(returnFocus: "item:1")
        navigation.nativeControlFocus(scope: "home", key: "item:1", focused: true)
        navigation.nativeControlFocus(scope: "home", key: "jump", focused: true)
        XCTAssertEqual(navigation.focusedKey, "jump")
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "item:1")
        XCTAssertNil(navigation.focusedKey)
        navigation.physicalInput()
        let revision = navigation.focusRevision
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.focusRevision, revision)
    }
    @MainActor
    func testUnchangedNativeFocusCannotCancelPendingTargetButCanAcknowledgeIt() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["opener", "target"], columns: 1)
        for key in ["opener", "target"] {
            navigation.register(scope: "home", key: key, entry: .init(id: UUID(), label: key, frame: .init(x: 0, y: 0, width: 100, height: 60)) {})
        }
        navigation.nativeControlFocus(scope: "home", key: "opener", focused: true)
        XCTAssertTrue(navigation.focusControl(scope: "home", key: "target"))
        navigation.nativeControlFocus(scope: "home", key: "opener", focused: true)
        XCTAssertEqual(navigation.requestedFocus, "target")
        XCTAssertNil(navigation.focusedKey)
        navigation.nativeControlFocus(scope: "home", key: "target", focused: true)
        XCTAssertEqual(navigation.focusedKey, "target")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
    }
    @MainActor
    func testInitialNativeDefaultCannotCancelSelectedChoiceBeforeRealization() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.openModal(scope: "view", opener: "view") {}
        navigation.enterChoicePanel(scope: "view", keys: ["rows", "grid"], selected: "grid")
        navigation.physicalFocus(scope: "view", key: "rows")
        XCTAssertEqual(navigation.requestedFocus, "grid")
        let revision = navigation.focusRequestRevision
        navigation.enterChoicePanel(scope: "view", keys: ["rows", "grid"], selected: "grid")
        XCTAssertGreaterThan(navigation.focusRequestRevision, revision)
        navigation.physicalFocus(scope: "view", key: "grid")
        XCTAssertEqual(navigation.focusedKey, "grid")
    }
    @MainActor
    func testChoiceEntrySelectsCurrentValueAndNativeDismissalRestoresOpener() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["sort", "view"], columns: 1)
        navigation.physicalFocus(scope: "home", key: "sort")
        navigation.openModal(scope: "sort-menu", opener: "sort") {}
        let token = UUID()
        navigation.attachPresentation(token, scope: "sort-menu", controllers: [])
        navigation.enterChoicePanel(scope: "sort-menu", keys: ["title", "year"], selected: "year")
        XCTAssertEqual(navigation.requestedFocus, "year")
        XCTAssertNil(navigation.focusedKey)
        navigation.closeModal()
        navigation.detachPresentation(token)
        navigation.modalPresentationDismissed(scope: "sort-menu")
        XCTAssertEqual(navigation.activeScope, "home")
        XCTAssertEqual(navigation.requestedFocus, "sort")
        XCTAssertNil(navigation.focusedKey)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
    }
    @MainActor
    func testDismissalCannotOverrideNewPhysicalIntentAndMissingOpenerFallsBack() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["survivor"], columns: 1)
        navigation.openModal(scope: "menu", opener: "removed") {}
        navigation.closeModal()
        XCTAssertEqual(navigation.requestedFocus, "survivor")
        navigation.physicalInput()
        let revision = navigation.focusRevision
        navigation.modalPresentationDismissed(scope: "menu")
        XCTAssertEqual(navigation.focusRevision, revision)
        XCTAssertNil(navigation.requestedFocus)
    }
    @MainActor
    func testOldNativeDismissalCannotCloseReplacementWithSameScope() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        var dismissed = 0
        navigation.openModal(scope: "menu", opener: "sort") { dismissed += 1 }
        let identity = navigation.ownedModalIdentity
        navigation.closeModal()
        navigation.openModal(scope: "menu", opener: "view") { dismissed += 1 }
        navigation.modalPresentationDismissed(scope: "menu", identity: identity)
        XCTAssertTrue(navigation.hasOwnedModal)
        XCTAssertEqual(dismissed, 1)
    }
    @MainActor
    func testChoiceReturnOverrideRequiresParentOrderAndNativeAcknowledgment() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["jump", "item:416"], columns: 1)
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.closeModal(returnFocus: "unknown")
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "jump")
        navigation.openModal(scope: "jump-menu", opener: "jump") {}
        navigation.closeModal(returnFocus: "item:416")
        navigation.modalPresentationDismissed(scope: "jump-menu")
        XCTAssertEqual(navigation.requestedFocus, "item:416")
        XCTAssertNil(navigation.focusedKey)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
        navigation.register(scope: "home", key: "item:416", entry: .init(id: UUID(), label: "Movie", frame: .init(x: 0, y: 0, width: 100, height: 60)) {})
        navigation.physicalFocus(scope: "home", key: "item:416")
        XCTAssertEqual(navigation.focusedKey, "item:416")
    }
    @MainActor
    func testFocusControlRequiresScopeAndOrderAndNativeAcknowledgment() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["lazy"], columns: 1)
        XCTAssertFalse(navigation.focusControl(scope: "other", key: "lazy"))
        XCTAssertFalse(navigation.focusControl(scope: "home", key: "missing"))
        XCTAssertTrue(navigation.focusControl(scope: "home", key: "lazy"))
        let revision = navigation.focusRevision
        XCTAssertTrue(navigation.focusControl(scope: "home", key: "lazy"))
        XCTAssertGreaterThan(navigation.focusRevision, revision)
        XCTAssertNil(navigation.focusedKey)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
    }
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
    @MainActor
    func testOwnedDestinationBackRouteReplacementAndLateDisposalStayScoped() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["opener", "fallback"], columns: 1)
        let first = UUID(), replacement = UUID()
        var dismissed = 0
        XCTAssertTrue(navigation.attachDestination(token: first, parent: "home", scope: "library:expanded", opener: "opener") { dismissed += 1 })
        navigation.setOrder(scope: "library:expanded", keys: ["item:1", "item:2"], columns: 1)
        let oldContext = navigation.context
        navigation.navigate(to: .item(1))
        XCTAssertEqual(navigation.activeScope, "item:1")
        XCTAssertEqual(navigation.dispatch(.select, context: oldContext), .staleContext)
        navigation.detachDestination(token: first)
        XCTAssertEqual(navigation.activeScope, "item:1")
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .applied)
        XCTAssertTrue(navigation.attachDestination(token: replacement, parent: "home", scope: "library:replacement", opener: "opener") { dismissed += 1 })
        navigation.detachDestination(token: first)
        XCTAssertEqual(navigation.activeScope, "library:replacement")
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .applied)
        XCTAssertEqual(navigation.activeScope, "home"); XCTAssertEqual(navigation.requestedFocus, "opener")
        XCTAssertEqual(dismissed, 1)
    }
    @MainActor
    func testOwnedSharedPresentationExitProtectsUnderlyingDetailAndReplacement() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.navigate(to: .item(1))
        let owner = UUID(), replacement = UUID()
        navigation.attachPresentation(owner, scope: "playback:shared:owner", controllers: [])
        XCTAssertEqual(navigation.dispatch(.back, context: navigation.context), .unsupported)
        XCTAssertEqual(navigation.dispatch(.home, context: navigation.context), .unsupported)
        navigation.detachPresentation(owner)
        XCTAssertEqual(navigation.activeScope, "item:1")
        navigation.attachPresentation(replacement, scope: "playback:shared:replacement", controllers: [])
        navigation.detachPresentation(owner)
        XCTAssertEqual(navigation.activeScope, "playback:shared:replacement")
        navigation.detachPresentation(replacement)
        XCTAssertEqual(navigation.dispatch(.home, context: navigation.context), .applied)
        XCTAssertEqual(navigation.activeScope, "home")
    }

    func testEpisodeContextChangeRetiresLoadingAndAllowsFreshRetryWithoutClearingReplacement() throws {
        var attempt = EpisodePlayAttempt()
        let old = try XCTUnwrap(attempt.begin(itemID: 1))
        // The awaited detail result loses its route/context permission. Its
        // owned loading completion still runs, permitting another Play.
        attempt.finish(old)
        XCTAssertNil(attempt.itemID)
        let retry = try XCTUnwrap(attempt.begin(itemID: 2))
        attempt.finish(old)
        XCTAssertEqual(attempt.itemID, 2)
        XCTAssertNil(attempt.begin(itemID: 3))
        attempt.finish(retry)
        XCTAssertNotNil(attempt.begin(itemID: 3))
    }

    @MainActor
    func testStationarySharedPhysicalActionRetiresNetworkAuthorityBeforeTransportEffect() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        let id = UUID()
        navigation.register(scope: "home", key: "stationary", entry: .init(id: id, label: "Play", frame: CGRect(x: 0, y: 0, width: 20, height: 20), activate: {}))
        navigation.physicalFocus(scope: "home", key: "stationary")
        let before = navigation.context
        var retired = false
        navigation.onPhysicalInput = { retired = true }
        navigation.performPhysicalAction {
            XCTAssertTrue(retired)
            XCTAssertNotEqual(navigation.context.focusRevision, before.focusRevision)
            XCTAssertEqual(navigation.focusedKey, "stationary")
        }
        XCTAssertEqual(navigation.dispatch(.select, context: before), .staleFocus)
    }

    @MainActor
    func testOwnedDestinationBoundsAndRealizationRequireCurrentNativeFocus() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        XCTAssertFalse(navigation.attachDestination(token: UUID(), parent: "unrelated", scope: "library:unknown", opener: nil) {})
        var parent = "home"
        for index in 0..<8 {
            let child = "library:child:\(index)"
            XCTAssertTrue(navigation.attachDestination(token: UUID(), parent: parent, scope: child, opener: nil) {})
            parent = child
        }
        XCTAssertFalse(navigation.attachDestination(token: UUID(), parent: parent, scope: "library:overflow", opener: nil) {})
        var activated = 0
        navigation.setOrder(scope: parent, keys: ["lazy"], columns: 1)
        let registration = UUID()
        navigation.register(scope: parent, key: "lazy", entry: .init(id: registration, label: "Lazy item", frame: .init(x: 0, y: 0, width: 80, height: 80)) { activated += 1 })
        XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .applied)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
        navigation.physicalFocus(scope: parent, key: "lazy")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(activated, 1)
    }

    func testRefreshedPreplaySourceAndReplacedPanelRejectCapturedChoice() {
        let token = UUID(), epoch = UUID()
        let source = RemoteOwnedChoiceSource(token: token, epoch: epoch, signature: Data("file1:audio0".utf8))
        XCTAssertTrue(source.accepts(token: token, epoch: epoch, signature: Data("file1:audio0".utf8)))
        XCTAssertFalse(source.accepts(token: token, epoch: epoch, signature: Data("file2:audio0".utf8)))
        XCTAssertFalse(source.accepts(token: token, epoch: epoch, signature: Data("file1:audio1".utf8)))
        XCTAssertFalse(source.accepts(token: UUID(), epoch: epoch, signature: source.signature))
        XCTAssertFalse(source.accepts(token: token, epoch: UUID(), signature: source.signature))
    }
    @MainActor
    func testPhysicalDestinationDismissRestoresAvailableFallbackWhenOpenerRemoved() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.setOrder(scope: "home", keys: ["opener", "fallback"], columns: 1)
        let token = UUID()
        XCTAssertTrue(navigation.attachDestination(token: token, parent: "home", scope: "library:child", opener: "opener") {})
        navigation.setOrder(scope: "home", keys: ["fallback"], columns: 1)
        navigation.detachDestination(token: token)
        XCTAssertEqual(navigation.activeScope, "home"); XCTAssertEqual(navigation.requestedFocus, "fallback")
    }

    @MainActor
    func testStaleCapturedPreplaySelectRejectsBeforeActionAndCannotCloseReplacementModal() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        let source = RemoteOwnedChoiceSource(token: UUID(), epoch: navigation.epoch, signature: Data("file1".utf8))
        var currentToken: UUID? = source.token
        var currentSignature = source.signature
        var applied = 0, dismissals = 0
        navigation.openModal(scope: "item:1:preplay", opener: "version") { dismissals += 1 }
        let registration = UUID()
        navigation.register(scope: "item:1:preplay", key: "choice:1", entry: .init(id: registration, label: "Version", frame: .init(x: 0, y: 0, width: 80, height: 60), admission: {
            source.accepts(token: currentToken, epoch: navigation.epoch, signature: currentSignature) ? nil : .staleContext
        }) { applied += 1; navigation.closeModal() })
        navigation.physicalFocus(scope: "item:1:preplay", key: "choice:1")
        currentSignature = Data("file2".utf8)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .staleContext)
        XCTAssertEqual(applied, 0); XCTAssertEqual(dismissals, 0); XCTAssertTrue(navigation.hasOwnedModal)
        currentSignature = source.signature; currentToken = UUID()
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .staleContext)
        XCTAssertEqual(applied, 0); XCTAssertEqual(dismissals, 0); XCTAssertTrue(navigation.hasOwnedModal)
    }

    @MainActor
    func testRealRegistrationRefreshesVersionClosureAtUnchangedButtonGeometry() async {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        let source = RemoteVersionRegistrationSource()
        let host = UIHostingController(rootView: RemoteVersionRegistrationButton(source: source).remoteScope("home").environmentObject(navigation))
        let window = UIWindow(frame: CGRect(x: 0, y: 0, width: 400, height: 400))
        window.rootViewController = host; window.makeKeyAndVisible()
        defer { window.isHidden = true; window.rootViewController = nil }
        host.view.setNeedsLayout(); host.view.layoutIfNeeded()
        try? await Task.sleep(for: .milliseconds(150))
        navigation.physicalFocus(scope: "home", key: "version-play")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(source.played, [1])
        let focusRevision = navigation.focusRevision
        source.version = 2
        try? await Task.sleep(for: .milliseconds(150))
        XCTAssertEqual(navigation.focusRevision, focusRevision)
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .applied)
        XCTAssertEqual(source.played, [1, 2])
    }

}


@MainActor
private final class RemoteVersionRegistrationSource: ObservableObject {
    @Published var version = 1
    var played: [Int] = []
}
private struct RemoteVersionRegistrationButton: View {
    @ObservedObject var source: RemoteVersionRegistrationSource
    var body: some View {
        let version = source.version
        Button("Play") { }.frame(width: 100, height: 60)
            .remoteControl("version-play", label: "Play") { source.played.append(version) }
    }
}

@MainActor
private final class RemoteFocusUpdates {
    var renders = 0
    var focusedKeys: [String?] = []
    var inputRevisions: [UInt64] = []
}

private struct RemoteFocusUpdateFixture: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let updates: RemoteFocusUpdates
    var body: some View {
        updates.renders += 1
        return Text("Focus observation")
            .onRemoteFocusChange(navigation) { updates.focusedKeys.append(navigation.focusedKey) }
            .onRemotePhysicalInput(navigation) { updates.inputRevisions.append($0) }
    }
}
