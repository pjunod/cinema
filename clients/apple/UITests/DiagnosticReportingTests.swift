import XCTest

/// Exercises the shipped report sheet offline, without real credentials or
/// a production endpoint. The simulator is owned by this qualification run.
@MainActor
final class DiagnosticReportingTests: XCTestCase {
    func testOfflineCaptureKeepsUploadExplicitAndRecoversAfterRelaunch() throws {
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.origin", "http://127.0.0.1:9"]
        app.launch()
        defer { app.terminate() }
        openReport(app)
        let status = app.staticTexts["diagnostics-status"]
        let frozen = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "Frozen local report"), object: status)
        XCTAssertEqual(XCTWaiter.wait(for: [frozen], timeout: 20), .completed, app.debugDescription)
        let identity = app.staticTexts.matching(NSPredicate(format: "label MATCHES %@", "Report [0-9a-f-]{36}")).firstMatch
        reveal(identity, in: app)
        let originalIdentity = identity.label
        #if os(iOS)
        let export = app.buttons["Save / share local report"]
        reveal(export, in: app)
        XCTAssertTrue(export.isEnabled, "Missing server evidence must not block local export")
        #endif
        let send = app.buttons["Send report"]
        reveal(send, in: app)
        XCTAssertFalse(send.isEnabled, "A fresh report must not imply upload consent")
        select(app.buttons["Close"], in: app)
        app.terminate()
        app.launch()
        openReport(app)
        let savedID = originalIdentity.replacingOccurrences(of: "Report ", with: "")
        let resume = app.buttons["diagnostics-resume-" + savedID]
        reveal(resume, in: app)
        select(resume, in: app)
        let recovered = app.staticTexts["diagnostics-status"]
        reveal(recovered, in: app, towardStart: true)
        XCTAssertTrue(recovered.label.contains("Recovered local report"), app.debugDescription)
        reveal(identity, in: app)
        XCTAssertEqual(identity.label, originalIdentity, "Recovery must open the incident saved before relaunch")
        reveal(send, in: app)
        XCTAssertFalse(send.isEnabled, "Recovery must not imply upload consent")
        let savedHistory = app.buttons["diagnostics-resume-" + savedID]
        reveal(savedHistory, in: app)
        XCTAssertTrue(savedHistory.isEnabled, "Resuming an incident must preserve accessible local history")
    }

    private func reveal(_ element: XCUIElement, in app: XCUIApplication, towardStart: Bool = false) {
        // SwiftUI Form creates rows as they enter the viewport. Exercise the
        // same scrolling a person uses before inspecting a lazy row.
        for _ in 0..<40 {
            if element.exists && !element.frame.isEmpty && element.frame.intersects(app.frame) { break }
            #if os(tvOS)
            // Returning from a resumed row can focus the toolbar while the
            // Form keeps its old scroll offset. Enter its first action to
            // bring the top rows back into view.
            let fromToolbar = towardStart && app.buttons["Close"].hasFocus
            XCUIRemote.shared.press(towardStart && !fromToolbar ? .up : .down)
            #else
            if towardStart { app.swipeDown() } else { app.swipeUp() }
            #endif
        }
        XCTAssertTrue(element.exists && !element.frame.isEmpty && element.frame.intersects(app.frame), app.debugDescription)
    }

    private func openReport(_ app: XCUIApplication) {
        let open = app.buttons["diagnostics-open"]
        XCTAssertTrue(open.waitForExistence(timeout: 15), app.debugDescription)
        select(open, in: app)
        XCTAssertTrue(app.staticTexts["diagnostics-status"].waitForExistence(timeout: 10), app.debugDescription)
    }

    private func select(_ target: XCUIElement, in app: XCUIApplication) {
        #if os(tvOS)
        let focused = NSPredicate(format: "hasFocus == true")
        // SwiftUI Form gives remote focus to the action's Cell rather than
        // its nested Button. Accept only the row for this exact action.
        let actionCell = app.cells.matching(NSPredicate(format: "label == %@", target.label)).firstMatch
        for _ in 0..<80 {
            if target.hasFocus || (actionCell.exists && actionCell.hasFocus) { break }
            let current = app.descendants(matching: .any).matching(focused).firstMatch
            XCTAssertTrue(current.exists, app.debugDescription)
            let destination = target.frame
            let origin = current.frame
            if destination.isEmpty { XCUIRemote.shared.press(.down) }
            else if destination.minY >= origin.maxY { XCUIRemote.shared.press(.down) }
            else if destination.maxY <= origin.minY { XCUIRemote.shared.press(.up) }
            else {
                let dx = destination.midX - origin.midX
                let dy = destination.midY - origin.midY
                if abs(dx) > abs(dy) { XCUIRemote.shared.press(dx > 0 ? .right : .left) }
                else { XCUIRemote.shared.press(dy > 0 ? .down : .up) }
            }
        }
        XCTAssertTrue(target.hasFocus || (actionCell.exists && actionCell.hasFocus), "The physical remote must focus the exact action: " + app.debugDescription)
        XCUIRemote.shared.press(.select)
        #else
        target.tap()
        #endif
    }
}
