#if os(tvOS)
import XCTest
import UIKit

/// Shared shipping-route setup; measured assertions always observe native focus.
@MainActor
class TVNavigationTestCase: XCTestCase {
    let remote = XCUIRemote.shared

    func openLibrary(origin: String, presentation: String = "grid", collectionID: String = "movie", layout: ViewerLayoutChoice = .standard, posterSize: String? = nil) -> XCUIApplication {
        let app = openSignIn(origin: origin, layout: layout, posterSize: posterSize)
        signInToFixture(in: app)
        let libraries = app.viewerTab("Libraries")
        XCTAssertTrue(libraries.waitForExistence(timeout: 15), app.debugDescription)
        reachForSetup(libraries, in: app, limit: 8)
        assertFocus(libraries, in: app, context: "Libraries tab")
        remote.press(.select)
        let open = app.buttons["library-open-category:" + collectionID]
        XCTAssertTrue(open.waitForExistence(timeout: 10), app.debugDescription)
        reachForSetup(open, in: app, limit: 8)
        assertFocus(open, in: app, context: "Movies collection")
        remote.press(.select)
        XCTAssertTrue(app.staticTexts["library-loaded-summary"].waitForExistence(timeout: 15),
                      app.debugDescription)
        establishPresentation(presentation, in: app)
        return app
    }

    func openSignIn(origin: String, layout: ViewerLayoutChoice = .standard, posterSize: String? = nil) -> XCUIApplication {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.layout", layout.rawValue]
        if let posterSize { app.launchArguments += ["-plurx.posterSize", posterSize] }
        app.launch()
        let manual = app.buttons["Add server manually"]
        let change = app.buttons["Use a different server"]
        let libraries = app.viewerTab("Libraries")
        let screen = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in
            manual.exists || change.exists || libraries.exists
        }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [screen], timeout: 45), .completed, app.debugDescription)
        if !manual.exists {
            if change.exists {
                reachForSetup(change, in: app, limit: 8)
                assertFocus(change, in: app, context: "Choose another fixture server")
                remote.press(.select)
            } else {
                let settings = app.viewerTab("Settings")
                reachForSetup(settings, in: app, limit: 16)
                assertFocus(settings, in: app, context: "Settings before changing fixture server")
                remote.press(.select)
                let account = app.buttons["settings-category-Account"]
                reachForSetup(account, in: app, limit: 12)
                assertFocus(account, in: app, context: "Account before changing fixture server")
                remote.press(.right)
                let leave = app.buttons["Change server"]
                reachForSetup(leave, in: app, limit: 8)
                assertFocus(leave, in: app, context: "Leave previous fixture server")
                remote.press(.select)
            }
        }
        connectManually(origin: origin, in: app)
        return app
    }

    func connectManually(origin: String, in app: XCUIApplication) {
        let manual = app.buttons["Add server manually"]
        reachForSetup(manual, in: app, limit: 8)
        assertFocus(manual, in: app, context: "Manual fixture server setup")
        remote.press(.select)
        let address = app.textFields.firstMatch
        reachForSetup(address, in: app, limit: 8)
        assertFocus(address, in: app, context: "Fixture server address")
        remote.press(.select)
        let previous = address.value as? String ?? ""
        address.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: previous.count) + origin)
        dismissKeyboard(in: app)
        let connect = app.buttons["Connect"]
        reachForSetup(connect, in: app, limit: 8)
        assertFocus(connect, in: app, context: "Connect to fixture server")
        remote.press(.select)
        XCTAssertTrue(app.textFields["Username"].waitForExistence(timeout: 10), app.debugDescription)
    }

    func signInToFixture(in app: XCUIApplication) {
        let username = app.textFields["Username"]
        reachForSetup(username, in: app, limit: 4)
        assertFocus(username, in: app, context: "Username setup")
        remote.press(.select)
        let previous = username.value as? String ?? ""
        username.typeText(String(repeating: XCUIKeyboardKey.delete.rawValue, count: previous.count) + "fixture")
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

    /// Preferences must remain writable. NSArgumentDomain launch overrides
    /// would defeat a later user choice, so setup uses the ordinary menu.
    func establishPresentation(_ presentation: String, in app: XCUIApplication) {
        let view = app.buttons["library:view"]
        reachForSetup(view, in: app, limit: 8)
        assertFocus(view, in: app, context: "Presentation setup opener")
        remote.press(.select)
        let desired = app.buttons["choice:" + presentation]
        XCTAssertTrue(desired.waitForExistence(timeout: 5), app.debugDescription)
        guard let selected = [app.buttons["choice:rows"], app.buttons["choice:grid"]].first(where: { $0.isSelected }) else {
            XCTFail("View must expose its current selected value: \(app.debugDescription)")
            return
        }
        assertFocus(selected, in: app, context: "Current presentation setup")
        if selected.identifier == "choice:" + presentation { remote.press(.menu) }
        else {
            remote.press(presentation == "rows" ? .up : .down)
            assertFocus(desired, in: app, context: "Desired presentation setup")
            remote.press(.select)
        }
        assertFocus(view, in: app, context: "Presentation setup restores opener")
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
        guard target.exists || target.waitForExistence(timeout: 5) else {
            XCTFail("Setup control is missing: \(app.debugDescription)")
            return
        }
        let focused = NSPredicate(format: "hasFocus == true")
        let targetIsTab = app.tabBars.buttons.matching(
            NSPredicate(format: "identifier == %@ AND label == %@", target.identifier, target.label)
        ).firstMatch.exists
        for _ in 0..<limit {
            guard target.exists else { break }
            if hasNativeFocus(target, in: app) { return }
            let current = app.descendants(matching: .any)
                .matching(NSPredicate(format: "hasFocus == true")).firstMatch
            guard current.exists || current.waitForExistence(timeout: 3) else { break }
            let focusedTab = app.tabBars.buttons.matching(focused).firstMatch
            if !targetIsTab && focusedTab.exists { remote.press(.down); continue }
            if targetIsTab && !focusedTab.exists { remote.press(.up); continue }
            let destination = target.frame
            let source = current.frame
            if abs(destination.midX - source.midX) < min(destination.width, source.width) / 2
                && abs(destination.midY - source.midY) > min(destination.height, source.height) / 2 {
                remote.press(destination.midY > source.midY ? .down : .up)
            }
            else if destination.minY >= source.maxY { remote.press(.down) }
            else if destination.maxX <= source.minX { remote.press(.left) }
            else if destination.minX >= source.maxX { remote.press(.right) }
            else if destination.maxY <= source.minY { remote.press(.up) }
            else { remote.press(destination.midX > source.midX ? .right : .left) }
        }
    }

    /// SwiftUI's native menu picker focuses an anonymous leaf inside its
    /// identified Button. Accept only that observed exact-frame descendant,
    /// never a nearby row, a larger ancestor, existence or hittability.
    func hasNativeFocus(_ target: XCUIElement, in app: XCUIApplication) -> Bool {
        guard target.exists else { return false }
        if target.hasFocus { return true }
        guard !target.identifier.isEmpty,
              app.descendants(matching: .any).matching(identifier: target.identifier).count == 1 else { return false }
        let focused = NSPredicate(format: "hasFocus == true")
        let proxy = target.descendants(matching: .other).matching(focused).firstMatch
        guard proxy.exists, proxy.descendants(matching: .any).matching(focused).count == 0 else { return false }
        let expected = target.frame
        let actual = proxy.frame
        return expected.width > 0 && expected.height > 0
            && abs(expected.minX - actual.minX) <= 1 && abs(expected.minY - actual.minY) <= 1
            && abs(expected.width - actual.width) <= 1 && abs(expected.height - actual.height) <= 1
    }

    /// The navigation movie/HLS fixtures are solid black. Native focus must
    /// remain on the reveal target without painting a button over that video.
    func assertHiddenFixtureVideoIsUnobstructed(in app: XCUIApplication,
                                               file: StaticString = #filePath, line: UInt = #line) {
        let screenshot = app.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = "Hidden video must contain no visible focus control"
        attachment.lifetime = .keepAlways
        add(attachment)
        guard let image = screenshot.image.cgImage else {
            XCTFail("Missing screenshot pixels", file: file, line: line)
            return
        }
        let width = 128, height = 72
        var pixels = [UInt8](repeating: 0, count: width * height * 4)
        let rendered = pixels.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: width, height: height,
                                          bitsPerComponent: 8, bytesPerRow: width * 4,
                                          space: CGColorSpaceCreateDeviceRGB(),
                                          bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue
                                            | CGBitmapInfo.byteOrder32Big.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
            return true
        }
        XCTAssertTrue(rendered, "Could not inspect screenshot pixels", file: file, line: line)
        var brightPixels = 0
        for y in (height / 3)..<(height * 2 / 3) {
            for x in (width / 3)..<(width * 2 / 3) {
                let offset = (y * width + x) * 4
                if pixels[offset] > 32 || pixels[offset + 1] > 32 || pixels[offset + 2] > 32 {
                    brightPixels += 1
                }
            }
        }
        XCTAssertLessThanOrEqual(brightPixels, 1,
                                "Hidden video has \(brightPixels) bright center pixels; the focus target must not draw chrome",
                                file: file, line: line)
    }

    func assertFocus(_ target: XCUIElement, in app: XCUIApplication, context: String,
                             timeout: TimeInterval = 5,
                             captureOnSuccess: Bool = true,
                             file: StaticString = #filePath, line: UInt = #line) {
        // Observe completed native moves immediately. XCTest's first polling
        // interval otherwise spends a second of the player's four-second
        // idle allowance on every assertion without sending any remote input.
        let result: XCTWaiter.Result
        if hasNativeFocus(target, in: app) {
            result = .completed
        } else {
            let expectation = XCTNSPredicateExpectation(
                predicate: NSPredicate { _, _ in self.hasNativeFocus(target, in: app) }, object: nil)
            result = XCTWaiter.wait(for: [expectation], timeout: timeout)
        }
        let focusDebug = app.staticTexts["choice-focus-debug"]
        if focusDebug.exists { print("NATIVE_CHOICE_FOCUS_DEBUG=\(focusDebug.label)") }
        if result != .completed || captureOnSuccess {
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
        }
        XCTAssertEqual(result, .completed, "\(context): \(app.debugDescription)", file: file, line: line)
    }
}
#endif
