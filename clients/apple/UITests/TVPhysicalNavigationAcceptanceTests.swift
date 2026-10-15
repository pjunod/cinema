#if os(tvOS)
import XCTest

/// Read-only acceptance on a paired Apple TV with its existing saved sign-in.
/// No fixture server, launch overrides, preference changes or playback starts.
@MainActor
final class TVPhysicalNavigationAcceptanceTests: TVNavigationTestCase {
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
