import Foundation
import XCTest
@testable import plurx

final class ChannelSubjectWireTests: XCTestCase {
    func testProgrammeSelectionOnlyTunesTheCurrentAiring() {
        let programme = LibraryChannelProgramme(source: "local", channelId: "channel", generationId: "generation", cycle: 0,
            ordinal: 0, startsAtMs: 1_000, endsAtMs: 2_000, itemId: 1, fileId: 1, title: "Programme")
        XCTAssertFalse(programme.isOnNow(atMs: 999), "Future selections open details rather than tuning a different airing")
        XCTAssertTrue(programme.isOnNow(atMs: 1_000))
        XCTAssertTrue(programme.isOnNow(atMs: 1_999))
        XCTAssertFalse(programme.isOnNow(atMs: 2_000), "An ended selection must not tune the following programme")
    }

    func testSharedSubjectFixtureDecodesAndClearingEncodesNull() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "channel-subject-wire", withExtension: "json"))
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        let cases = try XCTUnwrap(fixture["cases"] as? [[String: Any]])
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        for item in cases {
            let data = try JSONSerialization.data(withJSONObject: try XCTUnwrap(item["recipe"]))
            var recipe = try decoder.decode(LibraryChannelRecipe.self, from: data)
            if item["name"] as? String == "set" { XCTAssertEqual(recipe.subject, "Stand-up performances") }
            recipe.subject = nil
            let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(recipe)) as? [String: Any])
            XCTAssertTrue(wire["subject"] is NSNull, "clearing must be explicit even for an originally absent field")
        }
    }
}
