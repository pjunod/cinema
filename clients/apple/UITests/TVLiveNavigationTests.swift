#if os(tvOS)
import XCTest

@MainActor
final class TVLiveNavigationTests: TVNavigationTestCase {
    func testLiveFullscreenRevealMoreReportAndBackRetainSession() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let tab = app.tabBars.buttons["Live TV"]
        reachForSetup(tab, in: app, limit: 8)
        assertFocus(tab, in: app, context: "Live TV tab")
        remote.press(.select)
        let channel = app.buttons["live-tv-channel-12.1"]
        reachForSetup(channel, in: app, limit: 8)
        assertFocus(channel, in: app, context: "Live channel by number")
        remote.press(.select)
        let fullscreen = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Select · Fullscreen")).firstMatch
        reachForSetup(fullscreen, in: app, limit: 10)
        assertFocus(fullscreen, in: app, context: "Live fullscreen opener")
        remote.press(.select)
        let pause = app.buttons["live-player-play"]
        XCTAssertTrue(pause.waitForExistence(timeout: 15), app.debugDescription)
        let hidden = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !pause.exists }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [hidden], timeout: 10), .completed, app.debugDescription)
        assertFocus(app.buttons["live-player-reveal"], in: app, context: "Hidden Live TV owns remote focus")
        remote.press(.right)
        assertFocus(pause, in: app, context: "Live first Right reveals Pause")
        let more = app.buttons["live-player-more"]
        XCTAssertEqual(pause.label, "Pause", "The reveal press must not pause playback")
        // Exercise the actual four-step transport row. Generic geometric
        // discovery can spend the entire idle-hide interval taking snapshots
        // without delivering any remote input.
        for (identifier, name) in [("live-player-guide", "Guide"), ("live-player-channels", "Channels"), ("live-player-info", "Info"), ("live-player-more", "More")] {
            remote.press(.right)
            assertFocus(app.buttons[identifier], in: app, context: "Live Right reaches " + name)
        }
        remote.press(.select)
        let report = app.buttons["diagnostics-open"]
        reachForSetup(report, in: app, limit: 10)
        assertFocus(report, in: app, context: "Live Report opener")
        remote.press(.select)
        assertFocus(app.buttons["diagnostics-done"], in: app, context: "Live Report safe entry")
        remote.press(.menu)
        assertFocus(report, in: app, context: "Live Report Back restores opener")
        remote.press(.menu)
        assertFocus(more, in: app, context: "Live More Back restores pill")
        remote.press(.menu)
        assertFocus(app.buttons["live-player-reveal"], in: app, context: "Live controls Back returns to hidden video")
        remote.press(.menu)
        assertFocus(channel, in: app, context: "Live fullscreen Back restores channel")
        XCTAssertEqual(server.requestCount("POST /api/v1/live-tv/channels/news/sessions"), 1)
    }
}
#endif
