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
        let export = app.buttons["diagnostics-export"]
        reveal(export, in: app)
        XCTAssertTrue(export.isEnabled, "Missing server evidence must not block local export")
        #endif
        let send = app.buttons["Send report"]
        XCTAssertFalse(send.exists, "The ordinary report flow exports an attachment")
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
        XCTAssertFalse(send.exists, "Recovery must keep the archive-only flow")
        let savedHistory = app.buttons["diagnostics-resume-" + savedID]
        reveal(savedHistory, in: app)
        XCTAssertTrue(savedHistory.isEnabled, "Resuming an incident must preserve accessible local history")
    }

    func testDiagnosticZIPOpensSystemShareSheetWithoutSupport() throws {
        #if os(tvOS)
        throw XCTSkip("Native TV archive transfer is not implemented")
        #else
        continueAfterFailure = false
        let app = XCUIApplication()
        app.launchArguments += ["-plurx.origin", "http://127.0.0.1:9"]
        app.launch(); defer { app.terminate() }
        openReport(app)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "Frozen local report"), object: app.staticTexts["diagnostics-status"])
        XCTAssertEqual(XCTWaiter.wait(for: [ready], timeout: 20), .completed)
        let export = app.buttons["diagnostics-export"]
        reveal(export, in: app); XCTAssertTrue(export.isEnabled); export.tap()
        XCTAssertTrue(app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "Save to Files")).firstMatch.waitForExistence(timeout: 15), "The export action must open the actual system share sheet: " + app.debugDescription)
        XCTAssertFalse(app.buttons["Send report"].exists)
        #endif
    }

    func testTVZIPDownloadsSelectedIncidentAndCloseRevokesNewLink() async throws {
        #if os(iOS)
        throw XCTSkip("TV local transfer uses the TV report interface")
        #else
        continueAfterFailure = false
        let app = XCUIApplication(); app.launchArguments += ["-plurx.origin", "http://127.0.0.1:9"]
        app.launch(); defer { app.terminate() }; openReport(app)
        let ready = XCTNSPredicateExpectation(predicate: NSPredicate(format: "label CONTAINS %@", "Frozen local report"), object: app.staticTexts["diagnostics-status"])
        await fulfillment(of: [ready], timeout: 20)
        let identity = app.staticTexts.matching(NSPredicate(format: "label MATCHES %@", "Report [0-9a-f-]{36}")).firstMatch
        reveal(identity, in: app); let reportID = identity.label.replacingOccurrences(of: "Report ", with: "")
        let action = app.buttons["diagnostics-export-tv"]; reveal(action, in: app); select(action, in: app)
        let link = app.staticTexts["diagnostics-export-url"]
        XCTAssertTrue(link.waitForExistence(timeout: 15), app.debugDescription); reveal(link, in: app)
        let url = try XCTUnwrap(URL(string: link.label))
        let configuration = URLSessionConfiguration.ephemeral; configuration.timeoutIntervalForRequest = 5
        let session = URLSession(configuration: configuration); defer { session.invalidateAndCancel() }
        let (page, _) = try await session.data(from: url)
        XCTAssertTrue(String(decoding: page, as: UTF8.self).contains("Download diagnostic ZIP"))
        var request = URLRequest(url: url); request.httpMethod = "POST"
        let (bytes, response) = try await session.data(for: request)
        XCTAssertEqual((response as? HTTPURLResponse)?.mimeType, "application/zip")
        XCTAssertTrue(bytes.starts(with: [0x50, 0x4b, 3, 4]))
        XCTAssertTrue(String(decoding: bytes, as: UTF8.self).contains(reportID))
        XCTAssertTrue(String(decoding: bytes, as: UTF8.self).contains("manifest.json"))
        XCTAssertFalse(app.buttons["Send report"].exists)
        reveal(action, in: app, towardStart: true); select(action, in: app)
        XCTAssertTrue(link.waitForExistence(timeout: 15)); reveal(link, in: app)
        let cancelledURL = try XCTUnwrap(URL(string: link.label)); XCTAssertNotEqual(url, cancelledURL)
        select(app.buttons["Close"], in: app)
        do { _ = try await session.data(from: cancelledURL); XCTFail("Closing must revoke the local transfer") } catch {}
        #endif
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
