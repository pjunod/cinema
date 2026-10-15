#if os(tvOS)
import XCTest

@MainActor
final class TVConnectionNavigationTests: TVNavigationTestCase {
    func testSavedConnectionRecoveryReportAndRetryKeepCredentials() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        app.terminate()
        server.setConnectionFailure(true)
        app.launch()
        let retry = app.buttons["Try again"]
        XCTAssertTrue(retry.waitForExistence(timeout: 5), app.debugDescription)
        reachForSetup(retry, in: app, limit: 4)
        assertFocus(retry, in: app, context: "Saved connection Retry")
        let report = app.buttons["diagnostics-open"]
        XCTAssertEqual(report.label, "Help")
        reachForSetup(report, in: app, limit: 4)
        assertFocus(report, in: app, context: "Connection report stays in the form")
        remote.press(.select)
        assertFocus(app.buttons["diagnostics-done"], in: app, context: "Connection report safe entry")
        remote.press(.menu)
        assertFocus(report, in: app, context: "Connection report restores its opener")
        server.setConnectionFailure(false)
        reachForSetup(retry, in: app, limit: 4)
        assertFocus(retry, in: app, context: "Retry after connection recovers")
        let signIns = server.requestCount("POST /api/v1/auth/login")
        remote.press(.select)
        XCTAssertTrue(app.tabBars.buttons["Home"].waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(server.requestCount("POST /api/v1/auth/login"), signIns,
                       "Retry must reuse the saved sign-in")
    }
}
#endif
