#if os(tvOS)
import XCTest

/// Read-only acceptance on a paired Apple TV with its existing saved sign-in.
/// No fixture server, launch overrides, preference changes or playback starts.
@MainActor
final class TVPhysicalNavigationAcceptanceTests: TVNavigationTestCase {
    func testPhysicalFullGuideRemainsVisibleAndResponsive() throws {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        defer { app.terminate() }
        let tab = app.tabBars.buttons["Live TV"]
        XCTAssertTrue(tab.waitForExistence(timeout: 30),
                      "Physical acceptance requires the device's existing saved sign-in")
        reachForSetup(tab, in: app, limit: 8)
        assertFocus(tab, in: app, context: "Physical Live TV tab")
        remote.press(.select)
        let guide = app.buttons["Guide"]
        reachForSetup(guide, in: app, limit: 16)
        assertFocus(guide, in: app, context: "Physical Guide opener")
        remote.press(.select)
        let grid = app.scrollViews["live-guide-grid"]
        XCTAssertTrue(grid.waitForExistence(timeout: 15))
        let summary = app.staticTexts.matching(NSPredicate(format: "label MATCHES %@", "[0-9]+ channels")).firstMatch
        XCTAssertTrue(summary.exists)
        let count = try XCTUnwrap(Int(summary.label.split(separator: " ")[0]))
        XCTAssertGreaterThan(count, 1)
        var bounds = grid.frame
        bounds.origin.y += 42
        bounds.size.height -= 42
        func focusedCell() -> XCUIElement {
            app.buttons.matching(NSPredicate(format:
                "(identifier BEGINSWITH 'live-guide-programme:' OR identifier BEGINSWITH 'live-guide-empty:') AND hasFocus == true"
            )).firstMatch
        }
        let initial = focusedCell()
        XCTAssertTrue(initial.waitForExistence(timeout: 3))
        assertFocus(initial, in: app, context: "Physical guide first visible programme")
        func step(_ direction: XCUIRemote.Button) {
            let previous = focusedCell().identifier
            let began = Date()
            remote.press(direction)
            let visibleFocus = {
                let target = focusedCell()
                return target.exists && target.identifier != previous && bounds.contains(target.frame)
            }
            if !visibleFocus() {
                let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in visibleFocus() }, object: nil)
                XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 2), .completed,
                               "Physical guide must move promptly to another fully visible row")
            }
            XCTAssertLessThan(Date().timeIntervalSince(began), 3)
        }
        for _ in 1..<count { step(.down) }
        assertFocus(focusedCell(), in: app, context: "Physical guide last channel visible")
        for _ in 1..<count { step(.up) }
        assertFocus(focusedCell(), in: app, context: "Physical guide first channel restored")
        remote.press(.menu)
        assertFocus(tab, in: app, context: "Physical guide leaves root navigation responsive")
    }

    func testPhysicalLibraryMenusOwnFocusAndRestoreEachOpener() throws {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launch()
        defer { app.terminate() }
        let libraries = app.tabBars.buttons["Libraries"]
        XCTAssertTrue(libraries.waitForExistence(timeout: 30),
                      "Physical acceptance requires the device's existing saved sign-in")
        reachForSetup(libraries, in: app, limit: 8)
        assertFocus(libraries, in: app, context: "Physical Libraries tab")
        remote.press(.select)
        let movieCategory = app.buttons["library-open-category:movie"]
        let collection = movieCategory.exists ? movieCategory : app.buttons.matching(
            NSPredicate(format: "identifier BEGINSWITH 'library-open-share:'")
        ).firstMatch
        reachForSetup(collection, in: app, limit: 8)
        assertFocus(collection, in: app, context: "Physical saved collection")
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].waitForExistence(timeout: 30))
        XCTAssertFalse(app.buttons["diagnostics-open"].exists,
                       "Browsing must not expose a global reporting overlay")
        for key in ["view", "sort", "filter"] {
            let opener = app.buttons["library:" + key]
            reachForSetup(opener, in: app, limit: 8)
            assertFocus(opener, in: app, context: "Physical " + key + " opener")
            remote.press(.select)
            let selected = app.buttons.matching(NSPredicate(
                format: "identifier BEGINSWITH 'choice:' AND selected == true"
            )).firstMatch
            assertFocus(selected, in: app, context: "Physical " + key + " selected choice")
            remote.press(.menu)
            assertFocus(opener, in: app, context: "Physical " + key + " exact Back return")
        }
        remote.press(.menu)
        assertFocus(collection, in: app, context: "Physical collection Back restores its opener")
    }
}
#endif
