#if os(iOS)
import Network
import XCTest

/// Drives the shipped screen through its ordinary login/library routes. The
/// fixture owns a loopback-only API; no server credentials or app test mode.
@MainActor
final class LibraryRowsNavigationTests: XCTestCase {
    func testLetterIndexScrollsToOffscreenRowAndRepeatsAfterManualScroll() async throws {
        let server = try LibraryRowsServer()
        let origin = try await server.start()
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

    func testYearIndexScrollsToItsRowInsteadOfItsOwnButton() async throws {
        let server = try LibraryRowsServer()
        let origin = try await server.start()
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
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.origin", origin, "-plurx.libraryPresentation", "rows",
                                "-plurx.libraryGrouping", "category"]
        app.launch()
        if app.textFields["Username"].waitForExistence(timeout: 5) {
            app.textFields["Username"].tap()
            app.textFields["Username"].typeText("fixture")
            app.secureTextFields["Password"].tap()
            app.secureTextFields["Password"].typeText("fixture")
            app.buttons["Sign in"].tap()
        }
        let libraries = app.tabBars.buttons["Libraries"]
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

    private func reveal(_ label: String, in index: XCUIElement, forward: Bool) {
        let button = index.buttons["Jump to \(label)"]
        for _ in 0..<20 {
            if button.exists && button.isHittable { return }
            if forward { index.swipeLeft() } else { index.swipeRight() }
        }
        XCTFail("Could not reach index entry \(label)")
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

    func start() async throws -> String {
        listener.newConnectionHandler = { [weak self] connection in
            guard let self else { connection.cancel(); return }
            connection.start(queue: self.queue)
            self.receive(connection, buffered: Data())
        }
        return try await withCheckedThrowingContinuation { continuation in
            listener.stateUpdateHandler = { [weak self] state in
                guard let self else { return }
                switch state {
                case .ready:
                    self.listener.stateUpdateHandler = nil
                    continuation.resume(returning: "http://127.0.0.1:\(self.listener.port!.rawValue)")
                case .failed(let error):
                    self.listener.stateUpdateHandler = nil
                    continuation.resume(throwing: error)
                default: break
                }
            }
            listener.start(queue: queue)
        }
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
