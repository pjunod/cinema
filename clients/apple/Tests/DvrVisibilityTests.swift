import Foundation
import XCTest
@testable import plurx

final class DvrVisibilityTests: XCTestCase {
    func testRecordingsUseThePagedServerEnvelope() throws {
        let data = Data(#"{"rows":[],"next":"older-page"}"#.utf8)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let page = try decoder.decode(DvrRecordingsPage.self, from: data)
        XCTAssertTrue(page.rows.isEmpty)
        XCTAssertEqual(page.next, "older-page")
        XCTAssertThrowsError(try decoder.decode(DvrRecordingsPage.self, from: Data(#"[]"#.utf8)))
    }
}

