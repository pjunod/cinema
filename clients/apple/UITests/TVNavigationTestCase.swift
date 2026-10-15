#if os(tvOS)
import XCTest

/// Shared shipping-route setup; measured assertions always observe native focus.
@MainActor
class TVNavigationTestCase: XCTestCase {
    let remote = XCUIRemote.shared

    func openLibrary(origin: String, presentation: String = "grid") -> XCUIApplication {
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

    func dismissKeyboard(in app: XCUIApplication) {
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
    func reachForSetup(_ target: XCUIElement, in app: XCUIApplication, limit: Int) {
        guard target.waitForExistence(timeout: 5) else {
            XCTFail("Setup control is missing: \(app.debugDescription)")
            return
        }
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
            if destination.minY >= source.maxY { remote.press(.down) }
            else if destination.maxX <= source.minX { remote.press(.left) }
            else if destination.minX >= source.maxX { remote.press(.right) }
            else if destination.maxY <= source.minY { remote.press(.up) }
            else { remote.press(destination.midX > source.midX ? .right : .left) }
        }
    }

    func assertFocus(_ target: XCUIElement, in app: XCUIApplication, context: String,
                             file: StaticString = #filePath, line: UInt = #line) {
        let expectation = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in target.exists && target.hasFocus }, object: nil)
        let result = XCTWaiter.wait(for: [expectation], timeout: 5)
        let focusDebug = app.staticTexts["choice-focus-debug"]
        if focusDebug.exists { print("NATIVE_CHOICE_FOCUS_DEBUG=\(focusDebug.label)") }
        let screenshot = app.screenshot()
        let directory = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent("PlurxNavigationCaptures")
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let safeName = context.replacingOccurrences(of: "/", with: "-")
        let nativePath = directory.appendingPathComponent(safeName + ".png")
        try? screenshot.pngRepresentation.write(to: nativePath)
        print("NAVIGATION_SCREENSHOT_PATH=\(nativePath.path)")
        let capture = XCTAttachment(screenshot: screenshot)
        capture.name = context
        capture.lifetime = .keepAlways
        add(capture)
        XCTAssertEqual(result, .completed, "\(context): \(app.debugDescription)", file: file, line: line)
    }
}
#endif
