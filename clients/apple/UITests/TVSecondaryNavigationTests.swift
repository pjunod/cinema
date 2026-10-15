#if os(tvOS)
import XCTest

@MainActor
final class TVSecondaryNavigationTests: TVNavigationTestCase {
    func testMovieAboutReadsFinalRegionAndRestoresMovie() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let movie = app.buttons["item:1"]
        reachForSetup(movie, in: app, limit: 8)
        assertFocus(movie, in: app, context: "Movie before detail")
        remote.press(.select)
        let about = app.buttons["detail:about"]
        XCTAssertTrue(about.waitForExistence(timeout: 10), app.debugDescription)
        reachForSetup(about, in: app, limit: 8)
        assertFocus(about, in: app, context: "About opener")
        remote.press(.select)
        for index in 0..<4 {
            if index > 0 { remote.press(.down) }
            let paragraph = app.descendants(matching: .any).matching(identifier: "info:paragraph:\(index)").firstMatch
            assertFocus(paragraph, in: app, context: "Synopsis region \(index + 1)")
            if index == 3 { XCTAssertTrue(paragraph.label.contains("FINAL SYNOPSIS LINE")) }
        }
        remote.press(.menu)
        assertFocus(about, in: app, context: "About Back restores exact opener")
        remote.press(.menu)
        assertFocus(movie, in: app, context: "Detail Back restores original movie")
        XCTAssertEqual(server.requestCount("/api/v1/files/1/decision"), 0,
                       "Reading information must not prepare playback")
    }

    func testEpisodeDetailsDoNotStartPlayback() throws {
        let server = try TVNavigationServer(includeShows: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, collectionID: "show")
        defer { app.terminate() }
        let show = app.buttons["item:10001"]
        reachForSetup(show, in: app, limit: 8)
        assertFocus(show, in: app, context: "Show fixture")
        remote.press(.select)
        let season = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Fixture season")).firstMatch
        reachForSetup(season, in: app, limit: 12)
        assertFocus(season, in: app, context: "Season fixture")
        remote.press(.select)
        let episode = app.buttons["episode-details:10003"]
        XCTAssertTrue(episode.waitForExistence(timeout: 10), app.debugDescription)
        reachForSetup(episode, in: app, limit: 8)
        assertFocus(episode, in: app, context: "Episode details action")
        remote.press(.select)
        XCTAssertTrue(app.buttons["detail:about"].waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertGreaterThan(server.requestCount("/api/v1/items/10003"), 0)
        XCTAssertEqual(server.requestCount("/api/v1/files/1/decision"), 0)
        XCTAssertEqual(server.requestCount("/api/v1/library-channels/fixture-channel/sessions"), 0)
        XCTAssertFalse(app.buttons["player-play-pause"].exists)
        remote.press(.menu)
        assertFocus(episode, in: app, context: "Episode Back restores details action")
    }

    func testSettingsPickerHelpReportAndExactReturn() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        openRoot("Settings", in: app)
        let playback = app.buttons["settings-category-Playback"]
        reachForSetup(playback, in: app, limit: 8)
        assertFocus(playback, in: app, context: "Playback category")
        remote.press(.right)
        let quality = app.descendants(matching: .any).matching(identifier: "settings-quality").firstMatch
        assertFocus(quality, in: app, context: "Quality opener")
        remote.press(.select)
        let auto = app.cells.containing(NSPredicate(format: "label == %@", "Auto")).firstMatch
        assertFocus(auto, in: app, context: "Selected Auto quality")
        remote.press(.menu)
        assertFocus(quality, in: app, context: "Quality Back restores setting")
        remote.press(.menu)
        assertFocus(playback, in: app, context: "Playback Back restores category")
        let help = app.buttons["settings-category-Help"]
        reachForSetup(help, in: app, limit: 8)
        assertFocus(help, in: app, context: "Help category")
        remote.press(.right)
        let report = app.buttons["diagnostics-open"]
        reachForSetup(report, in: app, limit: 8)
        assertFocus(report, in: app, context: "Report opener")
        remote.press(.select)
        let done = app.buttons["diagnostics-done"]
        XCTAssertTrue(done.waitForExistence(timeout: 10), app.debugDescription)
        assertFocus(done, in: app, context: "Report safe Done entry")
        XCTAssertTrue(app.buttons["diagnostics-export-tv"].exists, "TV report must expose phone handoff")
        remote.press(.select)
        assertFocus(report, in: app, context: "Done restores exact Report opener")
        remote.press(.menu)
        assertFocus(help, in: app, context: "Help Back restores category")
    }

    func testSearchKeyboardBackRetrySameQueryAndResultReturn() throws {
        let server = try TVNavigationServer(failSearch: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        openRoot("Search", in: app)
        let query = app.textFields["Search movies, shows, and episodes"]
        reachForSetup(query, in: app, limit: 4)
        assertFocus(query, in: app, context: "Search query opener")
        remote.press(.select)
        query.typeText("movie")
        dismissKeyboard(in: app)
        assertFocus(query, in: app, context: "Keyboard Back restores search field")
        let retry = app.buttons["search:retry"]
        XCTAssertTrue(retry.waitForExistence(timeout: 10), app.debugDescription)
        let requests = server.requestCount("/api/v1/search")
        XCTAssertGreaterThan(requests, 0)
        reachForSetup(retry, in: app, limit: 4)
        assertFocus(retry, in: app, context: "Unchanged query Retry")
        server.allowSearchSuccess()
        remote.press(.select)
        let result = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "A Movie 0")).firstMatch
        XCTAssertTrue(result.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertGreaterThan(server.requestCount("/api/v1/search"), requests)
        XCTAssertEqual(query.value as? String, "movie", "Retry must not require a query edit")
        reachForSetup(query, in: app, limit: 4)
        assertFocus(query, in: app, context: "Query before Down into results")
        remote.press(.down)
        assertFocus(result, in: app, context: "Down reaches first search result")
        remote.press(.select)
        XCTAssertTrue(app.buttons["detail:about"].waitForExistence(timeout: 10), app.debugDescription)
        remote.press(.menu)
        assertFocus(result, in: app, context: "Detail Back restores same search result")
    }

    private func openRoot(_ label: String, in app: XCUIApplication) {
        remote.press(.menu)
        let tab = app.tabBars.buttons[label]
        reachForSetup(tab, in: app, limit: 8)
        assertFocus(tab, in: app, context: label + " tab setup")
        remote.press(.select)
    }
}
#endif
