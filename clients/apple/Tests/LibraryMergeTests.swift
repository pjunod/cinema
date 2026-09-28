import Foundation
import XCTest
@testable import plurx

final class LibraryMergeTests: XCTestCase {
    func testSharedServerOrderAcrossOneTwoAndThreePagedLibraries() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "library-sort-cases", withExtension: "json"))
        let fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        let rows = try XCTUnwrap(fixture["items"] as? [[String: Any]])
        let expected = try XCTUnwrap(fixture["expected_order"] as? [String: [Int]])
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let items = try rows.map { row -> Item in
            var wire = row
            let index = try XCTUnwrap(row["index"] as? Int)
            wire["id"] = index
            wire["kind"] = row["kind"] ?? "movie"
            wire["added_at"] = index
            let data = try JSONSerialization.data(withJSONObject: wire)
            return try decoder.decode(Item.self, from: data)
        }
        for sort in LibrarySort.allCases {
            let order = try XCTUnwrap(expected[sort.rawValue])
            for count in 1...3 {
                let cursors = (0..<count).map { cursor in
                    items.filter { ($0.id - 1) % count == cursor }
                        .sorted { LibraryMerge.precedes($0, $1, sort: sort) }
                }
                var merge = LibraryMerge(libraryIds: Array(0..<count), sort: sort)
                var requests = 0
                while let request = merge.nextRequest {
                    let page = Array(cursors[request.libraryId].dropFirst(request.offset).prefix(7))
                    merge.receive(libraryId: request.libraryId, items: page, total: cursors[request.libraryId].count, limit: 7)
                    requests += 1
                    XCTAssertLessThan(requests, 50)
                }
                XCTAssertTrue(merge.complete)
                XCTAssertEqual(merge.decided.map(\.id), order, "\(sort) across \(count) cursors")
            }
        }
    }

    func testFirstPaintNeedsOnePagePerLibrary() throws {
        let items = try (1...600).map { index -> Item in
            let wire: [String: Any] = ["id": index, "kind": "movie", "title": "Title \(index)", "sort_title": String(format: "title %04d", index)]
            let decoder = JSONDecoder()
            decoder.keyDecodingStrategy = .convertFromSnakeCase
            return try decoder.decode(Item.self, from: JSONSerialization.data(withJSONObject: wire))
        }
        let libraries = (0..<3).map { part in items.filter { ($0.id - 1) % 3 == part } }
        var merge = LibraryMerge(libraryIds: [0, 1, 2], sort: .title)
        var counts = [0, 0, 0]
        while merge.decided.count < 40, let request = merge.nextRequest {
            let page = Array(libraries[request.libraryId].dropFirst(request.offset).prefix(200))
            counts[request.libraryId] += 1
            merge.receive(libraryId: request.libraryId, items: page, total: libraries[request.libraryId].count)
        }
        XCTAssertEqual(counts, [1, 1, 1])
        XCTAssertGreaterThanOrEqual(merge.decided.count, 40)
    }
}
