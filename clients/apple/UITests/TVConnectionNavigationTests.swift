#if os(tvOS)
import XCTest

@MainActor
final class TVConnectionNavigationTests: TVNavigationTestCase {
    func testSignInCanChooseAnotherServerAndReconnectWithNativeRemote() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openSignIn(origin: origin)
        defer { app.terminate() }
        XCTAssertFalse(app.buttons["diagnostics-open"].exists, "Credential sign-in does not offer Report a problem")
        let change = app.buttons["Use a different server"]
        reachForSetup(change, in: app, limit: 8)
        assertFocus(change, in: app, context: "Sign in can choose another server")
        remote.press(.select)
        let manual = app.buttons["Add server manually"]
        assertFocus(manual, in: app, context: "Server picker remains reachable after leaving Sign in")
        connectManually(origin: origin, in: app)
        signInToFixture(in: app)
        XCTAssertTrue(app.viewerTab("Home").waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(server.requestCount("POST /api/v1/auth/login"), 1)
    }

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
        remote.press(.down)
        assertFocus(app.buttons["Use a different server"], in: app, context: "Connection server choice below Retry")
        let report = app.buttons["diagnostics-open"]
        XCTAssertEqual(report.label, "Help")
        remote.press(.down)
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
        XCTAssertTrue(app.viewerTab("Home").waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(server.requestCount("POST /api/v1/auth/login"), signIns,
                       "Retry must reuse the saved sign-in")
    }
}
#endif
