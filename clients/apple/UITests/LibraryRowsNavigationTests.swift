#if os(iOS)
import Network
import XCTest

/// Drives the shipped screen through its ordinary login/library routes. The
/// fixture owns a loopback-only API; no server credentials or app test mode.
@MainActor
final class LibraryRowsNavigationTests: XCTestCase {
    func testPortraitLiveGuideShowsMultipleChannelsDespiteLegacySchedulePreference() throws {
        let server = try LibraryRowsServer()
        let origin = try server.start()
        defer { server.stop() }
        XCUIDevice.shared.orientation = .portrait
        let app = appAtLogin(origin: origin, launchArguments: [
            "-plurx.liveTvMobileGuide", "schedule", "-plurx.liveTvView", "list"
        ])
        defer { app.terminate() }
        signInToFixture(app)
        let live = app.viewerTab("Live TV")
        XCTAssertTrue(live.waitForExistence(timeout: 15), app.debugDescription)
        live.tap()
        let view = app.buttons["Live TV view, On now"]
        XCTAssertTrue(view.waitForExistence(timeout: 10), app.debugDescription)
        view.tap()
        app.buttons["Guide"].tap()

        let grid = app.scrollViews["live-guide-grid"]
        XCTAssertTrue(grid.waitForExistence(timeout: 10), app.debugDescription)
        let news = grid.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "live-guide-programme:news:")).firstMatch
        let film = grid.buttons.matching(NSPredicate(format: "identifier BEGINSWITH %@", "live-guide-programme:film:")).firstMatch
        XCTAssertTrue(news.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(film.waitForExistence(timeout: 10), app.debugDescription)
        XCTAssertTrue(news.isHittable && film.isHittable,
                      "Portrait Guide must show programmes from multiple stations together; "
                      + "news=\(news.frame), film=\(film.frame), grid=\(grid.frame)\n\(app.debugDescription)")
        XCTAssertGreaterThan(film.frame.midY, news.frame.midY,
                             "Stations must occupy separate rows of the time grid")
        XCTAssertFalse(app.buttons["Grid"].exists, "Guide must open directly as a grid")
        XCTAssertFalse(app.buttons["Selected channel schedule"].exists)
        XCTAssertFalse(app.buttons["Channel"].exists, "The single-channel picker must be absent")
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "portrait-live-guide-grid-legacy-preference"
        attachment.lifetime = .keepAlways
        add(attachment)

        news.tap()
        XCTAssertTrue(app.buttons["Watch"].waitForExistence(timeout: 5),
                      "A grid programme must still open its programme actions")
    }

    func testSignInCanReturnToServerPickerAndConnectAgain() throws {
        let server = try LibraryRowsServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = appAtLogin(origin: origin)
        defer { app.terminate() }

        let changeServer = app.buttons["Use a different server"]
        XCTAssertTrue(changeServer.isHittable, app.debugDescription)
        changeServer.tap()
        XCTAssertTrue(app.buttons["Add server manually"].waitForExistence(timeout: 5), app.debugDescription)
        connectManually(origin: origin, in: app)
        signInToFixture(app)
        XCTAssertTrue(app.viewerTab("Libraries").waitForExistence(timeout: 15), app.debugDescription)
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "sign-in-server-picker-recovery"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    func testLetterIndexScrollsToOffscreenRowAndRepeatsAfterManualScroll() throws {
        let server = try LibraryRowsServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }

        let index = app.scrollViews["library-group-index"]
        let results = app.scrollViews["library-results"]
        reveal("M", in: index, forward: true)
        index.buttons["Jump to M"].tap()
        assertHeading("M", atTopOf: results, in: app)

        // Re-selecting the same letter must still execute a scroll command.
        results.swipeUp()
        index.buttons["Jump to M"].tap()
        assertHeading("M", atTopOf: results, in: app)

        reveal("A", in: index, forward: false)
        index.buttons["Jump to A"].tap()
        assertHeading("A", atTopOf: results, in: app)
    }

    func testYearIndexScrollsToItsRowInsteadOfItsOwnButton() throws {
        let server = try LibraryRowsServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }

        app.buttons["Sort"].tap()
        app.buttons["Year"].tap()
        let index = app.scrollViews["library-group-index"]
        XCTAssertTrue(index.buttons["Jump to 2025"].waitForExistence(timeout: 10))
        reveal("2012", in: index, forward: true)
        index.buttons["Jump to 2012"].tap()
        assertHeading("year-2012", atTopOf: app.scrollViews["library-results"], in: app)
    }

    private func openLibrary(origin: String) -> XCUIApplication {
        let app = appAtLogin(origin: origin)
        signInToFixture(app)
        let libraries = app.viewerTab("Libraries")
        XCTAssertTrue(libraries.waitForExistence(timeout: 15), app.debugDescription)
        libraries.tap()
        let open = app.buttons["library-open-category:movie"]
        XCTAssertTrue(open.waitForExistence(timeout: 10), app.debugDescription)
        open.tap()
        let summary = app.staticTexts["library-loaded-summary"]
        let loaded = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label == %@", "416 items"), object: summary)
        XCTAssertEqual(XCTWaiter.wait(for: [loaded], timeout: 15), .completed)
        return app
    }

    private func appAtLogin(origin: String, launchArguments: [String] = []) -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.libraryPresentation", "rows",
                                "-plurx.libraryGrouping", "category"]
        app.launchArguments += launchArguments
        app.launch()
        let manual = app.buttons["Add server manually"]
        let changeServer = app.buttons["Use a different server"]
        let libraries = app.viewerTab("Libraries")
        // A previous fixture's closed port may spend the production 30 s
        // connectivity budget before recovery controls replace the spinner.
        let initialScreen = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            manual.exists || changeServer.exists || libraries.exists
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [initialScreen], timeout: 45), .completed, app.debugDescription)
        if !manual.exists {
            if changeServer.exists {
                changeServer.tap()
            } else {
                // A fixture from a preceding test may still be reachable at
                // this port. Leave it through the ordinary Account control.
                let settings = app.viewerTab("Settings")
                XCTAssertTrue(settings.waitForExistence(timeout: 10), app.debugDescription)
                settings.tap()
                let change = app.buttons["Change server"]
                for _ in 0..<12 {
                    if change.exists && change.isHittable { break }
                    app.swipeUp()
                }
                XCTAssertTrue(change.isHittable, app.debugDescription)
                change.tap()
            }
        }
        XCTAssertTrue(manual.waitForExistence(timeout: 5), app.debugDescription)
        connectManually(origin: origin, in: app)
        return app
    }

    private func connectManually(origin: String, in app: XCUIApplication) {
        app.buttons["Add server manually"].tap()
        let address = app.textFields.firstMatch
        XCTAssertTrue(address.waitForExistence(timeout: 5), app.debugDescription)
        address.tap()
        let previous = address.value as? String ?? ""
        address.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: previous.count) + origin)
        let connect = app.buttons["Connect"]
        XCTAssertTrue(connect.isHittable, app.debugDescription)
        connect.tap()
        XCTAssertTrue(app.textFields["Username"].waitForExistence(timeout: 10), app.debugDescription)
    }

    private func signInToFixture(_ app: XCUIApplication) {
        let username = app.textFields["Username"]
        username.tap()
        let previous = username.value as? String ?? ""
        username.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: previous.count) + "fixture")
        app.secureTextFields["Password"].tap()
        app.secureTextFields["Password"].typeText("fixture")
        app.buttons["Sign in"].tap()
    }

    private func reveal(_ label: String, in index: XCUIElement, forward: Bool) {
        let button = index.buttons["Jump to \(label)"]
        for _ in 0..<20 {
            if button.exists && button.isHittable { return }
            // Slow, bounded drags avoid flinging past a narrow target letter.
            let movingForward = button.exists ? button.frame.midX > index.frame.midX : forward
            let start = index.coordinate(withNormalizedOffset: CGVector(dx: movingForward ? 0.8 : 0.2, dy: 0.5))
            let end = index.coordinate(withNormalizedOffset: CGVector(dx: movingForward ? 0.3 : 0.7, dy: 0.5))
            start.press(forDuration: 0.1, thenDragTo: end, withVelocity: .slow, thenHoldForDuration: 0.2)
        }
        XCTFail("Could not reach index entry \(label): \(index.debugDescription)")
    }

    private func assertHeading(_ id: String, atTopOf results: XCUIElement, in app: XCUIApplication) {
        let header = app.staticTexts["library-group-heading-\(id)"]
        let aligned = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            header.exists && header.isHittable && abs(header.frame.minY - results.frame.minY) < 8
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [aligned], timeout: 5), .completed,
                       "Tapping the index must align row \(id) with the vertical viewport")
        let attachment = XCTAttachment(screenshot: app.screenshot())
        attachment.name = "library-jump-\(id)"
        attachment.lifetime = .keepAlways
        add(attachment)
    }
}

private final class LibraryRowsServer: @unchecked Sendable {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "library-rows-fixture")

    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: .ipv4(.loopback), port: .any)
        listener = try NWListener(using: parameters)
    }

    func start() throws -> String {
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            connection.start(queue: self.queue)
            self.receive(connection, buffered: Data())
        }
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { [weak self] state in
            switch state {
            case .ready, .failed:
                self?.listener.stateUpdateHandler = nil
                ready.signal()
            default: break
            }
        }
        listener.start(queue: queue)
        guard ready.wait(timeout: .now() + 5) == .success, let port = listener.port else {
            listener.cancel()
            throw URLError(.cannotConnectToHost)
        }
        return "http://127.0.0.1:\(port.rawValue)"
    }

    func stop() { listener.cancel() }

    private func receive(_ connection: NWConnection, buffered: Data) {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] chunk, _, complete, error in
            guard let self else { connection.cancel(); return }
            var data = buffered
            data.append(chunk ?? Data())
            guard let request = String(data: data, encoding: .utf8), request.contains("\r\n\r\n") else {
                if complete || error != nil { connection.cancel() }
                else { self.receive(connection, buffered: data) }
                return
            }
            let path = request.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
            let body = self.response(path)
            var response = Data("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \(body.count)\r\nConnection: close\r\n\r\n".utf8)
            response.append(body)
            connection.send(content: response, completion: .contentProcessed { _ in connection.cancel() })
        }
    }

    private func response(_ target: String) -> Data {
        let url = URLComponents(string: "http://fixture" + target)!
        let path = url.path
        let user: [String: Any] = ["id": 1, "username": "fixture"]
        let value: Any
        switch path {
        case "/api/v1/auth/login": value = ["token": "fixture-only", "user": user]
        case "/api/v1/me": value = user
        case "/api/v1/server": value = ["name": "Library fixture", "instance_id": "library-fixture"]
        case "/api/v1/libraries": value = [["id": 1, "name": "Movies", "kind": "movie"]]
        case "/api/v1/live-tv/channels":
            value = ["channels": [
                ["id": "news", "guide_number": "12.1", "guide_name": "Evening News", "favorite": false, "drm": false, "support": "ready"],
                ["id": "film", "guide_number": "27", "guide_name": "Film", "favorite": false, "drm": false, "support": "ready"]
            ], "freshness": "fresh", "age_seconds": 0]
        case "/api/v1/live-tv/guide":
            let now = Int(Date().timeIntervalSince1970)
            value = ["source": "fixture", "freshness": "fresh", "age_seconds": 0,
                     "window": ["start": now - 3600, "end": now + 7200],
                     "channels": [
                         ["id": "news", "guide_number": "12.1", "programmes": [
                             ["start": now - 300, "end": now + 3300, "title": "Fixture evening news"]
                         ]],
                         ["id": "film", "guide_number": "27", "programmes": [
                             ["start": now - 300, "end": now + 3300, "title": "Fixture film"]
                         ]]
                     ]]
        case "/api/v1/libraries/1/items":
            let query = Dictionary(uniqueKeysWithValues: (url.queryItems ?? []).map { ($0.name, $0.value ?? "") })
            let offset = Int(query["offset"] ?? "0") ?? 0
            let limit = Int(query["limit"] ?? "200") ?? 200
            var items: [[String: Any]] = (0..<416).map { id in
                let letter = String(UnicodeScalar(65 + id / 16)!)
                let title = "\(letter) Movie \(id)"
                return ["id": id + 1, "library_id": 1, "kind": "movie", "title": title,
                        "sort_title": title.lowercased(), "year": 2000 + id / 16]
            }
            if query["sort"] == "year" { items.reverse() }
            value = ["items": Array(items.dropFirst(offset).prefix(limit)), "total": items.count]
        default: value = [:]
        }
        return try! JSONSerialization.data(withJSONObject: value)
    }
}
#endif
