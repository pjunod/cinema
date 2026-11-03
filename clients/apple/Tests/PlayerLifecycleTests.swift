import AVFoundation
import SwiftUI
import XCTest
@testable import plurx

private final class SuspendedPlaybackPlayer: AVPlayer {
    override func currentTime() -> CMTime {
        CMTime(seconds: 42.5, preferredTimescale: 1_000)
    }
}

@MainActor
final class PlayerLifecycleTests: XCTestCase {
    func testBackgroundRetiresTVPlayerBeforeReturningToTitle() async {
        let player = SuspendedPlaybackPlayer()
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/sleeping-film"))
        player.replaceCurrentItem(with: item)
        player.pause()
        let controller = PlayerController(player: player)
        let lifecycle = PlayerLifecycleCoordinator()
        var stoppedAt: Int?
        var dismissals = 0
        let returned = expectation(description: "returned to title")
        #if os(iOS)
        returned.isInverted = true
        #endif

        let cleanup = {
            stoppedAt = controller.positionForPlaybackStop()
            controller.stop()
        }
        let completion = {
            XCTAssertNil(player.currentItem, "the expired item must be gone before title returns")
            XCTAssertEqual(stoppedAt, 42_500, "the title receives the last film position")
            dismissals += 1
            returned.fulfill()
        }

        lifecycle.sceneChanged(to: .background, teardown: cleanup, completion: completion)
        #if os(tvOS)
        XCTAssertTrue(lifecycle.isTearingDown)
        XCTAssertNil(player.currentItem, "release the item synchronously before suspension")
        XCTAssertEqual(stoppedAt, 42_500)
        await fulfillment(of: [returned], timeout: 1)
        // A wake notification, duplicate background delivery and the cover's
        // disappearance must not repeat the progress handoff or dismissal.
        lifecycle.sceneChanged(to: .active, teardown: cleanup, completion: completion)
        lifecycle.sceneChanged(to: .background, teardown: cleanup, completion: completion)
        lifecycle.teardown { XCTFail("sleep cleanup already ran") }
        await Task.yield()
        XCTAssertEqual(dismissals, 1)
        XCTAssertEqual(controller.surface.surface.kind, .none)
        #else
        XCTAssertFalse(lifecycle.isTearingDown)
        XCTAssertTrue(player.currentItem === item, "iOS retains playback for PiP/background audio")
        XCTAssertNil(stoppedAt)
        await fulfillment(of: [returned], timeout: 0.1)
        controller.stop()
        #endif
    }

    #if os(tvOS)
    func testBackgroundReplacesPendingAutoplayWithDismissal() async {
        let lifecycle = PlayerLifecycleCoordinator()
        let dismissed = expectation(description: "dismiss instead of opening next episode")
        var cleanups = 0
        lifecycle.finish(
            teardown: { cleanups += 1 },
            backgroundCompletion: { dismissed.fulfill() },
            completion: { XCTFail("autoplay must not open a new player after backgrounding") }
        )
        // Natural completion already owns teardown and has queued autoplay;
        // suspension arrives before the next-loop completion drain.
        lifecycle.sceneChanged(
            to: .background,
            teardown: { XCTFail("natural completion already released playback") },
            completion: { XCTFail("the existing finish still owns dismissal") }
        )
        await fulfillment(of: [dismissed], timeout: 1)
        XCTAssertEqual(cleanups, 1)
    }
    #endif

    func testInactiveSystemUIKeepsPausedPlaybackAttached() async {
        let player = AVPlayer()
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/paused-film"))
        player.replaceCurrentItem(with: item)
        player.pause()
        let lifecycle = PlayerLifecycleCoordinator()
        for phase in [ScenePhase.inactive, .active] {
            lifecycle.sceneChanged(
                to: phase,
                teardown: { XCTFail("temporary system UI must not close playback") },
                completion: { XCTFail("temporary system UI must not dismiss the player") }
            )
        }
        await Task.yield()
        XCTAssertFalse(lifecycle.isTearingDown)
        XCTAssertTrue(player.currentItem === item)
        player.replaceCurrentItem(with: nil)
    }
}
