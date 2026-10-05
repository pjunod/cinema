import AVFoundation
import XCTest
@testable import plurx

/// The screensaver came on over Live TV on Apple TV, in the guide and in
/// fullscreen (2026-10-04): no stack owned display wake, and AVPlayer's own
/// implicit assertion does not survive the picture moving between surfaces.
@MainActor
final class DisplayWakeTests: XCTestCase {
    private func owner() -> (DisplayWakeOwner, DisplayWakeRecorder) {
        let recorder = DisplayWakeRecorder()
        return (DisplayWakeOwner { recorder.applied.append($0) }, recorder)
    }

    private func waitUntil(_ condition: @escaping @MainActor () -> Bool) async {
        let done = expectation(description: "display wake settled")
        let observer = Task { @MainActor in
            while !Task.isCancelled {
                if condition() { done.fulfill(); return }
                await Task.yield()
            }
        }
        await fulfillment(of: [done], timeout: 3)
        observer.cancel()
        await observer.value
    }

    func testOnlyVideoThatIsPlayingOrWaitingToPlayHoldsTheDisplay() {
        XCTAssertTrue(PlaybackDisplayWake.holds(status: .playing, hasVideo: true, external: false))
        XCTAssertTrue(PlaybackDisplayWake.holds(status: .waitingToPlayAtSpecifiedRate,
                                                hasVideo: true, external: false),
                      "a viewer waiting on a buffer is still watching")
        XCTAssertFalse(PlaybackDisplayWake.holds(status: .paused, hasVideo: true, external: false))
        XCTAssertFalse(PlaybackDisplayWake.holds(status: .playing, hasVideo: false, external: false),
                       "an audiobook never holds the display awake")
        XCTAssertFalse(PlaybackDisplayWake.holds(status: .playing, hasVideo: true, external: true),
                       "a picture on an AirPlay receiver leaves this screen free to lock")
    }

    func testTheTimerStaysDisabledWhileAnyStackHolds() {
        let (owner, recorder) = owner()
        let live = UUID(), film = UUID()
        owner.set(live, holding: true)
        owner.set(film, holding: true)
        owner.set(live, holding: false)
        XCTAssertTrue(owner.isHeld, "the finite player still holds after Live TV lets go")
        owner.set(film, holding: false)
        XCTAssertFalse(owner.isHeld)
        XCTAssertEqual(recorder.applied, [true, true, true, false],
                       "every change is written through, so a flag UIKit reset is put back")
    }

    func testTheClaimFollowsItsPlayersTransportState() async {
        let (owner, recorder) = owner()
        let player = AVPlayer()
        let wake = PlaybackDisplayWake(owner: owner)
        wake.track(player, hasVideo: true)
        XCTAssertFalse(owner.isHeld, "a paused player holds nothing")

        player.play()
        await waitUntil { owner.isHeld }
        wake.track(player, hasVideo: true)
        XCTAssertTrue(owner.isHeld, "re-tracking the same player keeps its claim")

        player.pause()
        await waitUntil { !owner.isHeld }
        player.play()
        await waitUntil { owner.isHeld }
        wake.release()
        XCTAssertFalse(owner.isHeld)
        XCTAssertEqual(recorder.applied.last, false)
    }

    func testANewPlayerReplacesTheTrackedOne() async {
        let (owner, _) = owner()
        let incumbent = AVPlayer()
        let successor = AVPlayer()
        let wake = PlaybackDisplayWake(owner: owner)
        incumbent.play()
        wake.track(incumbent, hasVideo: true)
        await waitUntil { owner.isHeld }
        wake.track(successor, hasVideo: true)
        XCTAssertFalse(owner.isHeld, "a prepared commit hands the claim to the adopted player")
        incumbent.pause()
        successor.play()
        await waitUntil { owner.isHeld }
        successor.pause()
        await waitUntil { !owner.isHeld }
    }
}

/// Records what the display-wake owner applied, in order.
final class DisplayWakeRecorder {
    var applied: [Bool] = []
}
