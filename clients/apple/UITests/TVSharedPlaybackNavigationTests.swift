#if os(tvOS)
import XCTest

@MainActor
final class TVSharedPlaybackNavigationTests: TVNavigationTestCase {
    func testSharedPlaybackRevealQualityReportAndBackRetainSession() throws {
        let server = try TVNavigationServer(playableShared: true)
        let origin = try server.start()
        defer { server.stop() }
        let app = openLibrary(origin: origin)
        defer { app.terminate() }
        remote.press(.menu)
        let entry = app.buttons["shared-libraries-entry"]
        reachForSetup(entry, in: app, limit: 8)
        remote.press(.select)
        let library = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Shared fixture movies")).firstMatch
        reachForSetup(library, in: app, limit: 8)
        remote.press(.select)
        let title = app.buttons.matching(NSPredicate(format: "label CONTAINS %@", "Shared fixture title")).firstMatch
        reachForSetup(title, in: app, limit: 8)
        assertFocus(title, in: app, context: "Shared title opener")
        remote.press(.select)
        let about = app.buttons["shared:about"]
        reachForSetup(about, in: app, limit: 8)
        assertFocus(about, in: app, context: "Shared About opener")
        remote.press(.select)
        for index in 0..<4 {
            if index > 0 { remote.press(.down) }
            let paragraph = app.descendants(matching: .any).matching(identifier: "info:paragraph:\(index)").firstMatch
            assertFocus(paragraph, in: app, context: "Shared synopsis region \(index + 1)")
            if index == 3 { XCTAssertTrue(paragraph.label.contains("FINAL SHARED SYNOPSIS LINE")) }
        }
        remote.press(.menu)
        assertFocus(about, in: app, context: "Shared About Back restores opener")
        let play = app.buttons["Play Shared file"]
        reachForSetup(play, in: app, limit: 8)
        assertFocus(play, in: app, context: "Shared file Play")
        remote.press(.select)
        let pause = app.buttons["shared-play-pause"]
        XCTAssertTrue(pause.waitForExistence(timeout: 20), app.debugDescription)
        assertFocus(pause, in: app, context: "Shared initial focus is Pause")
        let hidden = XCTNSPredicateExpectation(predicate: NSPredicate { _, _ in !pause.exists }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [hidden], timeout: 10), .completed, app.debugDescription)
        assertFocus(app.buttons["shared-player-reveal"], in: app, context: "Hidden shared video owns remote focus")
        assertHiddenFixtureVideoIsUnobstructed(in: app)
        remote.press(.right)
        assertFocus(pause, in: app, context: "Shared first Right reveals Pause")
        remote.press(.select)
        let quality = app.buttons["shared-quality"]
        reachForSetup(quality, in: app, limit: 8)
        assertFocus(quality, in: app, context: "Shared Quality opener")
        remote.press(.select)
        let selected = app.buttons.matching(NSPredicate(format: "identifier BEGINSWITH 'choice:' AND selected == true")).firstMatch
        assertFocus(selected, in: app, context: "Shared selected quality")
        remote.press(.menu)
        assertFocus(quality, in: app, context: "Shared Quality Back restores opener")
        let more = app.buttons["shared-more"]
        reachForSetup(more, in: app, limit: 8)
        assertFocus(more, in: app, context: "Shared More")
        remote.press(.select)
        let report = app.buttons["diagnostics-open"]
        reachForSetup(report, in: app, limit: 8)
        assertFocus(report, in: app, context: "Shared Report opener")
        remote.press(.select)
        assertFocus(app.buttons["diagnostics-done"], in: app, context: "Shared Report safe entry")
        remote.press(.menu)
        assertFocus(report, in: app, context: "Shared Report Back restores opener")
        remote.press(.menu)
        assertFocus(more, in: app, context: "Shared More Back restores opener")
        remote.press(.menu)
        assertFocus(app.buttons["shared-player-reveal"], in: app, context: "Shared More returns to hidden picture")
        remote.press(.right)
        assertFocus(more, in: app, context: "Shared reveal restores the last More control")
        remote.press(.menu)
        assertFocus(app.buttons["shared-player-reveal"], in: app, context: "Shared picture before exit")
        remote.press(.menu)
        assertFocus(play, in: app, context: "Shared playback Back restores file")
        let start = "/api/v1/shared/imports/11111111-1111-4111-8111-111111111111/files/" + String(repeating: "L", count: 236) + "/hls/sessions"
        XCTAssertEqual(server.requestCount("POST " + start), 1)
    }
}
#endif
