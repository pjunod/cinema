import Foundation
import XCTest
@testable import plurx

/// `GET /api/v1/features` decides which optional top-level tabs and entries
/// exist. These pin the wire shape, the older-server default, and the one
/// visibility rule HomeView, the remote order and the detail page share.
final class ServerFeaturesTests: XCTestCase {
    private func decode(_ json: String) throws -> ServerFeatures {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(ServerFeatures.self, from: Data(json.utf8))
    }

    func testDecodesEverySwitchFromSnakeCase() throws {
        let features = try decode(#"{"live_tv":false,"recordings":true,"shared_libraries":false,"library_channels":true}"#)
        XCTAssertEqual(features, ServerFeatures(liveTv: false, recordings: true, sharedLibraries: false, libraryChannels: true))
    }

    func testMissingKeysDefaultToOnSoAnOlderServerHidesNothing() throws {
        XCTAssertEqual(try decode("{}"), .all)
        let partial = try decode(#"{"recordings":false}"#)
        XCTAssertEqual(partial, ServerFeatures(liveTv: true, recordings: false, sharedLibraries: true, libraryChannels: true))
    }

    func testUnknownKeysAreIgnored() throws {
        XCTAssertEqual(try decode(#"{"live_tv":true,"future_switch":false}"#), .all)
    }

    func testAWrongTypeIsAFailureNotASilentDefault() {
        // AppModel keeps the previous value on any failure; a malformed answer
        // must reach that path rather than decode as "everything on".
        XCTAssertThrowsError(try decode(#"{"live_tv":"no"}"#))
    }

    func testAllIsEverySwitchOn() {
        XCTAssertEqual(ServerFeatures.all, ServerFeatures(liveTv: true, recordings: true, sharedLibraries: true, libraryChannels: true))
    }

    func testOnlyTheOptionalTabsFollowTheirSwitch() {
        let none = ServerFeatures(liveTv: false, recordings: false, sharedLibraries: false, libraryChannels: false)
        let alwaysShown: [HomeTab] = [.home, .libraries, .search, .downloads, .settings]
        for tab in alwaysShown {
            XCTAssertTrue(tab.isVisible(none), "\(tab)")
            XCTAssertTrue(tab.isVisible(.all), "\(tab)")
        }
        for tab in [HomeTab.liveTv, .recordings, .libraryChannels] {
            XCTAssertTrue(tab.isVisible(.all), "\(tab)")
            XCTAssertFalse(tab.isVisible(none), "\(tab)")
        }
        // Each tab follows its own switch, not a neighbour's.
        var liveOff = ServerFeatures.all
        liveOff.liveTv = false
        XCTAssertFalse(HomeTab.liveTv.isVisible(liveOff))
        XCTAssertTrue(HomeTab.recordings.isVisible(liveOff))
        XCTAssertTrue(HomeTab.libraryChannels.isVisible(liveOff))
        var channelsOff = ServerFeatures.all
        channelsOff.libraryChannels = false
        XCTAssertFalse(HomeTab.libraryChannels.isVisible(channelsOff))
        XCTAssertTrue(HomeTab.liveTv.isVisible(channelsOff))
        // Shared libraries is an entry, not a tab: it hides nothing here.
        var sharedOff = ServerFeatures.all
        sharedOff.sharedLibraries = false
        for tab in [HomeTab.home, .libraries, .liveTv, .recordings, .libraryChannels, .search, .downloads, .settings] {
            XCTAssertTrue(tab.isVisible(sharedOff), "\(tab)")
        }
    }

    func testAHiddenSelectionFallsBackToHome() {
        var features = ServerFeatures.all
        features.recordings = false
        XCTAssertEqual(HomeTab.recordings.visibleSelection(features), .home)
        XCTAssertEqual(HomeTab.liveTv.visibleSelection(features), .liveTv)
        XCTAssertEqual(HomeTab.search.visibleSelection(features), .search)
    }

    func testBothShellsGateEveryOptionalTabAndTheSharedEntry() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/HomeView.swift").standardizedFileURL,
            encoding: .utf8
        )
        // One gate per optional tab in the iOS shell and one in the tvOS shell.
        for tab in ["liveTv", "recordings", "libraryChannels"] {
            XCTAssertEqual(
                source.components(separatedBy: "if HomeTab.\(tab).isVisible(model.features) {").count - 1, 2, tab
            )
            XCTAssertEqual(source.components(separatedBy: ".tag(HomeTab.\(tab))").count - 1, 2, tab)
        }
        XCTAssertTrue(source.contains("if model.features.sharedLibraries {\n                    SharedLibrariesEntry()"))
        XCTAssertTrue(source.contains(".onChange(of: model.features) { _, _ in keepSelectionVisible() }"))
        XCTAssertTrue(source.contains("let shared = model.features.sharedLibraries ? [\"libraries:shared\"] : []"))
    }
}
