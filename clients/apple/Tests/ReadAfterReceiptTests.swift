import XCTest
@testable import plurx

final class ReadAfterReceiptTests: XCTestCase {
    func testFullUnsignedIndexExpiresOnMonotonicTime() throws {
        var time: TimeInterval = 10
        let state = ReadAfterReceipt(now: { time })
        let current = { UInt64(1) }
        let request = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("18446744073709551615", mutating: true, request: request, currentGeneration: current)
        time = 69.999
        XCTAssertEqual(state.request(generation: 1, currentGeneration: current)?.index, "18446744073709551615")
        time = 70
        XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index)
    }

    func testLowerReplyCannotLowerOrRefreshFloor() throws {
        var time: TimeInterval = 0
        let state = ReadAfterReceipt(now: { time }), current = { UInt64(1) }
        let first = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("19", mutating: false, request: first, currentGeneration: current)
        time = 59
        state.observe("18", mutating: false, request: first, currentGeneration: current)
        XCTAssertEqual(state.request(generation: 1, currentGeneration: current)?.index, "19")
        time = 60
        XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index)
    }

    func testUnknownWriteRetiresInFlightKnownReplies() throws {
        let state = ReadAfterReceipt(), current = { UInt64(1) }
        let old = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("19", mutating: false, request: old, currentGeneration: current)
        state.observe("unknown", mutating: true, request: old, currentGeneration: current)
        state.observe("20", mutating: false, request: old, currentGeneration: current)
        XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index)
        let fresh = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("21", mutating: false, request: fresh, currentGeneration: current)
        XCTAssertEqual(state.request(generation: 1, currentGeneration: current)?.index, "21")
        // Even an older unknown write can have committed after the fresh reply.
        state.observe(nil, mutating: true, request: old, currentGeneration: current)
        XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index)
    }

    func testTransportFailureOnlyRetiresMutatingRequests() throws {
        let state = ReadAfterReceipt(), current = { UInt64(1) }
        let request = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("19", mutating: false, request: request, currentGeneration: current)
        state.transportFailed(mutating: false, request: request, currentGeneration: current)
        state.observe(nil, mutating: false, request: request, currentGeneration: current)
        XCTAssertEqual(state.request(generation: 1, currentGeneration: current)?.index, "19")
        state.transportFailed(mutating: true, request: request, currentGeneration: current)
        state.observe("20", mutating: false, request: request, currentGeneration: current)
        XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index)
    }

    func testAuthChangeFencesOldRequestsAndResponses() throws {
        var generation: UInt64 = 1
        let state = ReadAfterReceipt(), current = { generation }
        let old = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
        state.observe("19", mutating: false, request: old, currentGeneration: current)
        generation = 2
        XCTAssertNil(state.request(generation: 1, currentGeneration: current))
        let fresh = try XCTUnwrap(state.request(generation: 2, currentGeneration: current))
        XCTAssertNil(fresh.index)
        state.observe("20", mutating: false, request: fresh, currentGeneration: current)
        state.observe("99", mutating: false, request: old, currentGeneration: current)
        state.transportFailed(mutating: true, request: old, currentGeneration: current)
        XCTAssertEqual(state.request(generation: 2, currentGeneration: current)?.index, "20")
    }

    func testMalformedIndicesCannotBecomeFloors() throws {
        let state = ReadAfterReceipt(), current = { UInt64(1) }
        for malformed in ["0", "01", "-1", "+1", "18446744073709551616", "12, 13", " 12", "12\n", ""] {
            let request = try XCTUnwrap(state.request(generation: 1, currentGeneration: current))
            state.observe("19", mutating: false, request: request, currentGeneration: current)
            state.observe(malformed, mutating: false, request: request, currentGeneration: current)
            XCTAssertNil(state.request(generation: 1, currentGeneration: current)?.index, malformed)
        }
    }
}
