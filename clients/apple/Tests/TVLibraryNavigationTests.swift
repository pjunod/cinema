import XCTest
@testable import plurx

@MainActor
final class TVLibraryNavigationTests: XCTestCase {
    func testSynopsisRegionsKeepAllWordsAndMakeTheLastPartReachable() {
        let text = (0..<300).map { "Word\($0)" }.joined(separator: " ")
        let regions = TVReadableInformation.readingRegions(text)
        XCTAssertGreaterThan(regions.count, 1)
        XCTAssertTrue(regions.allSatisfy { $0.count <= 360 })
        XCTAssertEqual(regions.joined(separator: " "), text)
        XCTAssertTrue(regions.last?.hasSuffix("Word299") == true)
        XCTAssertEqual(TVReadableInformation.readingRegions("First paragraph.\n\nSecond paragraph."),
                       ["First paragraph.", "Second paragraph."])
        let longToken = String(repeating: "x", count: 1_000)
        let tokenRegions = TVReadableInformation.readingRegions(longToken)
        XCTAssertTrue(tokenRegions.allSatisfy { $0.count <= 360 })
        XCTAssertEqual(tokenRegions.joined(), longToken)
    }

    func testEpisodeShelfRealizesBothPlayAndDetailsReturnKeys() {
        XCTAssertEqual(MediaRow.realizedItemID(key: "detail:children:item:416", shelf: "detail:children"), 416)
        XCTAssertEqual(MediaRow.realizedItemID(key: "detail:children:details:416", shelf: "detail:children"), 416)
        XCTAssertNil(MediaRow.realizedItemID(key: "other:details:416", shelf: "detail:children"))
    }

    func testRefreshKeepsIdentityThenNextPreviousAndRecoveryFallback() {
        let previous = [10, 20, 30, 40]
        XCTAssertEqual(TVLibraryNavigation.survivingBookmark(20, previous: previous, current: [40, 20, 10]), 20)
        XCTAssertEqual(TVLibraryNavigation.survivingBookmark(20, previous: previous, current: [10, 40]), 40)
        XCTAssertEqual(TVLibraryNavigation.survivingBookmark(40, previous: previous, current: [10, 20]), 20)
        XCTAssertEqual(TVLibraryNavigation.survivingBookmark(20, previous: previous, current: [90]), 90)
        XCTAssertNil(TVLibraryNavigation.survivingBookmark(20, previous: previous, current: []))
    }
}
