#if os(tvOS)
import XCTest

@MainActor
final class TVMeridianNavigationTests: TVNavigationTestCase {
    func testPanelOwnsNativeFocusAndBackPreservesLibraryRoute() throws {
        let server = try TVNavigationServer(homeTitles: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, layout: .meridian)
        defer { app.terminate() }
        let opener = app.buttons["meridian-navigation-toggle"]
        reachForSetup(opener, in: app, limit: 12)
        assertFocus(opener, in: app, context: "Meridian explicit rail opener")
        remote.press(.select)
        let current = app.viewerTab("Libraries")
        assertFocus(current, in: app, context: "Expanded panel realizes selected destination")
        let attachment = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        attachment.name = "Meridian TV expanded panel — actual native focus"
        attachment.lifetime = .keepAlways
        add(attachment)
        remote.press(.menu)
        assertFocus(opener, in: app, context: "Back closes panel to its valid library opener")
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].exists,
            "Panel cancellation must preserve the exact library route")
        remote.press(.select)
        assertFocus(current, in: app, context: "Reopening panel preserves current primary")
        remote.press(.select)
        assertFocus(opener, in: app, context: "Selecting current primary preserves nested history")
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].exists)
        remote.press(.select)
        assertFocus(current, in: app, context: "Source selection starts in the current Libraries panel")
        let source = app.buttons["meridian-source-share:1"]
        reachForSetup(source, in: app, limit: 10)
        assertFocus(source, in: app, context: "Explicit Movies source is physically focused")
        remote.press(.select)
        let tools = app.buttons["library:view"]
        assertFocus(tools, in: app, context: "Changed source realizes its visible tools instead of a stale category opener")
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].exists)
        reachForSetup(opener, in: app, limit: 8)
        remote.press(.select)
        assertFocus(current, in: app, context: "Changed source still belongs to Libraries")
        let home = app.viewerTab("Home")
        reachForSetup(home, in: app, limit: 3)
        assertFocus(home, in: app, context: "Different primary selection remains explicit")
        remote.press(.select)
        assertFocus(app.buttons["theater-play"], in: app, context: "Different primary realizes actual Home content focus")
        reachForSetup(opener, in: app, limit: 8)
        remote.press(.select)
        assertFocus(home, in: app, context: "Home panel selects its current destination")
        reachForSetup(current, in: app, limit: 3)
        remote.press(.select)
        assertFocus(tools, in: app, context: "Returning primary retains the explicit library source and tools")
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].exists)
        XCTAssertEqual(server.requestCount("GET /api/v1/items/1"), 0, "Chrome movement cannot prepare or start playback")
    }
}
#endif
