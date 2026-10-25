// AppKit rendering regressions: the NSMenu built from the model, template
// icon and accessibility text, the upgrade window's cancel/recovery display
// and the log viewer's filter. These create views but never an NSStatusItem,
// a login item or a controller process.
import AppKit
import XCTest
@testable import PlurxTrayCore
@testable import PlurxTrayUI

@MainActor
final class AppKitTests: XCTestCase {
    func snapshot(_ state: ServiceState) -> TraySnapshot {
        let status = StatusResult(installationId: "00000000-0000-4000-8000-000000000001", serviceState: state,
                                  version: "0.7.0", browserOrigin: "http://127.0.0.1:32400",
                                  serverStartup: StartupStateV1(state: .approvalRequired),
                                  trayStartup: StartupStateV1(state: .unknown), transactionId: nil)
        return TraySnapshot(status: status, trayLogin: .enabled, appVersion: "0.7.0", appBuild: "abc")
    }

    final class Target: NSObject { @objc func hit(_ sender: Any?) {} }

    func testRenderedMenuMatchesModelWithStatesAndChecks() {
        let menu = NSMenu()
        let target = Target()
        let entries = buildMenu(snapshot(.running))
        MenuRenderer.render(entries, into: menu, target: target, selector: #selector(Target.hit(_:)))
        XCTAssertEqual(menu.items.count, entries.count)
        XCTAssertFalse(menu.autoenablesItems)
        XCTAssertEqual(menu.items[0].title, "plurx — Running")
        XCTAssertEqual(menu.items[0].accessibilityLabel(), "plurx server Running")
        let start = menu.items.first { $0.title == "Start" }
        XCTAssertEqual(start?.isEnabled, false)
        let stop = menu.items.first { $0.title == "Stop…" }
        XCTAssertEqual(stop?.isEnabled, true)
        XCTAssertTrue(stop?.target === target)
        XCTAssertEqual((stop?.representedObject as? ActionBox)?.action, .stop)
        let server = menu.items.first { $0.title.hasPrefix("Start server at login") }
        XCTAssertEqual(server?.state, .mixed)
        XCTAssertEqual(server?.isEnabled, false)
        XCTAssertEqual(menu.items.first { $0.title == "Show tray app at login" }?.state, .on)
        XCTAssertEqual(menu.items.last?.title, "Quit tray app")
        XCTAssertEqual(menu.items.last?.keyEquivalent, "q")
    }

    func testLongMenuRowsStayCompactAndKeepFullDetails() throws {
        var state = snapshot(.running)
        let full = String(repeating: "W", count: 200)
        state.lastError = "The release could not be verified: " + full
        state.appVersion = full
        state.trayLogin = .unavailable(full)
        let menu = NSMenu()
        let target = Target()
        let entries = buildMenu(state)
        MenuRenderer.render(entries, into: menu, target: target, selector: #selector(Target.hit(_:)))
        for item in menu.items where !item.isSeparatorItem {
            let width = (item.title as NSString).size(withAttributes: [.font: NSFont.menuFont(ofSize: 0)]).width
            XCTAssertLessThanOrEqual(width, MenuRenderer.maximumTitleWidth)
            XCTAssertFalse(item.title.contains("\n"))
        }
        let details = try XCTUnwrap(menu.items.first { ($0.representedObject as? ActionBox)?.action == .showErrorDetails })
        XCTAssertEqual(details.title, "View error details…")
        XCTAssertTrue(details.isEnabled)
        XCTAssertTrue(details.target === target)
        let shortened = try XCTUnwrap(menu.items.first { $0.title.hasPrefix("Tray app ") })
        XCTAssertTrue(shortened.title.hasSuffix("…"))
        XCTAssertEqual(shortened.toolTip, "Tray app " + full + " (abc)")
        XCTAssertEqual(shortened.accessibilityLabel(), shortened.toolTip)
        let login = try XCTUnwrap(menu.items.first { ($0.representedObject as? ActionBox)?.action == .toggleTrayLogin })
        XCTAssertTrue(login.title.hasSuffix("…"))
        XCTAssertEqual(login.state, .mixed)
        XCTAssertFalse(login.isEnabled)
        XCTAssertEqual(MenuRenderer.compactTitle("  hello\nworld  ", font: NSFont.menuFont(ofSize: 0)), "hello world")
    }

    func testIconsAreTemplateImagesWithStateText() throws {
        var bitmaps: [Data] = []
        for state in ServiceState.allCases {
            let image = StatusIcon.image(for: state)
            XCTAssertTrue(image.isTemplate)
            XCTAssertEqual(image.size, StatusIcon.size)
            XCTAssertEqual(image.accessibilityDescription, "plurx server \(state.title)")
            let rep = try XCTUnwrap(NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: 36, pixelsHigh: 36, bitsPerSample: 8, samplesPerPixel: 4,
                hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
            image.draw(in: NSRect(x: 0, y: 0, width: 36, height: 36))
            NSGraphicsContext.restoreGraphicsState()
            bitmaps.append(try XCTUnwrap(rep.representation(using: .png, properties: [:])))
        }
        // Each state has its own shape, not only its own colour.
        XCTAssertEqual(Set(bitmaps).count, ServiceState.allCases.count)
    }

    func testUpgradeWindowShowsCancelOnlyBeforeCommitment() {
        let window = UpgradeWindowController()
        window.update(UpdateResult(state: .waitingForIdle, version: "0.7.1", transactionId: txnUI, detail: nil))
        XCTAssertTrue(window.cancelVisible)
        XCTAssertTrue(window.stateText.contains("0.7.1"))
        window.update(UpdateResult(state: .installing, version: "0.7.1", transactionId: txnUI, detail: nil))
        XCTAssertFalse(window.cancelVisible)
        XCTAssertTrue(window.explanationText.contains("continues even if you quit"))
        window.update(UpdateResult(state: .recoveryRequired, version: "0.7.1", transactionId: txnUI, detail: "phase installing"))
        XCTAssertFalse(window.cancelVisible)
        XCTAssertEqual(window.stateText, "Recovery required — 0.7.1")
        XCTAssertTrue(window.explanationText.contains("needs recovery"))
        XCTAssertTrue(window.explanationText.contains("phase installing"))
    }

    func testUpgradeProgressIsMonotonic() {
        let values = upgradePhases.compactMap(upgradeProgress)
        XCTAssertEqual(values, values.sorted())
        XCTAssertEqual(values.last, 1)
        XCTAssertNil(upgradeProgress(.failed))
    }

    func testLogWindowFiltersAndReportsTruncation() {
        let window = LogWindowController()
        window.show(lines: ["INFO a", "WARN b", "INFO c"], truncated: true)
        XCTAssertEqual(window.shownText, "INFO a\nWARN b\nINFO c")
        window.close()
    }

    func testReleaseNotesURLIsFixedGitHubOrigin() {
        XCTAssertEqual(releaseNotesURL(repository: "pjunod/cinema", version: "0.7.1")?.absoluteString,
                       "https://github.com/pjunod/cinema/releases/tag/v0.7.1")
        XCTAssertNil(releaseNotesURL(repository: "evil.example/x/y", version: "0.7.1"))
        XCTAssertNil(releaseNotesURL(repository: "", version: "0.7.1"))
        XCTAssertNil(releaseNotesURL(repository: "pjunod/cinema", version: "0.7.1/../../x"))
    }

    func testLoginItemStatusMapping() {
        XCTAssertEqual(LoginItem.map(.enabled), .enabled)
        XCTAssertEqual(LoginItem.map(.requiresApproval), .requiresApproval)
        XCTAssertEqual(LoginItem.map(.notRegistered), .notRegistered)
        XCTAssertEqual(LoginItem.map(.notFound), .notFound)
    }

    func testAppInfoReadsStampedKeys() throws {
        let info = AppInfo.from(Bundle(for: AppKitTests.self))
        XCTAssertEqual(info.build, "unknown", "an unstamped bundle never invents a build id")
    }
}

let txnUI = "00000000-0000-4000-8000-000000000002"
