#if os(tvOS)
import XCTest

/// Shipping authentication, channel controller, video surface and remote input.
/// All media and JSON come from the loopback fixture; no product entry hooks.
@MainActor
final class TVPlaybackNavigationTests: TVNavigationTestCase {
    func testLocalMovieHiddenRevealReportAndBackRetainPlayback() throws {
        let server = try TVNavigationServer(playableTitles: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let movie = app.buttons["item:1"]
        reachForSetup(movie, in: app, limit: 8)
        assertFocus(movie, in: app, context: "Local movie opener")
        remote.press(.select)
        let play = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "Play")).firstMatch
        reachForSetup(play, in: app, limit: 8)
        assertFocus(play, in: app, context: "Local movie Play")
        remote.press(.select)
        let pause = app.buttons["player-play-pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 20), app.debugDescription)
        waitUntil("Local video was requested", app: app, timeout: 15) { server.requestCount("/fixture.mp4") > 0 }
        waitUntil("Local controls hide", app: app, timeout: 10) { !pause.exists }
        XCTAssertFalse(app.buttons["diagnostics-open"].exists)
        assertFocus(app.buttons["player-reveal"], in: app, context: "Hidden local video owns remote focus")
        assertHiddenFixtureVideoIsUnobstructed(in: app)
        remote.press(.right)
        assertFocus(pause, in: app, context: "Local first Right reveals Play Pause")
        let decisions = server.requestCount("/api/v1/files/1/decision")
        remote.press(.select)
        let more = app.buttons["player-more"]
        reachForSetup(more, in: app, limit: 10)
        assertFocus(more, in: app, context: "Local More")
        remote.press(.select)
        let report = app.buttons["diagnostics-open"]
        reachForSetup(report, in: app, limit: 10)
        assertFocus(report, in: app, context: "Local Report belongs to More")
        remote.press(.select)
        assertFocus(app.buttons["diagnostics-done"], in: app, context: "Local report safe entry")
        remote.press(.menu)
        assertFocus(report, in: app, context: "Local report Back restores opener")
        remote.press(.menu)
        assertFocus(more, in: app, context: "Local More Back restores More")
        XCTAssertEqual(server.requestCount("/api/v1/files/1/decision"), decisions)
        remote.press(.menu)
        XCTAssertFalse(pause.exists)
        remote.press(.menu)
        XCTAssertTrue(app.buttons["detail:about"].waitForExistence(timeout: 10), app.debugDescription)
    }

    func testLocalMovieTimelineSelectCommitsForwardAndBackwardPreview() throws {
        let server = try TVNavigationServer(playableTitles: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let movie = app.buttons["item:1"]
        reachForSetup(movie, in: app, limit: 8)
        assertFocus(movie, in: app, context: "Timeline movie opener")
        remote.press(.select)
        let play = app.buttons.matching(NSPredicate(format: "label ENDSWITH %@", "Play")).firstMatch
        reachForSetup(play, in: app, limit: 8)
        assertFocus(play, in: app, context: "Timeline movie Play")
        remote.press(.select)
        let pause = app.buttons["player-play-pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 20), app.debugDescription)
        reachForSetup(pause, in: app, limit: 8)
        assertFocus(pause, in: app, context: "Pause before scrubbing")
        remote.press(.select)
        waitUntil("Playback paused", app: app, timeout: 5) { pause.label == "Play" }

        let timeline = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@ AND hasFocus == true", "Playback position")).firstMatch
        remote.press(.up)
        assertFocus(timeline, in: app, context: "Timeline receives Up from transport")
        let position = app.staticTexts["player-position"]
        let initial = try timelineSeconds(position)
        remote.press(.right)
        remote.press(.right)
        waitUntil("Forward preview", app: app, timeout: 5) {
            (timeline.value as? String)?.hasPrefix("Previewing ") == true
        }
        let forward = try timelineSeconds(position)
        XCTAssertGreaterThanOrEqual(forward, initial + 20)
        remote.press(.select)
        waitUntil("Select commits forward preview", app: app, timeout: 5) {
            timeline.value as? String == "Timeline"
        }
        XCTAssertEqual(try timelineSeconds(position), forward, accuracy: 1)
        XCTAssertEqual(pause.label, "Play", "Commit must preserve paused playback")

        remote.press(.left)
        waitUntil("Backward preview", app: app, timeout: 5) {
            (timeline.value as? String)?.hasPrefix("Previewing ") == true
        }
        let backward = try timelineSeconds(position)
        XCTAssertLessThanOrEqual(backward, forward - 10)
        remote.press(.select)
        waitUntil("Select commits backward preview", app: app, timeout: 5) {
            timeline.value as? String == "Timeline"
        }
        XCTAssertEqual(try timelineSeconds(position), backward, accuracy: 1)
        XCTAssertEqual(pause.label, "Play")
        remote.press(.right)
        remote.press(.menu)
        waitUntil("Menu cancels preview", app: app, timeout: 5) {
            timeline.value as? String == "Timeline"
        }
        XCTAssertEqual(try timelineSeconds(position), backward, accuracy: 1)
        remote.press(.playPause)
        waitUntil("Committed seek resumes advancing playback", app: app, timeout: 5) {
            guard position.exists else { return false }
            return (try? self.timelineSeconds(position)) ?? 0 >= backward + 2
        }
        XCTAssertEqual(pause.label, "Pause")
        remote.press(.right)
        let playingPreview = try timelineSeconds(position)
        remote.press(.select)
        waitUntil("Select commits while playing", app: app, timeout: 3) {
            timeline.value as? String == "Timeline"
        }
        XCTAssertEqual(pause.label, "Pause", "Commit must preserve playing playback")
        XCTAssertEqual(try timelineSeconds(position), playingPreview, accuracy: 2)
        let capture = XCTAttachment(screenshot: app.screenshot())
        capture.name = "timeline-select-forward-backward"
        capture.lifetime = .keepAlways
        add(capture)
    }

    private func timelineSeconds(_ element: XCUIElement) throws -> Double {
        let label = element.label
        let parts = label.split(separator: ":").compactMap { Double($0) }
        XCTAssertTrue(parts.count == 2 || parts.count == 3, "Unexpected timeline: \(label)")
        return parts.reduce(0) { $0 * 60 + $1 }
    }

    func testLibraryChannelHiddenRevealMoreReportAndCloseRetainSession() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openChannels(origin: origin)
        defer { app.terminate() }
        tuneAndOpenFullscreen(in: app, server: server)
        let more = app.buttons["channel-player-more"]
        XCTAssertTrue(more.waitForExistence(timeout: 5), app.debugDescription)
        let sessionPath = "/api/v1/library-channels/fixture-channel/sessions"
        let tunes = server.requestCount(sessionPath)
        XCTAssertEqual(tunes, 1)
        waitUntil("Idle hides every channel control", app: app, timeout: 8) {
            !more.exists && !app.buttons["Return to channels"].exists
        }
        XCTAssertFalse(app.buttons["diagnostics-open"].exists,
                       "Healthy hidden video must not expose Report")

        let reveal = app.descendants(matching: .any)["channel-player-reveal"]
        assertFocus(reveal, in: app, context: "Hidden channel surface owns the native remote")
        assertHiddenFixtureVideoIsUnobstructed(in: app)
        remote.press(.playPause)
        XCTAssertTrue(app.buttons["Resume live"].firstMatch.waitForExistence(timeout: 5),
                      "Play/Pause must reach the existing transport owner from native reveal")
        assertFocus(reveal, in: app, context: "Pause keeps the native hidden target")
        remote.press(.playPause)
        waitUntil("Play/Pause resumes without revealing controls", app: app, timeout: 5) {
            !app.buttons["Resume live"].exists
        }
        assertFocus(reveal, in: app, context: "Resume keeps the native hidden target")
        remote.press(.right)
        assertFocus(app.buttons["channel-player-play"], in: app, context: "First Right only reveals Pause")
        XCTAssertEqual(server.requestCount(sessionPath), tunes, "Reveal must not retune")
        remote.press(.right)
        assertFocus(more, in: app, context: "Second Right reaches channel More")
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["Channel options"].waitForExistence(timeout: 5), app.debugDescription)
        let report = app.buttons["diagnostics-open"]
        assertFocus(report, in: app, context: "Channel More initially focuses its Report action")
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["diagnostics-status"].waitForExistence(timeout: 5), app.debugDescription)
        let reportClose = app.buttons["diagnostics-done"]
        reachForSetup(reportClose, in: app, limit: 8)
        assertFocus(reportClose, in: app, context: "Report Close")
        remote.press(.select)
        assertFocus(report, in: app, context: "Report Close restores Report opener inside More")
        let optionsClose = app.buttons["Close"].firstMatch
        reachForSetup(optionsClose, in: app, limit: 8)
        assertFocus(optionsClose, in: app, context: "Channel More Close")
        remote.press(.select)
        assertFocus(more, in: app, context: "Closing More restores exact channel opener")
        XCTAssertEqual(server.requestCount(sessionPath), tunes,
                       "Report and options must retain the same channel session")
        remote.press(.menu)
        assertFocus(reveal, in: app, context: "Controls Back returns to native reveal")
        remote.press(.menu)
        XCTAssertTrue(app.buttons["Fullscreen"].waitForExistence(timeout: 5),
                      "Hidden Back must reach the existing Return to channels owner")
        XCTAssertFalse(more.exists)
        XCTAssertEqual(server.requestCount(sessionPath), tunes)
    }

    func testLibraryChannelLayoutHasNamedChoicesAndRestoresOpener() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openChannels(origin: origin)
        defer { app.terminate() }
        let layout = app.buttons["channel-layout"]
        reachForSetup(layout, in: app, limit: 8)
        assertFocus(layout, in: app, context: "Channel Layout opener")
        remote.press(.select)
        assertFocus(app.buttons["choice:guide_preview"], in: app, context: "Selected Guide preview layout")
        remote.press(.down)
        assertFocus(app.buttons["choice:guide_over_picture"], in: app, context: "Named Guide over picture choice")
        remote.press(.select)
        assertFocus(layout, in: app, context: "Layout selection restores opener")
        XCTAssertTrue(layout.label.contains("Guide over picture"), layout.label)
        remote.press(.select)
        assertFocus(app.buttons["choice:guide_over_picture"], in: app, context: "Reopened layout retains selection")
        remote.press(.menu)
        assertFocus(layout, in: app, context: "Layout Back restores opener without changing selection")
        XCTAssertTrue(layout.label.contains("Guide over picture"), layout.label)

    }

    func testLibraryChannelFutureProgrammeExplainsAvailabilityAndCurrentSelectTunes() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openChannels(origin: origin)
        defer { app.terminate() }
        let sessionPath = "/api/v1/library-channels/fixture-channel/sessions"
        let future = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Future fixture")).firstMatch
        reachForSetup(future, in: app, limit: 8)
        assertFocus(future, in: app, context: "Future channel programme")
        remote.press(.select)
        let details = app.staticTexts["channel-programme-title"]
        XCTAssertTrue(details.waitForExistence(timeout: 5), app.debugDescription)
        XCTAssertEqual(details.label, "Future fixture")
        XCTAssertEqual(server.requestCount(sessionPath), 0, "Future programme details must not tune current content")
        let close = app.buttons["Close"]
        reachForSetup(close, in: app, limit: 8)
        assertFocus(close, in: app, context: "Future programme Close")
        remote.press(.select)
        assertFocus(future, in: app, context: "Programme Close restores exact future opener")
        let current = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "On now fixture")).firstMatch
        reachForSetup(current, in: app, limit: 8)
        assertFocus(current, in: app, context: "Current channel programme")
        remote.press(.select)
        XCTAssertTrue(app.buttons["Fullscreen"].waitForExistence(timeout: 15), app.debugDescription)
        XCTAssertEqual(server.requestCount(sessionPath), 1, "Current programme Select must tune")
        XCTAssertFalse(details.exists, "Current programme Select must not open the unavailable-programme sheet")
    }

    func testSharedLibraryRouteAndBackRestoreActualOpener() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let entry = app.buttons["shared-libraries-entry"]
        reachForSetup(entry, in: app, limit: 8)
        assertFocus(entry, in: app, context: "Shared libraries entry")
        remote.press(.select)
        let library = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Shared fixture movies")).firstMatch
        reachForSetup(library, in: app, limit: 8)
        assertFocus(library, in: app, context: "Shared library source row")
        remote.press(.select)
        let refresh = app.buttons["Refresh list"]
        XCTAssertTrue(refresh.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertEqual(server.requestCount("/api/v1/shared/imports/11111111-1111-4111-8111-111111111111/libraries/1/items"), 1)
        remote.press(.menu)
        assertFocus(library, in: app, context: "Shared list Back restores library row")
        remote.press(.menu)
        assertFocus(entry, in: app, context: "Shared root Back restores Libraries entry")
    }

    private func openChannels(origin: String) -> XCUIApplication {
        let app = openLibrary(origin: origin)
        remote.press(.menu)
        let channels = app.viewerTab("Channels")
        reachForSetup(channels, in: app, limit: 8)
        assertFocus(channels, in: app, context: "Channels tab setup")
        remote.press(.select)
        XCTAssertTrue(app.buttons["Watch live"].waitForExistence(timeout: 10), app.debugDescription)
        return app
    }

    private func tuneAndOpenFullscreen(in app: XCUIApplication, server: TVNavigationServer) {
        let watch = app.buttons["Watch live"]
        let favourite = app.buttons["channel-favourite:fixture-channel"]
        // Enter the trailing action from the toolbar above it; steering Right
        // from the final programme cannot cross the empty part of its row.
        let layout = app.buttons["channel-layout"]
        reachForSetup(layout, in: app, limit: 8)
        assertFocus(layout, in: app, context: "Channel toolbar before Favorite")
        remote.press(.down)
        assertFocus(favourite, in: app, context: "Channel Favorite entry")
        remote.press(.down)
        assertFocus(watch, in: app, context: "Down from Favorite reaches Watch across a short programme row")
        remote.press(.select)
        let fullscreen = app.buttons["Fullscreen"]
        XCTAssertTrue(fullscreen.waitForExistence(timeout: 15), app.debugDescription)
        waitUntil("The shipping AVPlayer requests fixture video", app: app, timeout: 10) {
            server.requestCount("/fixture.mp4") > 0
        }
        // Cross to the preview column before steering down to its controls;
        // Down from the channel-list's final row cannot cross that column.
        remote.press(.left)
        reachForSetup(fullscreen, in: app, limit: 8)
        assertFocus(fullscreen, in: app, context: "Fullscreen setup")
        remote.press(.select)
    }

    private func waitUntil(_ context: String, app: XCUIApplication, timeout: TimeInterval,
                           condition: @escaping () -> Bool) {
        let expectation = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in condition() }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [expectation], timeout: timeout), .completed,
                       "\(context): \(app.debugDescription)")
    }
}
#endif
