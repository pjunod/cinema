import Foundation
import XCTest
@testable import plurx

final class DvrVisibilityTests: XCTestCase {
    private struct OverviewCases: Decodable {
        let overviews: [OverviewCase]
    }

    private struct OverviewCase: Decodable {
        let `case`: String
        let clientAgeMs: UInt64
        let body: DvrOverview
        let fresh: Bool
        let indicator: String?
    }

    func testRecordingsUseThePagedServerEnvelope() throws {
        let data = Data(#"{"rows":[],"next":"older-page"}"#.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let page = try decoder.decode(DvrRecordingsPage.self, from: data)
        XCTAssertTrue(page.rows.isEmpty)
        XCTAssertEqual(page.next, "older-page")
        XCTAssertThrowsError(try decoder.decode(DvrRecordingsPage.self, from: Data(#"[]"#.utf8)))
    }

    func testEverySharedOverviewUsesRecorderAndClientFreshness() throws {
        let url = try XCTUnwrap(
            Bundle(for: DvrVisibilityTests.self)
                .url(forResource: "dvr-visibility-cases", withExtension: "json")
        )
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let cases = try decoder.decode(OverviewCases.self, from: Data(contentsOf: url)).overviews
        XCTAssertFalse(cases.isEmpty)
        for row in cases {
            XCTAssertEqual(row.body.isFresh(clientAgeMs: row.clientAgeMs), row.fresh, row.case)
            XCTAssertEqual(row.body.indicatorText(clientAgeMs: row.clientAgeMs), row.indicator, row.case)
        }
    }
}
