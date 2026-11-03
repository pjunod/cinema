#if os(tvOS)
import XCTest
@testable import plurx

@MainActor
private final class TimelineRepeatClock {
    var waits: [CheckedContinuation<Void, Error>] = []
    func wait() async throws {
        try await withCheckedThrowingContinuation { waits.append($0) }
    }
    func tick() { if !waits.isEmpty { waits.removeFirst().resume() } }
    func drain() { while !waits.isEmpty { tick() } }
}

@MainActor
final class PlayerTimelineHoldTests: XCTestCase {
    private func settle() async { for _ in 0..<20 { await Task.yield() } }

    func testHoldKeepsPreviewingAndReleaseFencesLateTicks() async {
        let clock = TimelineRepeatClock()
        let hold = PlayerTimelineHoldRepeater(wait: clock.wait)
        var inputs: [PlayerContractInput] = []
        hold.begin(.right) { inputs.append($0); return true }
        await settle()
        for _ in 0..<6 { clock.tick(); await settle() }
        XCTAssertEqual(inputs, Array(repeating: .right, count: 7))
        hold.cancel() // press-up or cancellation
        clock.tick() // a late clock wake cannot move the preview after release
        await settle()
        XCTAssertEqual(inputs.count, 7)
        XCTAssertNil(hold.input)
        clock.drain()
    }

    func testDirectionChangeRetiresPreviousHold() async {
        let clock = TimelineRepeatClock()
        let hold = PlayerTimelineHoldRepeater(wait: clock.wait)
        var inputs: [PlayerContractInput] = []
        hold.begin(.right) { inputs.append($0); return true }
        await settle()
        hold.begin(.left) { inputs.append($0); return true }
        await settle()
        clock.tick() // cancelled right timer
        await settle()
        clock.tick() // current left timer
        await settle()
        XCTAssertEqual(inputs, [.right, .left, .left])
        hold.cancel()
        clock.drain()
        await settle()
    }

    func testLosingTimelineOwnershipStopsHoldWithoutCommit() async {
        let clock = TimelineRepeatClock()
        let hold = PlayerTimelineHoldRepeater(wait: clock.wait)
        var ownsTimeline = true
        var previews = 0
        hold.begin(.right) { _ in
            guard ownsTimeline else { return false }
            previews += 1
            return true
        }
        await settle()
        ownsTimeline = false // focus, menu, stream owner or scene changed
        clock.tick()
        await settle()
        XCTAssertEqual(previews, 1)
        XCTAssertNil(hold.input)
        hold.begin(.select) { _ in XCTFail("Select must never acquire a repeat owner"); return true }
        XCTAssertNil(hold.input)
        clock.drain()
    }
}
#endif
