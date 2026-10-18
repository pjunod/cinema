import XCTest
@testable import plurx

final class TVSecondaryNavigationTests: XCTestCase {
    #if os(tvOS)
    func testHardwareIntentRequiresFreshActivationNotReleaseOrRepeatedValue() {
        var transitions = RemotePhysicalInputTransitions()
        XCTAssertTrue(transitions.observe(["select"]))
        XCTAssertFalse(transitions.observe(["select"]))
        XCTAssertFalse(transitions.observe([]))
        XCTAssertTrue(transitions.observe(["select"]))
        XCTAssertFalse(transitions.observe([]))
        XCTAssertTrue(transitions.observe(["right"]))
        XCTAssertFalse(transitions.observe(["right"]))
        XCTAssertTrue(transitions.observe(["up"]))
        XCTAssertFalse(transitions.observe([]))
        transitions.reset(to: ["right"])
        XCTAssertFalse(transitions.observe(["right"]))
        XCTAssertFalse(transitions.observe([]))
        XCTAssertTrue(transitions.observe(["right"]))
    }
    #endif
    @MainActor
    func testReportReturnBelongsToItsPresenterAndRunsOnlyOnce() {
        let reports = DiagnosticReportModel()
        var restored = 0
        reports.presenter = "settings"
        reports.presented = true
        reports.prepareFocusReturn = { { restored += 1 } }
        reports.close()
        reports.restoreFocus(after: "player")
        XCTAssertEqual(restored, 0)
        reports.restoreFocus(after: "settings")
        XCTAssertEqual(restored, 1)
        reports.restoreFocus(after: "settings")
        XCTAssertEqual(restored, 1)
    }
    @MainActor
    func testPhysicalReturnSurvivesNativeSettlementButCancelsOnNewInput() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.openModal(scope: "confirmation", opener: "delete:1") {}
        navigation.closeModal()
        let revision = navigation.physicalInputRevision
        navigation.nativeControlFocus(scope: "home", key: "delete:1", focused: true)
        XCTAssertEqual(navigation.physicalInputRevision, revision)
        XCTAssertTrue(navigation.canRestorePhysicalFocus(after: "confirmation"))
        navigation.physicalInput()
        XCTAssertGreaterThan(navigation.physicalInputRevision, revision)
        XCTAssertFalse(navigation.canRestorePhysicalFocus(after: "confirmation"))
    }
    func testRemovedOpenerChoosesNextThenPreviousThenRecovery() {
        let previous = ["recording:1", "delete:1", "recording:2", "delete:2", "refresh"]
        XCTAssertEqual(TVSecondaryFocusFallback.target(opener: "delete:1", previous: previous, surviving: ["recording:1", "recording:2", "refresh"]), "recording:2")
        XCTAssertEqual(TVSecondaryFocusFallback.target(opener: "delete:2", previous: previous, surviving: ["recording:1"]), "recording:1")
        XCTAssertEqual(TVSecondaryFocusFallback.target(opener: "gone", previous: previous, surviving: ["refresh"]), "refresh")
        XCTAssertNil(TVSecondaryFocusFallback.target(opener: "gone", previous: previous, surviving: []))
    }
    func testSurvivingOpenerRetainsIdentityAcrossReorder() {
        XCTAssertEqual(TVSecondaryFocusFallback.target(opener: "grant:2", previous: ["grant:1", "grant:2", "refresh"], surviving: ["refresh", "grant:2", "grant:1"]), "grant:2")
    }
    func testTVHelpHasBoundedCategoryPathIndependentOfSettingsContent() {
        XCTAssertEqual(TVSettingsCategory.allCases.first, .playback)
        XCTAssertEqual(TVSettingsCategory.allCases.firstIndex(of: .help), 1)
        XCTAssertEqual(Set(TVSettingsCategory.allCases.map(\.id)).count, TVSettingsCategory.allCases.count)
    }
    func testManualRecordingChannelSearchUsesDisplayedNameAndNumber() {
        let news = LiveTvChannel(id: "internal-news", guideNumber: "12.1", guideName: "Evening News", favorite: false, drm: false, support: "ready", hd: true, videoCodec: nil, audioCodec: nil)
        let film = LiveTvChannel(id: "internal-film", guideNumber: "27", guideName: "Film", favorite: false, drm: false, support: "ready", hd: true, videoCodec: nil, audioCodec: nil)
        XCTAssertEqual(TVRecordingChannelSearch.matches([news, film], query: " NEWS ").map(\.id), [news.id])
        XCTAssertEqual(TVRecordingChannelSearch.matches([news, film], query: "12.").map(\.id), [news.id])
        XCTAssertTrue(TVRecordingChannelSearch.matches([news, film], query: "internal-film").isEmpty)
        XCTAssertEqual(TVRecordingChannelSearch.matches([news, film], query: " ").map(\.id), [news.id, film.id])
    }
}
