#if os(tvOS)
import XCTest

@MainActor
final class TVRemoteManagementNavigationTests: TVNavigationTestCase {
    func testPairingRevocationAndRegistrationResetStartSafeAndRestoreOpeners() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let settings = app.viewerTab("Settings")
        reachForSetup(settings, in: app, limit: 8)
        remote.press(.down)
        // Native tabs enter the nearest Settings control, which can be in
        // the detail column. Establish the category column before scrolling
        // to Developer below the fold.
        let playback = app.buttons["settings-category-Playback"]
        reachForSetup(playback, in: app, limit: 8)
        assertFocus(playback, in: app, context: "Settings category column")
        let developer = app.buttons["settings-category-Developer"]
        reachForSetup(developer, in: app, limit: 12)
        assertFocus(developer, in: app, context: "Developer category")
        remote.press(.right)
        let features = app.buttons["Shared libraries, Cinema remotes and other features awaiting evidence"]
        reachForSetup(features, in: app, limit: 6)
        assertFocus(features, in: app, context: "Developer settings entry")
        remote.press(.select)
        let enable = app.cells.matching(NSPredicate(format: "label == %@", "Enable Cinema remotes on this installation")).firstMatch
        reachForSetup(enable, in: app, limit: 14)
        assertFocus(enable, in: app, context: "Cinema remotes switch")
        let toggle = app.switches["Enable Cinema remotes on this installation"].firstMatch
        if toggle.value as? String == "0" { remote.press(.select) }
        let registered = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            server.requestCount("POST /api/remote/v1/receivers") > 0
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [registered], timeout: 15), .completed, app.debugDescription)
        remote.press(.menu)
        assertFocus(features, in: app, context: "Developer Back restores settings entry")
        remote.press(.menu)
        assertFocus(developer, in: app, context: "Developer settings Back restores category")
        let remotes = app.buttons["settings-category-Remotes & devices"]
        reachForSetup(remotes, in: app, limit: 12)
        assertFocus(remotes, in: app, context: "Remotes category")
        remote.press(.right)
        let manage = app.buttons["Manage remote pairings"]
        reachForSetup(manage, in: app, limit: 5)
        assertFocus(manage, in: app, context: "Remote management entry")
        remote.press(.select)
        let revoke = app.buttons["grant:55555555-5555-4555-8555-555555555555"]
        reachForSetup(revoke, in: app, limit: 6)
        assertFocus(revoke, in: app, context: "Paired phone revoke opener")
        remote.press(.select)
        let cancel = app.buttons["secondary-confirm-cancel"]
        assertFocus(cancel, in: app, context: "Revoke defaults to Cancel")
        remote.press(.menu)
        assertFocus(revoke, in: app, context: "Revoke Back restores phone")
        XCTAssertEqual(server.requestCount("DELETE /api/remote/v1/grants/55555555-5555-4555-8555-555555555555"), 0)
        let reset = app.buttons["remotes-reset-registration"]
        reachForSetup(reset, in: app, limit: 6)
        assertFocus(reset, in: app, context: "Reset registration opener")
        remote.press(.select)
        assertFocus(cancel, in: app, context: "Reset defaults to Cancel")
        remote.press(.select)
        assertFocus(reset, in: app, context: "Reset Cancel restores exact opener")
        XCTAssertEqual(server.requestCount("DELETE /api/remote/v1/receivers/44444444-4444-4444-8444-444444444444"), 0)
    }
}
#endif
