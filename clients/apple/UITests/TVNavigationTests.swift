#if os(tvOS)
import XCTest

/// Simulator journeys through the shipping authentication, tab shell and routes.
/// Setup may enter text; the measured navigation uses only the native remote.
@MainActor
final class TVNavigationTests: XCTestCase {
    private let remote = XCUIRemote.shared

    func testViewModalOwnsFocusAndBackRestoresOpener416() throws {
        try viewModalJourney(itemCount: 416)
    }

    func testViewModalOwnsFocusAndBackRestoresOpener5000() throws {
        try viewModalJourney(itemCount: 5000)
    }

    func testGridToolsAndJump416() throws { try toolsJourney(itemCount: 416, presentation: "grid") }
    func testGridToolsAndJump5000() throws { try toolsJourney(itemCount: 5000, presentation: "grid") }
    func testRowsToolsAndJump416() throws { try toolsJourney(itemCount: 416, presentation: "rows") }
    func testRowsToolsAndJump5000() throws { try toolsJourney(itemCount: 5000, presentation: "rows") }

    private func toolsJourney(itemCount: Int, presentation: String) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin, presentation: presentation)
        defer { app.terminate() }
        for (letter, groupIndex) in [("A", 0), ("M", 12), ("Z", 25), ("M", 12)] {
            let jump = app.buttons["library:jump"]
            reachForSetup(jump, in: app, limit: 8)
            assertFocus(jump, in: app, context: "Jump opener")
            remote.press(.select)
            let firstChoice = app.buttons["choice:A"]
            XCTAssertTrue(firstChoice.waitForExistence(timeout: 30), app.debugDescription)
            // Jump has no selection: entry is the first visible group. This
            // explicit count is setup, separate from the six-press tool bound.
            assertFocus(firstChoice, in: app, context: "Jump first choice")
            for _ in 0..<groupIndex { remote.press(.down) }
            let choice = app.buttons["choice:" + letter]
            assertFocus(choice, in: app, context: "Jump \(letter) choice")
            remote.press(.select)
            let index = (groupIndex * itemCount + 25) / 26
            let item = app.buttons["item:\(index + 1)"]
            assertFocus(item, in: app, context: "Jump \(letter) focuses first item")
            let tools = app.buttons["library:view"]
            for _ in 0..<6 where !tools.hasFocus { remote.press(.left) }
            assertFocus(tools, in: app, context: "Tools within six Left presses")
            remote.press(.right)
            assertFocus(item, in: app, context: "Right restores \(letter) bookmark")
        }
    }

    func testSortFilterAndGroupingOwnFocusAndRestoreOpeners() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        for (openerLabel, selectedLabel) in [("Sort", "Title (A–Z)"), ("Filter", "Everything")] {
            let opener = app.buttons[openerLabel]
            reachForSetup(opener, in: app, limit: 8)
            assertFocus(opener, in: app, context: openerLabel + " opener")
            remote.press(.select)
            assertFocus(app.buttons[selectedLabel], in: app, context: openerLabel + " selected choice")
            remote.press(.menu)
            assertFocus(opener, in: app, context: openerLabel + " Back restores opener")
        }
        remote.press(.menu)
        let grouping = app.buttons["Group by"]
        XCTAssertTrue(grouping.waitForExistence(timeout: 5), app.debugDescription)
        reachForSetup(grouping, in: app, limit: 8)
        assertFocus(grouping, in: app, context: "Group by opener")
        remote.press(.select)
        assertFocus(app.buttons["Category"], in: app, context: "Group by selected category")
        remote.press(.menu)
        assertFocus(grouping, in: app, context: "Group by Back restores opener")
    }

    func testEveryEnabledRootTabReceivesNativeFocus() throws {
        let server = try TVNavigationServer()
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let libraries = app.tabBars.buttons["Libraries"]
        reachForSetup(libraries, in: app, limit: 4)
        assertFocus(libraries, in: app, context: "Libraries root tab")
        remote.press(.left)
        assertFocus(app.tabBars.buttons["Home"], in: app, context: "Home root tab")
        for label in ["Libraries", "Live TV", "Recordings", "Channels", "Search", "Settings"] {
            remote.press(.right)
            let tab = app.tabBars.buttons[label]
            assertFocus(tab, in: app, context: label + " root tab")
            remote.press(.select)
            XCTAssertTrue(tab.isSelected, label + " must remain the selected route")
        }
        for label in ["Search", "Channels", "Recordings", "Live TV", "Libraries", "Home"] {
            remote.press(.left)
            assertFocus(app.tabBars.buttons[label], in: app, context: label + " reverse root tab")
        }
    }

    private func viewModalJourney(itemCount: Int) throws {
        let server = try TVNavigationServer(itemCount: itemCount)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        let view = app.buttons["View"]
        reachForSetup(view, in: app, limit: 8)
        assertFocus(view, in: app, context: "View opener")
        remote.press(.select)
        let grid = app.buttons["Grid"]
        XCTAssertTrue(grid.waitForExistence(timeout: 5), app.debugDescription)
        assertFocus(grid, in: app, context: "selected Grid on modal entry")
        // Every direction stays within the panel. Mere panel existence is not
        // acceptance: the historical failure moves onto a movie underneath it.
        for direction: XCUIRemote.Button in [.down, .up, .left, .right] {
            remote.press(direction)
            let choices = [app.buttons["Rows"], grid, app.buttons["Close"]]
            XCTAssertTrue(choices.contains { $0.exists && $0.hasFocus },
                          "Modal lost native focus: \(app.debugDescription)")
        }
        remote.press(.menu)
        assertFocus(view, in: app, context: "Back restores View")
        XCTAssertFalse(grid.exists, "Back must dismiss only the choice panel")
        remote.press(.select)
        assertFocus(grid, in: app, context: "reopened selection")
        remote.press(.menu)
        assertFocus(view, in: app, context: "second Back restores View")
        remote.press(.menu)
        XCTAssertTrue(app.tabBars.buttons["Libraries"].waitForExistence(timeout: 5),
                      "Back must return to Libraries, not leave the app")
    }

    private func openLibrary(origin: String, presentation: String = "grid") -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.origin", origin, "-plurx.libraryPresentation", presentation,
                                "-plurx.libraryGrouping", "category"]
        app.launch()
        if app.textFields["Username"].waitForExistence(timeout: 8) {
            reachForSetup(app.textFields["Username"], in: app, limit: 4)
            assertFocus(app.textFields["Username"], in: app, context: "Username setup")
            remote.press(.select)
            app.textFields["Username"].typeText("fixture")
            dismissKeyboard(in: app)
            reachForSetup(app.secureTextFields["Password"], in: app, limit: 4)
            assertFocus(app.secureTextFields["Password"], in: app, context: "Password setup")
            remote.press(.select)
            app.secureTextFields["Password"].typeText("fixture")
            dismissKeyboard(in: app)
            let signIn = app.buttons["Sign in"]
            reachForSetup(signIn, in: app, limit: 4)
            assertFocus(signIn, in: app, context: "Sign in setup")
            remote.press(.select)
        }
        let libraries = app.tabBars.buttons["Libraries"]
        XCTAssertTrue(libraries.waitForExistence(timeout: 15), app.debugDescription)
        reachForSetup(libraries, in: app, limit: 8)
        assertFocus(libraries, in: app, context: "Libraries tab")
        remote.press(.select)
        let open = app.buttons["library-open-category:movie"]
        XCTAssertTrue(open.waitForExistence(timeout: 10), app.debugDescription)
        reachForSetup(open, in: app, limit: 8)
        assertFocus(open, in: app, context: "Movies collection")
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].waitForExistence(timeout: 15),
                      app.debugDescription)
        return app
    }

    private func dismissKeyboard(in app: XCUIApplication) {
        let keyboard = app.keyboards.firstMatch
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            keyboard.exists && keyboard.hasFocus
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 5), .completed,
                       "System keyboard must own input before Back")
        remote.press(.menu)
        let closed = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            !keyboard.exists
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [closed], timeout: 5), .completed,
                       "Back must close the system keyboard: \(app.debugDescription)")
    }

    /// Bounded setup steering only; never used to prove the measured journey.
    private func reachForSetup(_ target: XCUIElement, in app: XCUIApplication, limit: Int) {
        let focused = NSPredicate(format: "hasFocus == true")
        let targetIsTab = app.tabBars.buttons.matching(
            NSPredicate(format: "identifier == %@ AND label == %@", target.identifier, target.label)
        ).firstMatch.exists
        for _ in 0..<limit {
            guard target.exists else { break }
            if target.hasFocus { return }
            let current = app.descendants(matching: .any)
                .matching(NSPredicate(format: "hasFocus == true")).firstMatch
            guard current.waitForExistence(timeout: 3) else { break }
            let focusedTab = app.tabBars.buttons.matching(focused).firstMatch
            if !targetIsTab && focusedTab.exists { remote.press(.down); continue }
            if targetIsTab && !focusedTab.exists { remote.press(.up); continue }
            let destination = target.frame
            let source = current.frame
            if destination.maxY <= source.minY { remote.press(.up) }
            else if destination.minY >= source.maxY { remote.press(.down) }
            else { remote.press(destination.midX > source.midX ? .right : .left) }
        }
    }

    private func assertFocus(_ target: XCUIElement, in app: XCUIApplication, context: String,
                             file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in target.exists && target.hasFocus }, object: nil)
        let result = XCTWaiter.wait(for: [expectation], timeout: 5)
        let capture = XCTAttachment(screenshot: app.screenshot())
        capture.name = context
        capture.lifetime = .keepAlways
        add(capture)
        XCTAssertEqual(result, .completed, "\(context): \(app.debugDescription)", file: file, line: line)
    }
}
#endif
