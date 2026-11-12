import XCTest
@testable import plurx

/// Offload: Curator removed a title's files to free disk. The item stays,
/// flagged, and the client dims it, offers no Play, and lets an administrator
/// ask for it back.
@MainActor
final class OffloadTests: XCTestCase {
    private func decode(_ json: String) throws -> Item {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(Item.self, from: Data(json.utf8))
    }

    func testAnOffloadedLeafDecodesFlaggedAndAnOlderServerDecodesUnflagged() throws {
        let off = try decode(#"{"id":1,"kind":"movie","title":"Dune","offloaded":true}"#)
        XCTAssertTrue(off.isOffloaded)
        XCTAssertTrue(off.hasOffloadedLeaves)
        let plain = try decode(#"{"id":2,"kind":"movie","title":"Heat"}"#)
        XCTAssertFalse(plain.isOffloaded)
        XCTAssertFalse(plain.hasOffloadedLeaves)
    }

    func testAPartlyOffloadedShowCarriesACountNotTheFlag() throws {
        let show = try decode(#"{"id":3,"kind":"show","title":"Severance","rollup":{"leaves":10,"watched":2,"offloaded":4}}"#)
        XCTAssertFalse(show.isOffloaded)
        XCTAssertTrue(show.hasOffloadedLeaves)
        XCTAssertEqual(show.rollup?.offloaded, 4)
        // A rollup from a server without Offload still decodes.
        let old = try decode(#"{"id":4,"kind":"show","title":"Old","rollup":{"leaves":10,"watched":2}}"#)
        XCTAssertFalse(old.hasOffloadedLeaves)
    }

    func testTheLibraryFilterNamesTheServersPresenceValues() {
        XCTAssertEqual(LibraryPresence.allCases.map(\.rawValue), ["disk", "offloaded", "everything"])
        XCTAssertEqual(LibraryPresence.disk.label, "On disk")
    }

    func testTheStatusLineSaysCuratorIsUnreachableOrWhatItAnswered() {
        XCTAssertEqual(OffloadStatusModel.unreachable, "Offloaded · curator unreachable")
        let answer = OffloadAnswer(matched: true, state: "downloading", message: "Curator is downloading it.")
        XCTAssertEqual(OffloadStatusModel.statusLine(answer), "Offloaded · Curator is downloading it.")
    }

    func testATitleThatCameBackOrIsNoLongerOffloadedEndsTheStatusLoop() {
        XCTAssertTrue(OffloadStatusModel.isGone(OffloadAnswer(matched: true, state: "on_disk", message: "")))
        XCTAssertFalse(OffloadStatusModel.isGone(OffloadAnswer(matched: true, state: "downloading", message: "")))
        XCTAssertTrue(OffloadStatusModel.isGone(APIError.conflict(code: "not_offloaded", message: "not offloaded")))
        XCTAssertFalse(OffloadStatusModel.isGone(APIError.transport("down")))
    }
}
