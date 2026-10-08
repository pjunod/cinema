import XCTest
import AVFoundation
@testable import plurx

final class AppleFrameEvidenceTests: XCTestCase {
    private let ready = LocalVideoEvidence(bindingMatches: true, targetVisible: true, readyForDisplay: true)
    private let unready = LocalVideoEvidence(bindingMatches: true, targetVisible: true, readyForDisplay: false)

    private func output(_ evidence: LocalVideoEvidence, video: Bool? = true, local: Bool = true) -> PlaybackFirstOutputEvidence {
        .observed(hasVideoSource: video, localTargetExpected: local, evidence: evidence)
    }

    func testVideoTTFFRejectsClockAndDimensionsWithoutLayerReadiness() {
        var measurement = ApplePlaybackTTFFState()
        measurement.opened(at: 0, observedAt: 10)
        // Known 2160p dimensions are deliberately absent from the evidence API.
        for position in stride(from: 0, through: 10_000, by: 1_000) {
            XCTAssertNil(measurement.observe(positionMs: position, playing: true,
                evidence: output(unready), observedAt: 20))
        }
        XCTAssertNil(measurement.observe(positionMs: 10_000, playing: true,
            evidence: output(ready, video: nil), observedAt: 20))
    }

    func testVideoTTFFCompletesOnceForCurrentVisibleReadyLayer() {
        var measurement = ApplePlaybackTTFFState()
        measurement.opened(at: 90_000, observedAt: 10)
        // First observation of readiness is enough even while paused at start.
        XCTAssertEqual(measurement.observe(positionMs: 90_000, playing: false,
            evidence: output(ready), observedAt: 12), 2_000)
        XCTAssertNil(measurement.observe(positionMs: 91_000, playing: true,
            evidence: output(ready), observedAt: 13))
        XCTAssertEqual(output(ready).label, "video-frame-ready")
    }

    func testAudioTTFFUsesProgressWithoutVideoLayer() {
        var measurement = ApplePlaybackTTFFState()
        measurement.opened(at: 0, observedAt: 10)
        let audio = output(.unavailable, video: false, local: false)
        XCTAssertNil(measurement.observe(positionMs: 249, playing: true, evidence: audio, observedAt: 11))
        XCTAssertNil(measurement.observe(positionMs: 250, playing: false, evidence: audio, observedAt: 11))
        XCTAssertEqual(measurement.observe(positionMs: 250, playing: true, evidence: audio, observedAt: 12), 2_000)
        XCTAssertEqual(audio.label, "audio-progress")
    }

    func testPendingSeekDefersFirstFrameReadiness() {
        var measurement = ApplePlaybackTTFFState()
        measurement.opened(at: 90_000, observedAt: 10)
        measurement.rebasePosition(at: 10_000)
        XCTAssertNil(measurement.observe(positionMs: 10_000, playing: true,
            evidence: output(ready), seekPending: true, observedAt: 11))
        XCTAssertEqual(measurement.observe(positionMs: 10_000, playing: true,
            evidence: output(ready), observedAt: 12), 2_000)
    }

    func testLocalFrameEvidenceRejectsHiddenMismatchedAndExternalTargets() {
        for evidence in [LocalVideoEvidence.unavailable,
                         LocalVideoEvidence(bindingMatches: false, targetVisible: true, readyForDisplay: true),
                         LocalVideoEvidence(bindingMatches: true, targetVisible: false, readyForDisplay: true)] {
            XCTAssertFalse(evidence.frameReady)
            XCTAssertEqual(output(evidence).label, "unobserved")
        }
        XCTAssertEqual(output(ready, local: false).label, "unobserved", "PiP/AirPlay cannot certify local TTFF")
    }

    @MainActor
    func testFrameEvidenceRejectsWrongPlayerItemAndStagedLayer() {
        let surface = PlayerSurfaceView()
        let first = AVPlayerItem(url: URL(fileURLWithPath: "/unused-first.mp4"))
        let second = AVPlayerItem(url: URL(fileURLWithPath: "/unused-second.mp4"))
        let player = AVPlayer(playerItem: first)
        let staged = AVPlayer(playerItem: second)
        surface.playerLayer.player = player
        surface.stage(staged)
        XCTAssertTrue(surface.videoEvidence(for: player, item: first).bindingMatches)
        XCTAssertFalse(surface.videoEvidence(for: player, item: first).readyForDisplay)
        XCTAssertFalse(surface.videoEvidence(for: player, item: first).targetVisible, "no attached window")
        XCTAssertFalse(surface.videoEvidence(for: player, item: second).bindingMatches)
        XCTAssertFalse(surface.videoEvidence(for: staged, item: second).bindingMatches)
    }

    @MainActor
    private func observe(_ watchdog: inout BlackFrameWatchdog, item: AVPlayerItem, position: Int,
                         frameReady: Bool = false, local: Bool = true) -> Bool {
        watchdog.observe(item: item, positionMs: position, frameReady: frameReady,
                         localTargetExpected: local, hasVideoSource: true, playing: true)
    }

    @MainActor
    private func fire(_ watchdog: inout BlackFrameWatchdog, item: AVPlayerItem, start: Int = 0) {
        for position in stride(from: start, to: start + 6_000, by: 1_000) {
            XCTAssertFalse(observe(&watchdog, item: item, position: position))
        }
        XCTAssertTrue(observe(&watchdog, item: item, position: start + 6_000))
    }

    @MainActor
    func testBlackFrameWatchdogRearmsForIngressReplacement() {
        let old = AVPlayerItem(url: URL(fileURLWithPath: "/unused-first.mp4"))
        let next = AVPlayerItem(url: URL(fileURLWithPath: "/unused-second.mp4"))
        var watchdog = BlackFrameWatchdog()
        XCTAssertFalse(observe(&watchdog, item: old, position: 10_000, frameReady: true))
        XCTAssertTrue(watchdog.presentedVideo)
        fire(&watchdog, item: next, start: 100_000)
        XCTAssertFalse(observe(&watchdog, item: next, position: 107_000))
        XCTAssertFalse(watchdog.presentedVideo)
    }

    @MainActor
    func testBlackFrameWatchdogIgnoresHiddenAndExternalTargets() {
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        var watchdog = BlackFrameWatchdog()
        for position in stride(from: 0, through: 5_000, by: 1_000) {
            XCTAssertFalse(observe(&watchdog, item: item, position: position))
        }
        XCTAssertFalse(observe(&watchdog, item: item, position: 6_000, local: false))
        XCTAssertEqual(watchdog.blackMs, 0)
        fire(&watchdog, item: item, start: 100_000)
    }

    @MainActor
    func testQueuedBlackFrameRecoveryRechecksEligibilityAndRearms() {
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        var watchdog = BlackFrameWatchdog()
        fire(&watchdog, item: item)
        XCTAssertFalse(watchdog.recoveryIsEligible(for: item, frameReady: false, eligible: false))
        XCTAssertFalse(watchdog.fired)
        XCTAssertNil(watchdog.lastPositionMs)
        fire(&watchdog, item: item, start: 100_000)
        XCTAssertTrue(watchdog.recoveryIsEligible(for: item, frameReady: false, eligible: true))
    }

    @MainActor
    func testLateReadyFrameCancelsQueuedBlackFrameRecovery() {
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        var watchdog = BlackFrameWatchdog()
        fire(&watchdog, item: item)
        XCTAssertFalse(watchdog.recoveryIsEligible(for: item, frameReady: true, eligible: true))
        XCTAssertTrue(watchdog.presentedVideo)
        XCTAssertFalse(observe(&watchdog, item: item, position: 7_000))
    }

    @MainActor
    func testPromotionAndRollbackUseCurrentItemEvidence() {
        let incumbent = AVPlayerItem(url: URL(fileURLWithPath: "/unused-first.mp4"))
        let successor = AVPlayerItem(url: URL(fileURLWithPath: "/unused-second.mp4"))
        var watchdog = BlackFrameWatchdog()
        fire(&watchdog, item: incumbent)
        XCTAssertFalse(observe(&watchdog, item: successor, position: 50_000, frameReady: true))
        XCTAssertTrue(watchdog.presentedVideo, "warm readiness need not transition from false")
        XCTAssertFalse(watchdog.recoveryIsEligible(for: incumbent, frameReady: false, eligible: true))
        fire(&watchdog, item: incumbent, start: 80_000)
        XCTAssertFalse(watchdog.recoveryIsEligible(for: successor, frameReady: false, eligible: true))
    }

    @MainActor
    func testBlackFrameWatchdogDoesNotCountAttachmentOrSeekDiscontinuity() {
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        var watchdog = BlackFrameWatchdog()
        XCTAssertFalse(observe(&watchdog, item: item, position: 0))
        XCTAssertFalse(observe(&watchdog, item: item, position: 1_000))
        XCTAssertFalse(observe(&watchdog, item: item, position: 90_000))
        XCTAssertEqual(watchdog.blackMs, 0)
        watchdog.bind(nil)
        XCTAssertNil(watchdog.lastPositionMs)
        fire(&watchdog, item: item, start: 100_000)
    }

    @MainActor
    func testWatchdogDoesNotRetainRetiredItem() {
        var watchdog = BlackFrameWatchdog()
        var item: AVPlayerItem? = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        weak var retired = item
        watchdog.bind(item)
        item = nil
        XCTAssertNil(retired)
        watchdog.bind(nil)
        XCTAssertEqual(watchdog.blackMs, 0)
    }

    @MainActor
    func testOldSurfaceDetachDoesNotClearReplacementSurface() {
        let controller = PlayerController()
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        controller.player.replaceCurrentItem(with: item)
        let old = PlayerSurfaceView()
        let current = PlayerSurfaceView()
        old.playerLayer.player = controller.player
        current.playerLayer.player = controller.player
        controller.attachPlaybackSurface(old, attached: true)
        controller.attachPlaybackSurface(current, attached: true)
        controller.attachPlaybackSurface(old, attached: false)
        XCTAssertTrue(controller.currentLocalVideoEvidence().bindingMatches)
        controller.attachPlaybackSurface(current, attached: false)
        XCTAssertEqual(controller.currentLocalVideoEvidence(), .unavailable)
    }

    #if DEBUG
    @MainActor
    func testControllerQueuedRecoveryRechecksCurrentSnapshotAndRearms() {
        let controller = PlayerController()
        let item = AVPlayerItem(url: URL(fileURLWithPath: "/unused.mp4"))
        controller.player.replaceCurrentItem(with: item)
        var watchdog = BlackFrameWatchdog()
        fire(&watchdog, item: item)
        controller.setBlackFrameWatchdogForTesting(watchdog)
        controller.localVideoEvidenceForTesting = { _, _ in .unavailable }
        XCTAssertFalse(controller.blackFrameRecoveryIsEligible(for: item))
        watchdog = controller.blackFrameWatchdogForTesting
        XCTAssertFalse(watchdog.fired)
        fire(&watchdog, item: item, start: 100_000)
        controller.setBlackFrameWatchdogForTesting(watchdog)
        controller.localVideoEvidenceForTesting = { _, _ in
            LocalVideoEvidence(bindingMatches: true, targetVisible: true, readyForDisplay: true)
        }
        XCTAssertFalse(controller.blackFrameRecoveryIsEligible(for: item))
        XCTAssertTrue(controller.blackFrameWatchdogForTesting.presentedVideo)
    }
    #endif
}
