#if os(tvOS)
import XCTest

@MainActor
final class TVHomeNavigationTests: TVNavigationTestCase {
    func testForegroundRefreshRetainsHomeContentAndRetryRecovers() throws {
        let server = try TVNavigationServer(homeTitles: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let home = app.viewerTab("Home")
        reachForSetup(home, in: app, limit: 8)
        remote.press(.select)
        let title = app.staticTexts["A Movie 0"].firstMatch
        XCTAssertTrue(title.waitForExistence(timeout: 10), app.debugDescription)
        server.setHomeFailure(true)
        remote.press(.home)
        XCTAssertTrue(app.wait(for: .runningBackground, timeout: 10))
        app.activate()
        let retry = app.buttons["home:retry"]
        XCTAssertTrue(retry.waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertTrue(title.exists, "A failed refresh must retain the previous Home titles")
        reachForSetup(retry, in: app, limit: 4)
        assertFocus(retry, in: app, context: "Home Retry beside retained content")
        server.setHomeFailure(false)
        let before = server.requestCount("GET /api/v1/hubs")
        remote.press(.select)
        let recovered = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !retry.exists }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [recovered], timeout: 10), .completed)
        XCTAssertGreaterThan(server.requestCount("GET /api/v1/hubs"), before)
        XCTAssertTrue(title.exists)
        assertFocus(app.buttons["A Movie 0"].firstMatch, in: app, context: "Home Retry returns to retained title")
    }
}
#endif
