// CQ qualification of the installed shipping app (tv.plurx.app) — iOS/iPadOS.
// Env (via TEST_RUNNER_ prefix): CQ_PROXY, CQ_PHASE (home|probe|campaign|auto), CQ_TITLE (movie title).
import XCTest
import Foundation

#if os(iOS)
final class CQShippingTests: XCTestCase {
    let env = ProcessInfo.processInfo.environment
    var app: XCUIApplication!
    let t0 = Date()

    func log(_ kind: String, _ fields: [String: Any] = [:]) {
        var f = fields; f["kind"] = kind; f["t_ms"] = Int(Date().timeIntervalSince(t0) * 1000)
        f["wall"] = ISO8601DateFormatter().string(from: Date())
        let data = try! JSONSerialization.data(withJSONObject: f, options: [.sortedKeys])
        print("CQEVT " + String(data: data, encoding: .utf8)!)
    }


    func labels(_ q: XCUIElementQuery) -> [String] { q.allElementsBoundByIndex.map { $0.label }.filter { !$0.isEmpty } }

    func revealControls() {
        if app.buttons["Playback quality"].isHittable { return }
        let surface = app.buttons["Show or hide playback controls"]
        if surface.exists { surface.tap() } else { app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap() }
        _ = app.buttons["Playback quality"].waitForExistence(timeout: 3)
    }

    func startPlayback() throws {
        app = XCUIApplication(bundleIdentifier: "tv.plurx.app")
        app.launchArguments = ["-plurx.origin", env["CQ_PROXY"] ?? "", "-plurx.playbackInfoMode", "debug"]
        app.launch()
        let title = env["CQ_TITLE"] ?? ""
        let libs = app.tabBars.buttons["Libraries"].exists ? app.tabBars.buttons["Libraries"] : app.buttons["Libraries"].firstMatch
        if !libs.waitForExistence(timeout: 30) { log("home_missing", ["buttons": Array(labels(app.buttons).prefix(40)), "texts": Array(labels(app.staticTexts).prefix(20))]) }
        XCTAssertTrue(libs.exists, "home did not load")
        if !(libs.exists) { throw XCTSkip("navigation failed before playback; see the logged screen") }
        libs.tap()
        let movies = app.buttons["library-open-category:movie"]
        if movies.waitForExistence(timeout: 15) { movies.tap() }
        let result = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", title)).firstMatch
        var tries = 0
        while !result.exists && tries < 12 { app.swipeUp(); tries += 1 }
        XCTAssertTrue(result.waitForExistence(timeout: 10), "no library item for \(title)"); result.tap()
        let startOver = app.buttons["Start over"]
        if startOver.waitForExistence(timeout: 6) { startOver.tap() } else { app.buttons["Play"].firstMatch.tap() }
        let pause = app.buttons.matching(NSPredicate(format: "identifier == 'player-play-pause' AND label == 'Pause'")).firstMatch
        XCTAssertTrue(pause.waitForExistence(timeout: 60), "playback did not start")
        log("playing", ["title": title])
    }

    func openInfo() -> [String] {
        revealControls()
        app.buttons["Playback info"].tap()
        sleep(1)
        for g in ["Session & history", "Picture & sound"] {
            let b = app.buttons[g]; if b.exists, b.value as? String == "Collapsed" { b.tap() }
        }
        let rows = labels(app.staticTexts) + labels(app.otherElements) + labels(app.buttons)
        let close = app.buttons["Close playback info"]; if close.exists { close.tap() }
        return rows
    }

    func row(_ rows: [String], _ prefix: String) -> String { rows.first { $0.hasPrefix(prefix) } ?? "absent" }

    func qualityChoices() -> [String] {
        revealControls()
        app.buttons["Playback quality"].tap()
        _ = app.buttons["Auto"].waitForExistence(timeout: 5)
        let c = labels(app.buttons).filter { $0 == "Auto" || $0 == "Original" || $0.range(of: #"^\d+p"#, options: .regularExpression) != nil }
        return c
    }

    func choose(_ prefix: String) -> Bool {
        if !app.buttons["Auto"].exists { _ = qualityChoices() }
        let b = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", prefix)).firstMatch
        guard b.exists else { return false }
        b.tap(); return true
    }

    /// Watch for visible wait surfaces for `seconds`; returns sightings.
    func watch(_ seconds: Double) -> [String] {
        var seen: [String] = []; let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            for s in ["Buffering…", "Loading…", "The requested quality did not arrive"] where app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", s)).firstMatch.exists {
                seen.append("\(Int(Date().timeIntervalSince(t0)*1000)):\(s)")
            }
            usleep(400_000)
        }
        return seen
    }

    func evidence(_ tag: String) {
        let rows = openInfo()
        log("evidence", ["tag": tag,
            "frames_at_switch": row(rows, "Frames at the switch"), "audio_at_switch": row(rows, "Audio at the switch"),
            "tap_to_new": row(rows, "Tap to new quality"), "interruptions": row(rows, "Buffering interruptions"),
            "display": row(rows, "Player display size"), "method": rows.first { $0.hasPrefix("Transcode") || $0.hasPrefix("Direct") || $0.hasPrefix("Remux") } ?? "absent"])
    }

    func testHome() throws {
        guard env["CQ_PHASE"] == "home" else { return }
        app = XCUIApplication(bundleIdentifier: "tv.plurx.app")
        app.launchArguments = ["-plurx.origin", env["CQ_PROXY"] ?? ""]
        app.launch()
        XCTAssertTrue(app.tabBars.buttons["Libraries"].waitForExistence(timeout: 30))
        app.tabBars.buttons["Libraries"].tap()
        let movies = app.buttons["library-open-category:movie"]
        if movies.waitForExistence(timeout: 15) { movies.tap() }
        sleep(6)
        log("library", ["buttons": Array(labels(app.buttons).prefix(80))])
    }

    func testCampaign() throws {
        guard env["CQ_PHASE"] != "home" else { return }
        continueAfterFailure = true
        try startPlayback()
        if env["CQ_PHASE"] == "probe" {
            sleep(8)
            log("choices", ["choices": qualityChoices()])
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.1, dy: 0.1)).tap()
            log("info_rows", ["rows": openInfo()])
            return
        }
        sleep(20)
        evidence("baseline")
        let choices = qualityChoices().filter { $0.range(of: #"^\d+p"#, options: .regularExpression) != nil }
        let heights = choices.compactMap { $0.split(separator: "p").first.map(String.init) }
        log("rungs", ["rungs": heights])
        guard heights.count >= 2 else { XCTFail("need two manual rungs, got \(heights)"); return }
        let a = heights[0], b = heights[1]
        for i in stride(from: 1, through: (env["CQ_PHASE"] == "auto" ? 0 : 15), by: 1) {
            let target = (i % 2 == 1) ? b : a
            let at = Date()
            let ok = choose("\(target)p")
            log("manual_request", ["n": i, "target": target, "ok": ok])
            let sightings = watch(18)
            log("manual_window", ["n": i, "target": target, "wait_surfaces": sightings, "window_ms": Int(Date().timeIntervalSince(at)*1000)])
            evidence("manual-\(i)-\(target)")
        }
        _ = choose("Auto"); log("auto_selected")
        sleep(15); evidence("auto-start-100mbps")
        for (i, mbps) in [10, 25, 10, 25, 3, 25].enumerated() {
            log("link_request", ["stage": i + 1, "mbps": mbps]); sleep(2)
            let sightings = watch(mbps == 3 ? 60 : 45)
            log("auto_window", ["stage": i + 1, "mbps": mbps, "wait_surfaces": sightings])
            evidence("auto-\(i + 1)-\(mbps)mbps")
        }
        revealControls(); app.buttons["player-play-pause"].tap(); log("paused")
    }
}
#endif
