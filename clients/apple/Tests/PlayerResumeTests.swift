import AVFoundation
import XCTest
@testable import plurx

private final class ResumeItem: AVPlayerItem {
    var ready = true
    var ranges: [ClosedRange<Double>] = [29.661...91.999]

    override var status: AVPlayerItem.Status { ready ? .readyToPlay : .unknown }
    override func currentTime() -> CMTime {
        CMTime(seconds: 29.661, preferredTimescale: 1_000)
    }
    override var loadedTimeRanges: [NSValue] {
        ranges.map {
            NSValue(timeRange: CMTimeRange(
                start: CMTime(seconds: $0.lowerBound, preferredTimescale: 1_000),
                duration: CMTime(
                    seconds: $0.upperBound - $0.lowerBound,
                    preferredTimescale: 1_000
                )
            ))
        }
    }
}

private final class ResumePlayer: AVPlayer {
    let item = ResumeItem(url: URL(fileURLWithPath: "/resume-test"))
    var commands: [String] = []
    var observedRate: Float = 1
    var observedStatus: AVPlayer.TimeControlStatus = .playing

    override var currentItem: AVPlayerItem? { item }
    override var timeControlStatus: AVPlayer.TimeControlStatus { observedStatus }
    override var rate: Float {
        get { observedRate }
        set { observedRate = newValue }
    }
    override func pause() {
        commands.append("pause")
        observedRate = 0
        observedStatus = .paused
    }
    override func play() {
        commands.append("play")
        observedRate = 1
        observedStatus = .waitingToPlayAtSpecifiedRate
    }
    override func playImmediately(atRate rate: Float) {
        commands.append("immediate")
        observedRate = rate
        observedStatus = .playing
    }
}

@MainActor
final class PlayerResumeTests: XCTestCase {
    private func attempt(
        fastDeadline: TimeInterval? = 11,
        expiresAt: TimeInterval = 25,
        baseline: Double? = 29.661
    ) -> PlaybackResumeAttempt {
        PlaybackResumeAttempt(
            lifecycleGeneration: 3,
            viewerActionEpoch: 7,
            attachmentGeneration: 11,
            itemIdentity: ObjectIdentifier(ResumeItem()),
            targetMs: 29_661,
            startedAt: 10,
            fastPathDeadline: fastDeadline,
            expiresAt: expiresAt,
            pauseDurationMs: 300_000,
            initialRunwaySeconds: 62.338,
            initialTimeControlStatus: "paused",
            initialWaitingReason: nil,
            selectedPath: "buffered-immediate",
            lastVideoDisplaySeconds: baseline
        )
    }

    func testHeartstopperRunwayQualifiesAndBoundaryCasesDoNot() {
        XCTAssertTrue(PlaybackResumeAttempt.bufferedFastPathQualifies(
            ready: true,
            runwaySeconds: 62.338,
            intendedRate: 1,
            hasPendingDestination: false,
            replacementInFlight: false
        ))
        XCTAssertFalse(PlaybackResumeAttempt.bufferedFastPathQualifies(
            ready: true,
            runwaySeconds: 10,
            intendedRate: 1,
            hasPendingDestination: false,
            replacementInFlight: false
        ))
        XCTAssertFalse(PlaybackResumeAttempt.bufferedFastPathQualifies(
            ready: true,
            runwaySeconds: 15,
            intendedRate: 2,
            hasPendingDestination: false,
            replacementInFlight: false
        ))
        let invalidRunways: [Double?] = [nil, .nan, -1]
        for runway in invalidRunways {
            XCTAssertFalse(PlaybackResumeAttempt.bufferedFastPathQualifies(
                ready: true,
                runwaySeconds: runway,
                intendedRate: 1,
                hasPendingDestination: false,
                replacementInFlight: false
            ))
        }
        XCTAssertFalse(PlaybackResumeAttempt.bufferedFastPathQualifies(
            ready: true,
            runwaySeconds: 62.338,
            intendedRate: 1,
            hasPendingDestination: true,
            replacementInFlight: false
        ))
    }

    func testPausedFrameAndPlayingStatusCannotSettleVideoResume() {
        var attempt = attempt()
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.661, at: 10.1, waiting: false),
            .none
        )
        XCTAssertFalse(attempt.fastPathExpired(at: 10.999))
        XCTAssertTrue(attempt.fastPathExpired(at: 11))
    }

    func testVideoNeedsTwoFreshSamplesAndContinuingMotion() {
        var attempt = attempt()
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.701, at: 10.1, waiting: false),
            .firstPicture(milliseconds: 100)
        )
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.741, at: 10.2, waiting: false),
            .none
        )
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.781, at: 10.36, waiting: false),
            .settled(firstPictureMs: 100, settledMs: 360)
        )
    }

    func testOneFreshFrameFollowedByWaitingDoesNotSettle() {
        var attempt = attempt()
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.701, at: 10.1, waiting: false),
            .firstPicture(milliseconds: 100)
        )
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.741, at: 10.4, waiting: true),
            .none
        )
        XCTAssertEqual(
            attempt.observeVideo(displaySeconds: 29.781, at: 10.7, waiting: false),
            .firstPicture(milliseconds: 700)
        )
    }

    func testRepairAdmissionIsOneShotAndSuccessorKeepsAbsoluteDeadline() {
        var attempt = attempt()
        XCTAssertEqual(attempt.admitRepair(at: 11), .admitted)
        XCTAssertEqual(attempt.admitRepair(at: 11.1), .alreadyAdmitted)
        let successor = attempt.boundToSuccessor(
            attachmentGeneration: 12,
            itemIdentity: ObjectIdentifier(ResumeItem()),
            baselineVideoDisplaySeconds: nil
        )
        XCTAssertEqual(successor.id, attempt.id)
        XCTAssertEqual(successor.startedAt, 10)
        XCTAssertEqual(successor.expiresAt, 25)
        XCTAssertNil(successor.fastPathDeadline)
        XCTAssertTrue(successor.repairAdmitted)
        XCTAssertNil(successor.firstPictureAt)
    }

    func testExpiredAttemptCannotAdmitARepair() {
        var attempt = attempt(expiresAt: 25)
        XCTAssertEqual(attempt.admitRepair(at: 25), .expired)
        XCTAssertTrue(attempt.totalExpired(at: 25))
    }

    func testPlaybackCommandSpyUsesImmediatePlayAndPreservesRate() {
        let player = ResumePlayer()
        PlayerController.applyPlaybackCommand(
            to: player,
            preferredRate: 1.5,
            immediately: true
        )
        XCTAssertEqual(player.commands, ["immediate"])
        XCTAssertEqual(player.rate, 1.5)
        XCTAssertTrue(player.automaticallyWaitsToMinimizeStalling)
    }

    func testExplicitPlaybackRequestsAreIdempotentAndColdStartupIsUnchanged() {
        let player = ResumePlayer()
        player.observedRate = 1.5
        let controller = PlayerController(player: player)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(true)
        controller.setPlaybackRequested(true)
        XCTAssertEqual(player.commands, ["pause", "play"])
        XCTAssertEqual(player.rate, 1.5)
        XCTAssertTrue(controller.wantsPlayback)
    }

    func testUnreadyOrInsufficientItemUsesOrdinaryPlaybackOutsideEstablishedResume() {
        let player = ResumePlayer()
        player.item.ready = false
        let controller = PlayerController(player: player)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(true)
        XCTAssertEqual(player.commands, ["pause", "play"])
    }

    func testPendingSeekNeverStartsThePredecessor() {
        let player = ResumePlayer()
        let controller = PlayerController(player: player)
        controller.setPlaybackRequested(false)
        controller.seek(toMs: 90_000)
        controller.setPlaybackRequested(true)
        XCTAssertEqual(player.commands, ["pause"])
        XCTAssertTrue(controller.wantsPlayback)
        controller.stop()
    }

    func testDeveloperEnablementIsAdvisoryAndNeverDisablesTheToggle() throws {
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvDeveloperView.swift"),
            encoding: .utf8
        )
        let section = try XCTUnwrap(source.range(
            of: "Section(\"Bounded pause/resume · advisory enablement\")"
        ))
        let tail = String(source[section.lowerBound...].prefix(1_800))
        XCTAssertTrue(tail.contains("Toggle(\"Enable bounded pause/resume\""))
        XCTAssertTrue(tail.contains("never disables or overrides the switch"))
        XCTAssertFalse(tail.contains(".disabled("))
    }
}
