import XCTest

/// Runs only from the dedicated physical tvOS scheme. The target Apple TV must
/// already be paired, signed in to a reachable server, and have the named media.
@MainActor
final class TVPhysicalInputTests: XCTestCase {
    private let remote = XCUIRemote.shared

    func testSettingsChoicesOpenAsMenusOnAppleTV() throws {
        let app = XCUIApplication()
        wakeDevice()
        app.launch()
        let settings = app.tabBars.buttons["Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: 30),
                      "Apple TV must already be signed in")
        XCTAssertTrue(focus(settings, in: app), "Could not focus Settings tab")
        remote.press(.select)

        let quality = app.descendants(matching: .any)
            .matching(identifier: "settings-quality").firstMatch
        XCTAssertTrue(quality.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(focus(quality, in: app), "Could not focus Quality picker")
        remote.press(.select)

        let auto = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "Auto")).firstMatch
        let original = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "Original")).firstMatch
        XCTAssertTrue(auto.waitForExistence(timeout: 5) && original.waitForExistence(timeout: 5),
                      "Quality opened without both Auto and Original choices")
        XCTAssertTrue(auto.isHittable && original.isHittable,
                      "Quality choices are not usable in the open menu")
        attachScreen(app, name: "settings-quality-menu-open")
    }

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

    func testRapidBackwardSeeksResumeAdvancingPlayback() throws {
        let app = try playbackApp(startMs: 5_100_000)
        let position = app.staticTexts["player-position"]
        let back = app.buttons["player-skip-back"]
        remote.press(.select)
        XCTAssertTrue(position.waitForExistence(timeout: 45), app.debugDescription)
        XCTAssertTrue(back.waitForExistence(timeout: 5))
        XCTAssertTrue(waitForPosition(position, timeout: 40) { $0 >= 5_103 },
                      "Initial playback clock never advanced from the requested position")
        if !back.exists { remote.press(.up) }
        XCTAssertTrue(focus(back, in: app), "Could not focus backward seek")
        let before = try timeSeconds(position.label)
        XCTAssertGreaterThanOrEqual(before, 5_100)
        remote.press(.select)
        remote.press(.select)
        XCTAssertTrue(waitForPosition(position, timeout: 15) { $0 <= before - 15 },
                      "Two backward seeks did not move the timeline")
        let landed = try timeSeconds(position.label)
        attachScreen(app, name: "two-backward-seeks-landed")
        XCTAssertTrue(waitForPosition(position, timeout: 25) { $0 >= landed + 3 },
                      "Playback clock did not resume after backward seeks")
        XCTAssertEqual(app.buttons["player-play-pause"].label, "Pause")
        attachScreen(app, name: "two-backward-seeks-playing")
    }

    private func waitForPosition(
        _ element: XCUIElement, timeout: TimeInterval, accepts: (Int) -> Bool
    ) -> Bool {
        let end = Date().addingTimeInterval(timeout)
        while Date() < end {
            if element.exists, let seconds = try? timeSeconds(element.label), accepts(seconds) {
                return true
            }
            // Up reveals idle chrome without committing another seek. The
            // frame clock, not the optimistic seek target, must then advance.
            if !element.exists { remote.press(.up) }
            Thread.sleep(forTimeInterval: 0.5)
        }
        return false
    }

    private func timeSeconds(_ value: String) throws -> Int {
        let parts = value.split(separator: ":").compactMap { Int($0) }
        XCTAssertTrue(parts.count == 2 || parts.count == 3, "Unexpected timeline: \(value)")
        return parts.reduce(0) { $0 * 60 + $1 }
    }

    func testCaptionSelectionWithRemote() throws {
        let trackLabel = try requiredEnvironment("PLURX_TVOS_CAPTION_LABEL")
        let app = try playbackApp()
        remote.press(.select)
        // SwiftUI's tvOS Menu may be a PopUpButton in the modern accessibility
        // tree even when the legacy Button query reports its identifier.
        let subtitles = app.descendants(matching: .any)
            .matching(identifier: "player-subtitles").firstMatch
        XCTAssertTrue(subtitles.waitForExistence(timeout: 20),
                      "Fixture needs a file with an exposed subtitle track")

        let reachedSubtitles = focus(subtitles, in: app)
        attachScreen(app, name: "caption-control-focus")
        XCTAssertTrue(reachedSubtitles, "Could not focus Subtitles with remote")
        remote.press(.select)
        // Every Subtitles menu has Off; Quality has Auto instead. Check the
        // menu we actually opened before attributing a missing track to media.
        let offChoice = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", "Off")).firstMatch
        attachScreen(app, name: "caption-menu-open")
        XCTAssertTrue(offChoice.waitForExistence(timeout: 5),
                      "Remote did not open the Subtitles menu")
        let choice = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", trackLabel)).firstMatch
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

        // Grouping is a saved user preference. Establish the named fixture
        // through ordinary UI, and restore it even if navigation/paging fails.
        let category = app.buttons["Category"]
        let library = app.buttons["Library"]
        XCTAssertTrue(category.waitForExistence(timeout: 10) && library.exists,
                      "Library grouping choices are unavailable")
        let originalGrouping = try XCTUnwrap(
            category.isSelected != library.isSelected
                ? (category.isSelected ? "Category" : "Library") : nil,
            "Could not observe the current library grouping")
        defer {
            // Relaunch returns from a collection detail to the ordinary root;
            // it retains the account and lets cleanup use the same controls.
            app.launch()
            let restoreLibraries = app.tabBars.buttons["Libraries"]
            if restoreLibraries.waitForExistence(timeout: 30)
                && focus(restoreLibraries, in: app) {
                remote.press(.select)
                XCTAssertTrue(setLibraryGrouping(originalGrouping, in: app),
                              "Could not restore the original library grouping")
            } else {
                XCTFail("Could not return to Libraries to restore grouping")
            }
        }
        let requiredGrouping = try XCTUnwrap(
            collectionID.hasPrefix("category:") ? "Category"
                : collectionID.hasPrefix("share:") ? "Library" : nil,
            "Collection must name a category or share")
        XCTAssertTrue(setLibraryGrouping(requiredGrouping, in: app),
                      "Could not establish the requested collection grouping")

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

    private func playbackApp(startMs: Int = 0) throws -> XCUIApplication {
        let fileID = try requiredEnvironment("PLURX_TVOS_FILE_ID")
        let itemID = try requiredEnvironment("PLURX_TVOS_ITEM_ID")
        let app = XCUIApplication()
        app.launchArguments += [
            "-plurx.acceptance.itemId", itemID,
            "-plurx.acceptance.fileId", fileID,
            "-plurx.acceptance.title", "Physical remote acceptance",
        ]
        if let origin = ProcessInfo.processInfo.environment["PLURX_TVOS_ORIGIN"] {
            app.launchArguments += ["-plurx.origin", origin]
        }
        app.launchArguments += [
            "-plurx.acceptance.startMs", String(startMs),
            "-plurx.acceptance.probe", "YES",
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

    /// Cross the tab/content boundary vertically before steering within a row.
    /// Horizontal presses on a tab or segmented picker can replace the target's
    /// entire view, so geometry must not steer across those rows first.
    private func focus(_ target: XCUIElement, in app: XCUIApplication) -> Bool {
        guard target.exists else { return false }
        let targetIsTab = app.tabBars.buttons.matching(
            NSPredicate(format: "identifier == %@ AND label == %@",
                        target.identifier, target.label)
        ).firstMatch.exists
        let focused = NSPredicate(format: "hasFocus == true")
        for _ in 0..<80 {
            // A vanished target is a failed navigation path, not a snapshot
            // exception or a reason to select a different control.
            guard target.exists else {
                attachScreen(app, name: "focus-target-disappeared")
                return false
            }
            if target.hasFocus { return true }
            let focusedTab = app.tabBars.buttons.matching(focused).firstMatch
            if targetIsTab && !focusedTab.exists {
                remote.press(.up)
                continue
            }
            if !targetIsTab && focusedTab.exists {
                remote.press(.down)
                continue
            }
            let current = app.buttons.matching(focused).firstMatch
            guard current.exists else { remote.press(.up); continue }
            let targetFrame = target.frame
            let currentFrame = current.frame
            // Leave the Category/Library segmented row downwards before
            // seeking a shelf's See All button horizontally. Otherwise Right
            // changes grouping and removes library-open-category:*.
            if targetFrame.minY >= currentFrame.maxY {
                remote.press(.down)
            } else if targetFrame.maxY <= currentFrame.minY {
                remote.press(.up)
            } else {
                let dx = targetFrame.midX - currentFrame.midX
                let dy = targetFrame.midY - currentFrame.midY
                if abs(dx) > abs(dy) {
                    remote.press(dx > 0 ? .right : .left)
                } else {
                    remote.press(dy > 0 ? .down : .up)
                }
            }
        }
        guard target.exists else { return false }
        let reached = target.hasFocus
        if !reached { attachScreen(app, name: "focus-path-exhausted") }
        return reached
    }

    private func setLibraryGrouping(_ label: String, in app: XCUIApplication) -> Bool {
        let choice = app.buttons[label]
        guard choice.waitForExistence(timeout: 10) else { return false }
        if !choice.isSelected {
            guard focus(choice, in: app) else { return false }
            // A tvOS segmented picker can select while focus moves onto it.
            if !choice.isSelected { remote.press(.select) }
        }
        let end = Date().addingTimeInterval(5)
        while Date() < end {
            if choice.exists && choice.isSelected { return true }
            Thread.sleep(forTimeInterval: 0.1)
        }
        attachScreen(app, name: "library-grouping-not-selected")
        return false
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
