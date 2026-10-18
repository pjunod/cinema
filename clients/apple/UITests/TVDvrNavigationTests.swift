#if os(tvOS)
import XCTest

@MainActor
final class TVDvrNavigationTests: TVNavigationTestCase {
    func testRecordingDetailsRuleConfirmationAndManualChannelReturn() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let tab = app.viewerTab("Recordings")
        reachForSetup(tab, in: app, limit: 8)
        assertFocus(tab, in: app, context: "Recordings tab")
        remote.press(.down)
        let row = app.buttons["recording:scheduled"]
        reachForSetup(row, in: app, limit: 8)
        assertFocus(row, in: app, context: "Recording title opens details")
        remote.press(.select)
        XCTAssertTrue(app.navigationBars["Recording details"].waitForExistence(timeout: 8), app.debugDescription)
        XCTAssertTrue(app.staticTexts["History"].exists)
        remote.press(.menu)
        assertFocus(row, in: app, context: "Recording detail Back restores title")
        let rules = app.buttons["chip:rules"]
        reachForSetup(rules, in: app, limit: 8)
        assertFocus(rules, in: app, context: "Series rules tab")
        remote.press(.select)
        let rule = app.buttons["rule:rule-1"]
        reachForSetup(rule, in: app, limit: 5)
        assertFocus(rule, in: app, context: "Rule options opener")
        remote.press(.select)
        let delete = app.buttons["Delete rule…"]
        reachForSetup(delete, in: app, limit: 4)
        assertFocus(delete, in: app, context: "Delete rule requires confirmation")
        remote.press(.select)
        let keep = app.buttons["Keep rule"]
        assertFocus(keep, in: app, context: "Rule confirmation starts on safe Keep")
        XCTAssertEqual(server.requestCount("DELETE /api/v1/dvr/rules/rule-1"), 0)
        remote.press(.menu)
        assertFocus(rule, in: app, context: "Confirmation Back restores rule")
        XCTAssertEqual(server.requestCount("DELETE /api/v1/dvr/rules/rule-1"), 0)
        let manual = app.buttons["chip:manual"]
        reachForSetup(manual, in: app, limit: 8)
        assertFocus(manual, in: app, context: "Manual recording tab")
        remote.press(.select)
        let channel = app.buttons["dvr-manual-channel"]
        reachForSetup(channel, in: app, limit: 6)
        assertFocus(channel, in: app, context: "Channel name picker opener")
        remote.press(.select)
        let news = app.buttons["dvr-channel-news"]
        reachForSetup(news, in: app, limit: 4)
        assertFocus(news, in: app, context: "Choose human-readable channel")
        remote.press(.select)
        assertFocus(channel, in: app, context: "Channel choice restores manual field")
        XCTAssertTrue(channel.label.contains("12.1"), channel.label)
        XCTAssertEqual(server.requestCount("POST /api/v1/dvr/recordings"), 0, "Choosing a channel must not schedule a recording")
    }
}
#endif
