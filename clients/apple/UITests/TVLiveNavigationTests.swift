#if os(tvOS)
import XCTest

@MainActor
final class TVLiveNavigationTests: TVNavigationTestCase {
    func testGuide77ChannelsKeepsNativeFocusVisibleAndResponsive() throws {
        let server = try TVNavigationServer(guideChannelCount: 77)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let tab = app.viewerTab("Live TV")
        reachForSetup(tab, in: app, limit: 8)
        remote.press(.select)
        let guide = app.buttons["Guide"]
        reachForSetup(guide, in: app, limit: 16)
        assertFocus(guide, in: app, context: "Full lineup Guide opener")
        remote.press(.select)
        func cell(_ row: Int) -> XCUIElement {
            app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "\(100 + row) · Guide station \(row), Programme \(row) slot 0,")).firstMatch
        }
        assertFocus(cell(0), in: app, context: "Guide enters current first programme")
        let grid = app.scrollViews["live-guide-grid"]
        XCTAssertTrue(grid.exists)
        var bounds = grid.frame
        bounds.origin.y += 42 // The pinned time heading is not a programme viewport.
        bounds.size.height -= 42
        func step(_ direction: XCUIRemote.Button, to row: Int) {
            let began = Date()
            remote.press(direction)
            let target = cell(row)
            let visibleFocus = {
                // Observe focus and geometry together without repeating the
                // remote accessibility query for each attribute on every row.
                guard let snapshot = try? target.snapshot() else { return false }
                return snapshot.hasFocus && bounds.contains(snapshot.frame)
            }
            // Check immediately: XCTest's predicate waiter otherwise adds a
            // one-second polling floor to every already-completed remote move.
            if !visibleFocus() {
                let focus = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in visibleFocus() }, object: nil)
                let result = XCTWaiter.wait(for: [focus], timeout: 2)
                if result != .completed { print("GUIDE_STEP_FAILURE=\(app.debugDescription)") }
                XCTAssertEqual(result, .completed,
                               "Remote press must promptly reveal programme row \(row)")
            }
            XCTAssertLessThan(Date().timeIntervalSince(began), 3,
                              "Native focus queries must not stall the app")
        }
        for row in 1...76 { step(.down, to: row) }
        step(.down, to: 76)
        assertFocus(cell(76), in: app, context: "Last of 77 guide channels stays visible")
        remote.press(.select)
        XCTAssertTrue(app.buttons["Watch"].waitForExistence(timeout: 3), app.debugDescription)
        remote.press(.menu)
        assertFocus(cell(76), in: app, context: "Programme Back restores visible last guide cell")
        for row in stride(from: 75, through: 0, by: -1) { step(.up, to: row) }
        assertFocus(cell(0), in: app, context: "Guide returns to first channel after full traversal")

    }

    func testGuideToolsAndTimeReturnKeepNativeFocus() throws {
        let server = try TVNavigationServer(guideChannelCount: 77)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let tab = app.viewerTab("Live TV")
        reachForSetup(tab, in: app, limit: 8)
        remote.press(.select)
        let guide = app.buttons["Guide"]
        reachForSetup(guide, in: app, limit: 16)
        assertFocus(guide, in: app, context: "Full lineup Guide opener")
        remote.press(.select)
        func cell(_ row: Int) -> XCUIElement {
            app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "\(100 + row) · Guide station \(row), Programme \(row) slot 0,")).firstMatch
        }
        assertFocus(cell(0), in: app, context: "Guide enters current first programme")
        remote.press(.up)
        assertFocus(app.buttons["Watch"], in: app, context: "Guide Up reaches programme actions")
        remote.press(.up)
        assertFocus(guide, in: app, context: "Programme Up returns to Guide toolbar")
        for label in ["Recordings", "Search", "Guide time"] {
            remote.press(.right)
            let button = app.buttons.matching(NSPredicate(format: "label == %@ AND hasFocus == true", label)).firstMatch
            XCTAssertTrue(button.waitForExistence(timeout: 2), "Toolbar Right reaches \(label)")
        }
        let time = app.buttons["Guide time"]
        remote.press(.select)
        assertFocus(app.buttons["choice:earlier"], in: app, context: "Guide time owns focus")
        remote.press(.down)
        assertFocus(app.buttons["choice:now"], in: app, context: "Guide time Now reachable")
        remote.press(.select)
        assertFocus(time, in: app, context: "Guide time restores exact opener")
        remote.press(.down)
        XCTAssertFalse(time.hasFocus, "Down must leave Guide time for content")
        for _ in 0..<3 {
            if time.hasFocus { break }
            remote.press(.up)
        }
        assertFocus(time, in: app, context: "Content Up restores its exact Guide time toolbar origin")
        remote.press(.up)
        assertFocus(tab, in: app, context: "Full lineup leaves root tabs responsive")

    }

    func testLiveFullscreenRevealMoreReportAndBackRetainSession() throws {
        let server = try TVNavigationServer(activeDvrRecording: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let tab = app.viewerTab("Live TV")
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
        assertHiddenFixtureVideoIsUnobstructed(in: app)
        remote.press(.right)
        assertFocus(pause, in: app, context: "Live first Right reveals Pause", captureOnSuccess: false)
        remote.press(.up)
        let activity = app.buttons.matching(NSPredicate(format: "label == %@", "Recording activity")).firstMatch
        assertFocus(activity, in: app, context: "Live Up reaches Recording activity", captureOnSuccess: false)
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["1 recording"].waitForExistence(timeout: 5), app.debugDescription)
        // Deliberately cross the four-second idle deadline while the sheet owns focus.
        let afterHideDeadline = Date().addingTimeInterval(5)
        let pastDeadline = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in Date() >= afterHideDeadline }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [pastDeadline], timeout: 7), .completed)
        remote.press(.menu)
        assertFocus(activity, in: app, context: "Recording activity Back restores its exact opener", captureOnSuccess: false)
        remote.press(.down)
        assertFocus(pause, in: app, context: "Recording activity returns to playback controls", captureOnSuccess: false)
        let more = app.buttons["live-player-more"]
        XCTAssertEqual(pause.label, "Pause", "The reveal press must not pause playback")
        let leftEdgeDeadline = Date().addingTimeInterval(5)
        repeat {
            remote.press(.left)
            assertFocus(pause, in: app, context: "Left edge retains Pause during active input", captureOnSuccess: false)
        } while Date() < leftEdgeDeadline
        // Exercise the actual four-step transport row. Generic geometric
        // discovery can spend the entire idle-hide interval taking snapshots
        // without delivering any remote input.
        for (identifier, name) in [("live-player-guide", "Guide"), ("live-player-channels", "Channels"), ("live-player-info", "Info"), ("live-player-more", "More")] {
            remote.press(.right)
            assertFocus(app.buttons[identifier], in: app, context: "Live Right reaches " + name, captureOnSuccess: false)
            if identifier == "live-player-guide" {
                remote.press(.select)
                let programme = app.buttons.matching(NSPredicate(format:
                    "identifier BEGINSWITH 'live-guide-programme:news:'"
                )).firstMatch
                assertFocus(programme, in: app, context: "Fullscreen guide enters the playing channel programme")
                remote.press(.menu)
                assertFocus(app.buttons[identifier], in: app, context: "Fullscreen guide Back restores Guide pill", captureOnSuccess: false)
                remote.press(.select)
                assertFocus(programme, in: app, context: "Fullscreen guide reopens at the playing programme")
                remote.press(.up)
                assertFocus(app.buttons["Close"], in: app, context: "Fullscreen guide Up reaches Close")
                remote.press(.select)
                assertFocus(app.buttons[identifier], in: app, context: "Fullscreen guide Close restores Guide pill", captureOnSuccess: false)
                // Restoration must release ownership to the next remote move,
                // including repeated moves across the former idle deadline.
                for pass in 1...20 {
                    remote.press(.right)
                    assertFocus(app.buttons["live-player-channels"], in: app,
                                context: "Guide return Right reaches Channels pass \(pass)",
                                captureOnSuccess: false)
                    remote.press(.left)
                    assertFocus(app.buttons[identifier], in: app,
                                context: "Guide return Left reaches Guide pass \(pass)",
                                captureOnSuccess: false)
                }
            }
        }
        let rightEdgeDeadline = Date().addingTimeInterval(5)
        repeat {
            remote.press(.right)
            assertFocus(more, in: app, context: "Right edge retains More during active input", captureOnSuccess: false)
        } while Date() < rightEdgeDeadline
        remote.press(.select)
        let report = app.buttons["diagnostics-open"]
        reachForSetup(report, in: app, limit: 10)
        assertFocus(report, in: app, context: "Live Report opener")
        remote.press(.select)
        assertFocus(app.buttons["diagnostics-done"], in: app, context: "Live Report safe entry")
        remote.press(.menu)
        assertFocus(report, in: app, context: "Live Report Back restores opener")
        remote.press(.menu)
        assertFocus(more, in: app, context: "Live More Back restores pill", captureOnSuccess: false)
        remote.press(.menu)
        assertFocus(app.buttons["live-player-reveal"], in: app,
                    context: "Live controls Back returns to hidden video", timeout: 2)
        remote.press(.menu)
        assertFocus(channel, in: app, context: "Live fullscreen Back restores channel")
        XCTAssertEqual(server.requestCount("POST /api/v1/live-tv/channels/news/sessions"), 1)
    }
}
#endif
