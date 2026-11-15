import XCTest
@testable import plurx

final class CustomTagsTests: XCTestCase {
    private let json = """
    {"layout":[{"key":"row:12","kind":"row","row_id":12,"future":1},
               {"key":"continue_watching","kind":"builtin","row_id":null},
               {"key":"libraries","kind":"builtin","row_id":null},
               {"key":"mystery_shelf","kind":"builtin","row_id":null},
               {"key":"row:99","kind":"row","row_id":99},
               {"key":"recently_added","kind":"builtin","row_id":null}],
     "rows":{"12":{"title":"Gay-themed movies & shows","total":38,"extra":true,
                   "items":[{"id":5,"kind":"movie","title":"A","unknown_field":1}]}},
     "version":3}
    """

    func testHomeRowsDecodeIgnoresUnknownKeysAndMapsRows() throws {
        let rows = try HomeRows.decode(Data(json.utf8))
        XCTAssertEqual(rows.layout.count, 6)
        XCTAssertEqual(rows.layout[0].rowId, 12)
        XCTAssertEqual(rows.rows["12"]?.title, "Gay-themed movies & shows")
        XCTAssertEqual(rows.rows["12"]?.total, 38)
        XCTAssertEqual(rows.rows["12"]?.items.map(\.id), [5])
        // Row 12 maps; row 99 has no body and the unknown/undrawn keys drop out.
        XCTAssertEqual(HomeSectionOrder.sections(rows: rows), [.row(12), .continueWatching, .recentlyAdded])
    }

    func testHiddenBuiltInsAreAbsent() throws {
        let rows = try HomeRows.decode(Data(#"{"layout":[{"key":"favorites","kind":"builtin"}],"rows":{}}"#.utf8))
        let sections = HomeSectionOrder.sections(rows: rows)
        XCTAssertEqual(sections, [.favorites])
        XCTAssertFalse(sections.contains(.continueWatching))
        XCTAssertFalse(sections.contains(.comingSoon))
    }

    func testMissingOrFailedEndpointKeepsDefaultOrder() {
        XCTAssertEqual(HomeSectionOrder.sections(rows: nil), HomeSectionOrder.standard)
        XCTAssertEqual(HomeSectionOrder.standard.first, .continueWatching)
        XCTAssertThrowsError(try HomeRows.decode(Data("not json".utf8)))
    }

    func testTagDecodeAndColour() throws {
        let data = Data(#"{"tags":[{"id":3,"name":"Mine","scope":"private","color":"teal","suggest_terms":["x"],"archived":false,"item_count":2,"editable":true,"new":1}]}"#.utf8)
        let d = JSONDecoder(); d.keyDecodingStrategy = .convertFromSnakeCase
        let tag = try d.decode(TagListResponse.self, from: data).tags[0]
        XCTAssertTrue(tag.isPrivate); XCTAssertEqual(tag.rgb, 0x2dd4bf); XCTAssertEqual(tag.itemCount, 2)
        XCTAssertEqual(PlurxTag(id: 1, name: "x", color: "nope").rgb, 0x94a3b8)
    }

    func testPutBodyCarriesOnlyEditableTickedIds() throws {
        let tags = [
            PlurxTag(id: 1, name: "a", editable: true),
            PlurxTag(id: 2, name: "household-locked", editable: false),
            PlurxTag(id: 3, name: "archived", archived: true, editable: true),
            PlurxTag(id: 4, name: "d", scope: "private", editable: true),
        ]
        let ids = TagSelection.putIds(tags: tags, ticked: [1, 2, 3, 4])
        XCTAssertEqual(ids, [1, 3, 4], "an archived tag still on the title is kept")
        let body = try JSONSerialization.jsonObject(with: PlurxAPI.setTagsBody(ids)) as? [String: [Int]]
        XCTAssertEqual(body, ["tag_ids": [1, 3, 4]])
        // The catalogue failed to load: the title's own editable tags still count.
        let current = [PlurxTag(id: 7, name: "on title", editable: true)]
        XCTAssertEqual(TagSelection.putIds(tags: [], ticked: [7, 8], current: current), [7])
        XCTAssertEqual(TagSelection.choosable(tags).map(\.id), [1, 4])
    }
}
