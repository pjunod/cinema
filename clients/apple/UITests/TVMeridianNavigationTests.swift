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
    }
}
#endif
