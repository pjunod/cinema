import Foundation
import XCTest
@testable import plurx

final class LibraryGroupsTests: XCTestCase {
    private func item(_ id: Int, _ fields: [String: Any] = [:]) throws -> Item {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Item.self, from: JSONSerialization.data(withJSONObject:
            ["id": id, "kind": "movie", "title": "Title"].merging(fields) { _, new in new }))
    }

    func testTitleRowsUseServerKeysAndKeepEveryItemInPagerOrder() throws {
        let items = try [item(1, ["title": "The Apple"]), item(2, ["title": "Zulu", "sort_title": "aardvark"]),
                         item(3, ["title": "Éclair"]), item(4, ["title": "2001"]), item(5, ["title": "Bee"])]
        let groups = LibraryGroups.make(items, sort: .title)
        XCTAssertEqual(groups.map(\.id), ["#", "A", "B"])
        XCTAssertEqual(groups.map { $0.items.map(\.id) }, [[3, 4], [1, 2], [5]])
        let long = try (1...421).map { try item($0, ["sort_title": "alpha"]) }
        XCTAssertEqual(LibraryGroups.make(long, sort: .title).first?.items.count, 421)
    }

    func testMetadataGroupsKeepUnknownsAndDescendingYears() throws {
        let items = try [item(1, ["year": 2025, "recorded_at": "2024-02-01", "resolution": 1080]),
                         item(2, ["year": 2026, "recorded_at": "2026-01-01", "resolution": 2160]), item(3)]
        XCTAssertEqual(LibraryGroups.make(items, sort: .year).map(\.id), ["year-2026", "year-2025", "unknown"])
        XCTAssertEqual(LibraryGroups.make(items, sort: .recorded).map(\.id), ["year-2026", "year-2024", "unknown"])
        XCTAssertEqual(LibraryGroups.make(items, sort: .resolution).map(\.label), ["4K", "1080p", "Unknown resolution"])
    }

    func testAddedPeriodsUseLocalCalendarBoundariesAndKeepMissingDates() throws {
        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = TimeZone(identifier: "America/New_York")!
        let formatter = ISO8601DateFormatter()
        let now = formatter.date(from: "2026-03-10T16:00:00Z")!
        let timestamps = ["2026-03-11T00:00:00Z", "2026-03-10T05:00:00Z", "2026-03-04T05:00:00Z", "2026-03-02T12:00:00Z", "2026-02-12T12:00:00Z"]
        var items = try timestamps.enumerated().map { index, text in
            try item(index, ["added_at": Int(formatter.date(from: text)!.timeIntervalSince1970)])
        }
        items.append(try item(99))
        XCTAssertEqual(LibraryGroups.make(items, sort: .added, now: now, calendar: calendar).map(\.id),
                       ["future", "today", "week", "month", "added-2026-2", "unknown"])
    }
}
