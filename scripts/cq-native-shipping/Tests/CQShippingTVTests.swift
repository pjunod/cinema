// CQ qualification of the installed shipping app (tv.plurx.app) — tvOS, driven by XCUIRemote.
import XCTest
import Foundation

#if os(tvOS)
@MainActor final class CQShippingTVTests: XCTestCase {
    let env = ProcessInfo.processInfo.environment
    var app: XCUIApplication!
    var remote: XCUIRemote { XCUIRemote.shared }
    let t0 = Date()

    func log(_ kind: String, _ fields: [String: Any] = [:]) {
        var f = fields; f["kind"] = kind; f["t_ms"] = Int(Date().timeIntervalSince(t0) * 1000)
        f["wall"] = ISO8601DateFormatter().string(from: Date())
        let data = try! JSONSerialization.data(withJSONObject: f, options: [.sortedKeys])
        print("CQEVT " + String(data: data, encoding: .utf8)!)
    }


    func labels(_ q: XCUIElementQuery) -> [String] { q.allElementsBoundByIndex.map { $0.label }.filter { !$0.isEmpty } }

    func focus(_ target: XCUIElement) -> Bool {
        guard target.waitForExistence(timeout: 10) else { return false }
        let targetIsTab = app.tabBars.buttons.matching(NSPredicate(format: "label == %@", target.label)).firstMatch.exists
        let focused = NSPredicate(format: "hasFocus == true")
        for _ in 0..<80 {
            guard target.exists else { return false }
            if target.hasFocus { return true }
            let focusedTab = app.tabBars.buttons.matching(focused).firstMatch
            if targetIsTab && !focusedTab.exists { remote.press(.up); continue }
            if !targetIsTab && focusedTab.exists { remote.press(.down); continue }
            let b = app.buttons.matching(focused).firstMatch
            let c = app.cells.matching(focused).firstMatch
            let current = b.exists ? b : c.exists ? c : app.descendants(matching: .any).matching(focused).firstMatch
            guard current.exists else { remote.press(.down); continue }
            let tf = target.frame, cf = current.frame
            if tf.minY >= cf.maxY { remote.press(.down) }
            else if tf.maxY <= cf.minY { remote.press(.up) }
            else { let dx = tf.midX - cf.midX, dy = tf.midY - cf.midY
                if abs(dx) > abs(dy) { remote.press(dx > 0 ? .right : .left) } else { remote.press(dy > 0 ? .down : .up) } }
        }
        return target.hasFocus
    }

    func activate(_ target: XCUIElement) -> Bool { guard focus(target) else { return false }; remote.press(.select); return true }

    func revealChrome() {
        if app.buttons["Playback quality"].firstMatch.isHittable { return }
        remote.press(.select); _ = app.buttons["Playback quality"].firstMatch.waitForExistence(timeout: 3)
    }

    func startPlayback() {
        app = XCUIApplication(bundleIdentifier: "tv.plurx.app")
        app.launchArguments = ["-plurx.origin", env["CQ_PROXY"] ?? "", "-plurx.playbackInfoMode", "debug"]
        remote.press(.menu); sleep(1)
        app.launch()
        let title = env["CQ_TITLE"] ?? ""
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 30), "app not foreground")
        sleep(3)
        var onTab = false
        for _ in 0..<4 { remote.press(.up) }
        for _ in 0..<12 {
            let f = app.descendants(matching: .any).matching(NSPredicate(format: "hasFocus == true")).firstMatch
            let label = f.exists ? f.label : ""
            if label == "Libraries" { onTab = true; break }
            log("tab_focus", ["label": label])
            remote.press(label.isEmpty ? .up : .right)
        }
        if !onTab { log("buttons", ["labels": Array(labels(app.buttons).prefix(40))]) }
        XCTAssertTrue(onTab, "Libraries tab")
        remote.press(.select)
        sleep(2)
        let movies = app.buttons["library-open-category:movie"]
        if movies.waitForExistence(timeout: 10) { XCTAssertTrue(activate(movies), "movies") }
        sleep(3)
        let item = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", title)).firstMatch
        XCTAssertTrue(activate(item), "item \(title)")
        sleep(3)
        let startOver = app.buttons["Start over"].firstMatch
        if startOver.waitForExistence(timeout: 4) { XCTAssertTrue(activate(startOver)) }
        else { let play = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "▶")).firstMatch; XCTAssertTrue(activate(play), "play") }
        sleep(15)
        log("playing", ["title": title])
    }

    func openInfoRows() -> [String] {
        revealChrome()
        guard activate(app.buttons["Playback info"].firstMatch) else { return ["info-unreachable"] }
        sleep(1)
        for g in ["Session & history", "Picture & sound"] {
            let b = app.buttons[g].firstMatch
            if b.exists, b.value as? String == "Collapsed" { _ = activate(b); sleep(1) }
        }
        let rows = labels(app.staticTexts) + labels(app.otherElements) + labels(app.buttons)
        remote.press(.menu); sleep(1)
        return rows
    }
    func row(_ rows: [String], _ p: String) -> String { rows.first { $0.hasPrefix(p) } ?? "absent" }

    func evidence(_ tag: String) {
        let rows = openInfoRows()
        log("evidence", ["tag": tag,
            "frames_at_switch": row(rows, "Frames at the switch"), "audio_at_switch": row(rows, "Audio at the switch"),
            "tap_to_new": row(rows, "Tap to new quality"), "interruptions": row(rows, "Buffering interruptions"),
            "display": row(rows, "Player display size"), "method": rows.first { $0.hasPrefix("Transcode") || $0.hasPrefix("Direct") || $0.hasPrefix("Remux") } ?? "absent"])
    }

    func openQuality() -> [String] {
        revealChrome()
        guard activate(app.buttons["Playback quality"].firstMatch) else { return [] }
        _ = app.buttons["Auto"].waitForExistence(timeout: 5)
        return labels(app.buttons).filter { $0 == "Auto" || $0 == "Original" || $0.range(of: #"^\d+p"#, options: .regularExpression) != nil }
    }
    func choose(_ label: String) -> Bool {
        if !app.buttons["Auto"].exists { _ = openQuality() }
        let b = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", label)).firstMatch
        return activate(b)
    }
    func watch(_ seconds: Double) -> [String] {
        var seen: [String] = []; let end = Date().addingTimeInterval(seconds)
        while Date() < end {
            for s in ["Buffering…", "Loading…", "The requested quality did not arrive"] where app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH %@", s)).firstMatch.exists {
                seen.append("\(Int(Date().timeIntervalSince(t0)*1000)):\(s)") }
            usleep(400_000)
        }
        return seen
    }

    func testCampaign() throws {
        continueAfterFailure = true
        startPlayback()
        if env["CQ_PHASE"] == "probe" {
            log("choices", ["choices": openQuality()]); remote.press(.menu)
            log("info_rows", ["rows": openInfoRows()]); return
        }
        evidence("baseline")
        let heights = openQuality().filter { $0.range(of: #"^\d+p"#, options: .regularExpression) != nil }.compactMap { $0.split(separator: "p").first.map(String.init) }
        log("rungs", ["rungs": heights])
        guard heights.count >= 2 else { XCTFail("need two manual rungs, got \(heights)"); return }
        let a = heights[0], b = heights[1]
        for i in stride(from: 1, through: (env["CQ_PHASE"] == "auto" ? 0 : 15), by: 1) {
            let target = (i % 2 == 1) ? b : a
            let at = Date(); let ok = choose("\(target)p")
            log("manual_request", ["n": i, "target": target, "ok": ok])
            let s = watch(18)
            log("manual_window", ["n": i, "target": target, "wait_surfaces": s, "window_ms": Int(Date().timeIntervalSince(at)*1000)])
            evidence("manual-\(i)-\(target)")
        }
        _ = choose("Auto"); log("auto_selected")
        sleep(15); evidence("auto-start-100mbps")
        for (i, mbps) in [10, 25, 10, 25, 3, 25].enumerated() {
            log("link_request", ["stage": i + 1, "mbps": mbps]); sleep(2)
            let s = watch(mbps == 3 ? 60 : 45)
            log("auto_window", ["stage": i + 1, "mbps": mbps, "wait_surfaces": s])
            evidence("auto-\(i + 1)-\(mbps)mbps")
        }
        remote.press(.playPause); log("paused")
    }
}
#endif
