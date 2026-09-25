import XCTest

/// Runs only from the dedicated physical tvOS scheme. The target Apple TV must
/// already be paired, signed in to a reachable server, and have the named media.
@MainActor
final class TVPhysicalInputTests: XCTestCase {
    private let remote = XCUIRemote.shared

    func testPlaybackRemoteControls() throws {
        let app = try playbackApp()
        let playPause = app.buttons["player-play-pause"]
        remote.press(.select) // Reveal chrome if playback has hidden it.
        XCTAssertTrue(playPause.waitForExistence(timeout: 20), app.debugDescription)

        let before = playPause.label
        XCTAssertTrue(before == "Play" || before == "Pause", "Unexpected transport: \(before)")
        remote.press(.playPause)
        let expected = before == "Play" ? "Pause" : "Play"
        let changed = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label == %@", expected), object: playPause
        )
        XCTAssertEqual(XCTWaiter.wait(for: [changed], timeout: 10), .completed)

        remote.press(.select)
        XCTAssertTrue(app.buttons["player-skip-forward"].waitForExistence(timeout: 5))
        attachScreen(app, name: "playback-remote")
    }

    func testCaptionSelectionWithRemote() throws {
        let trackLabel = try requiredEnvironment("PLURX_TVOS_CAPTION_LABEL")
        let app = try playbackApp()
        remote.press(.select)
        let subtitles = app.buttons["player-subtitles"]
        XCTAssertTrue(subtitles.waitForExistence(timeout: 20),
                      "Fixture needs a file with an exposed subtitle track")

        XCTAssertTrue(focus(subtitles, in: app),
                      "Could not focus Subtitles with remote")
        remote.press(.select)
        let choice = app.buttons[trackLabel]
        XCTAssertTrue(choice.waitForExistence(timeout: 10),
                      "Track \(trackLabel) was not exposed by the media item")
        for _ in 0..<12 where !choice.hasFocus { remote.press(.down) }
        for _ in 0..<12 where !choice.hasFocus { remote.press(.up) }
        XCTAssertTrue(choice.hasFocus, "Could not focus caption track with remote")
        remote.press(.select)
        let selected = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "value == %@", trackLabel), object: subtitles
        )
        XCTAssertEqual(XCTWaiter.wait(for: [selected], timeout: 10), .completed)
        attachScreen(app, name: "caption-selection")
    }

    func testLibraryPagingWithRemote() throws {
        let collectionID = try requiredEnvironment("PLURX_TVOS_LIBRARY_ID")
        let app = XCUIApplication()
        wakeDevice()
        app.launch()
        let libraries = app.tabBars.buttons["Libraries"]
        XCTAssertTrue(libraries.waitForExistence(timeout: 30),
                      "Apple TV must already be signed in")
        XCTAssertTrue(focus(libraries, in: app),
                      "Could not focus Libraries tab with remote")
        remote.press(.select)

        let openCollection = app.buttons["library-open-\(collectionID)"]
        for _ in 0..<60 where !openCollection.exists { remote.press(.down) }
        XCTAssertTrue(openCollection.exists,
                      "Collection \(collectionID) is not in the Libraries tab")
        XCTAssertTrue(focus(openCollection, in: app),
                      "Could not focus collection with remote")
        remote.press(.select)
        let summary = app.staticTexts["library-loaded-summary"]
        XCTAssertTrue(summary.waitForExistence(timeout: 30), app.debugDescription)
        let pageReady = XCTNSPredicateExpectation(
            predicate: NSPredicate(format: "label MATCHES %@",
                                   "^[0-9]+ of [1-9][0-9]* loaded.*"),
            object: summary
        )
        XCTAssertEqual(XCTWaiter.wait(for: [pageReady], timeout: 30), .completed,
                       "Library did not load its first page")
        let initial = try loadedCount(summary.label)
        let total = try totalCount(summary.label)
        XCTAssertGreaterThan(total, initial,
                             "Fixture needs a collection with more than the first page")

        // The grid loads the next page when later cards appear. This remains
        // a real focus/scroll path on Apple TV, with a bounded number of presses.
        var reachedNextPage = false
        for _ in 0..<80 {
            remote.press(.down)
            if ((try? loadedCount(summary.label)) ?? initial) > initial {
                reachedNextPage = true
                break
            }
        }
        XCTAssertTrue(reachedNextPage,
                      "Remote navigation did not load past \(initial) of \(total) items")
        attachScreen(app, name: "library-paging")
    }

    private func playbackApp() throws -> XCUIApplication {
        let fileID = try requiredEnvironment("PLURX_TVOS_FILE_ID")
        let itemID = try requiredEnvironment("PLURX_TVOS_ITEM_ID")
        let app = XCUIApplication()
        app.launchArguments += [
            "-plurx.acceptance.itemId", itemID,
            "-plurx.acceptance.fileId", fileID,
            "-plurx.acceptance.title", "Physical remote acceptance",
        ]
        wakeDevice()
        app.launch()
        return app
    }

    private func wakeDevice() {
        // CoreDevice can start the test runner while tvOS still forbids a
        // foreground app launch. The physical remote's Menu key wakes it.
        remote.press(.menu)
        Thread.sleep(forTimeInterval: 1)
    }

    private func requiredEnvironment(_ name: String) throws -> String {
        let value = try XCTUnwrap(ProcessInfo.processInfo.environment[name],
                                  "Set \(name) in the physical test scheme")
        return try XCTUnwrap(value.isEmpty ? nil : value,
                             "\(name) must not be empty")
    }

    /// Steer toward an identified visible control using the focused control's
    /// accessibility frame. The bounded path fails visibly if focus is trapped.
    private func focus(_ target: XCUIElement, in app: XCUIApplication) -> Bool {
        for _ in 0..<80 {
            if target.hasFocus { return true }
            let current = app.buttons.matching(
                NSPredicate(format: "hasFocus == true")
            ).firstMatch
            guard current.exists else { remote.press(.up); continue }
            let dx = target.frame.midX - current.frame.midX
            let dy = target.frame.midY - current.frame.midY
            if abs(dx) > abs(dy) {
                remote.press(dx > 0 ? .right : .left)
            } else {
                remote.press(dy > 0 ? .down : .up)
            }
        }
        return target.hasFocus
    }

    private func loadedCount(_ label: String) throws -> Int {
        try XCTUnwrap(Int(label.split(separator: " ").first ?? ""),
                      "Could not parse library count from \(label)")
    }

    private func totalCount(_ label: String) throws -> Int {
        let parts = label.split(separator: " ")
        if parts.count >= 2 && (parts[1] == "item" || parts[1] == "items") {
            return try loadedCount(label)
        }
        return try XCTUnwrap(parts.count >= 3 ? Int(parts[2]) : nil,
                             "Could not parse library total from \(label)")
    }

    private func attachScreen(_ app: XCUIApplication, name: String) {
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = name
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}
