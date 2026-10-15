import AVFoundation
import XCTest
@testable import plurx

private final class ResumeItem: AVPlayerItem {
    var ready = true
    var positionSeconds = 29.661
    var ranges: [ClosedRange<Double>] = [29.661...91.999]

    override var status: AVPlayerItem.Status { ready ? .readyToPlay : .unknown }
    override func currentTime() -> CMTime {
        CMTime(seconds: positionSeconds, preferredTimescale: 1_000)
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
private final class ResumeIntentGate {
    private(set) var calls = 0
    private var continuations: [CheckedContinuation<UInt64?, Never>] = []

    func report() async -> UInt64? {
        calls += 1
        return await withCheckedContinuation { continuations.append($0) }
    }

    func releaseNext(_ sequence: UInt64? = 1) {
        guard !continuations.isEmpty else { return }
        continuations.removeFirst().resume(returning: sequence)
    }

    func releaseAll() {
        while !continuations.isEmpty { releaseNext() }
    }
}

@MainActor
final class PlayerResumeTests: XCTestCase {
    private var testsDirectory: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent()
    }

    private func startEstablished(_ controller: PlayerController) {
        SettingsStore().boundedResumeEnabled = true
        controller.start(
            model: AppModel(),
            itemId: 1,
            fileId: 1,
            startMs: 0,
            durationMs: 600_000,
            title: "Resume owner"
        )
        controller.observeAttachmentPlayback(positionMs: 6_000, playing: true)
    }

    private func waitUntil(
        _ description: String,
        condition: @escaping @MainActor () -> Bool
    ) async throws {
        for _ in 0..<200 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(5))
        }
        XCTFail("Timed out waiting for \(description)")
    }

    private func attempt(
        fastDeadline: TimeInterval? = 11,
        expiresAt: TimeInterval = 25,
        baseline: Double? = 29.661
    ) -> PlaybackResumeAttempt {
        PlaybackResumeAttempt(
            lifecycleGeneration: 3,
            viewerActionEpoch: 7,
            attachmentGeneration: 11,
            itemIdentity: ObjectIdentifier(ResumeItem(
                url: URL(fileURLWithPath: "/resume-attempt")
            )),
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

    func testIncumbentFetchPreferenceLeaseRejectsPositiveGapInsideProtectedWindow() throws {
        let player = ResumePlayer()
        player.item.positionSeconds = 0
        player.item.ranges = [0...5, 5.1...40]
        player.item.preferredForwardBufferDuration = 60
        let controller = PlayerController(player: player,
            requestPlaybackDecision: { _, _, _, _ in
                try await Task.sleep(for: .seconds(3_600))
                throw CancellationError()
            })
        let credentials = Session.shared.credentials
        Session.shared.setCredentials(origin: "http://127.0.0.1:1", token: nil)
        defer {
            controller.stop()
            Session.shared.setCredentials(origin: credentials.origin, token: credentials.token)
        }
        controller.start(model: AppModel(), itemId: 1, fileId: 1, startMs: 0,
            durationMs: 600_000, title: "Physical lease runway")
        let sessionId = "7f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01"
        let action = try XCTUnwrap(PreparedReplacementAction(ControlAction(
            type: PlaybackControl.prepareActionType,
            actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
            sessionId: sessionId,
            playlistUrl: "/api/v1/hls/\(sessionId)/master.m3u8",
            mediaOriginMs: 0,
            effectiveSelection: EffectiveSelection(
                qualityAuto: false, height: 720, audioTrack: nil, subtitleBurn: nil,
                audioOffsetMs: 0, codec: "source", dynamicRange: nil))))

        XCTAssertEqual(try XCTUnwrap(controller.bufferedRunwaySeconds()), 40, accuracy: 0.001)
        XCTAssertEqual(try XCTUnwrap(controller.bufferedRunwaySeconds(joinTolerance: 0)), 5, accuracy: 0.001)
        XCTAssertTrue(controller.startPreparedSuccessor(action, filmPositionMs: 0))
        XCTAssertEqual(player.item.preferredForwardBufferDuration, 60,
            "A positive gap inside the protected window must prevent physical buffer reduction")
        controller.discardPreparedSuccessor()

        player.item.ranges = [0...5, 5...40]
        XCTAssertTrue(controller.startPreparedSuccessor(action, filmPositionMs: 0))
        XCTAssertEqual(player.item.preferredForwardBufferDuration, 16)
        controller.discardPreparedSuccessor()
        XCTAssertEqual(player.item.preferredForwardBufferDuration, 60)
        XCTAssertTrue(controller.startPreparedSuccessor(action, filmPositionMs: 0))
        player.item.preferredForwardBufferDuration = 45
        controller.discardPreparedSuccessor()
        XCTAssertEqual(player.item.preferredForwardBufferDuration, 45,
            "Restoration must preserve a later owner's preference")
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
            itemIdentity: ObjectIdentifier(ResumeItem(
                url: URL(fileURLWithPath: "/resume-successor")
            )),
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

    func testEstablishedBufferedResumeUsesTheProductionImmediatePath() async {
        let player = ResumePlayer()
        player.observedRate = 1.5
        let controller = PlayerController(
            player: player,
            requestPlaybackDecision: { _, _, _, _ in
                try await Task.sleep(for: .seconds(3_600))
                throw CancellationError()
            },
            reportPlaybackIntent: { _ in 1 }
        )
        startEstablished(controller)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(true)
        XCTAssertEqual(Array(player.commands.suffix(2)), ["pause", "immediate"])
        XCTAssertEqual(player.rate, 1.5)
        XCTAssertNotNil(controller.resumeOwnershipForTesting)
        controller.stop()
    }

    func testOneSecondAdmissionIsOneShotAndFinalPauseFencesTheRepair() async throws {
        var now: TimeInterval = 10
        let admission = now + PlaybackResumeAttempt.fastPathSeconds
        let player = ResumePlayer()
        let controller = PlayerController(
            player: player,
            requestPlaybackDecision: { _, _, _, _ in
                try await Task.sleep(for: .seconds(3_600))
                throw CancellationError()
            },
            reportPlaybackIntent: { _ in 1 },
            resumeNow: { now },
            waitResumeSample: {
                // The injected clock runs up to the one-second admission and
                // then holds. Advancing it on every sample let the monitor
                // spin through the remaining fourteen virtual seconds to the
                // 15-second root deadline in a few real milliseconds, so
                // whether `waitUntil` (polling every 5 ms) ever saw the
                // admitted repair was a race it lost about half the time.
                // Past the admission the monitor keeps sampling — which is
                // what makes the one-shot claim below mean something — but
                // the root deadline cannot pass until the test acts.
                if now < admission {
                    now += 0.25
                    await Task.yield()
                } else {
                    try? await Task.sleep(for: .milliseconds(1))
                }
            }
        )
        startEstablished(controller)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(true)
        try await waitUntil("the fast-path repair admission") {
            controller.resumeOwnershipForTesting?.repairAdmitted == true
        }
        let ownership = try XCTUnwrap(controller.resumeOwnershipForTesting)
        XCTAssertEqual(ownership.expiresAt, 25, "repair inherits the root deadline")
        // Further samples past the admission admit nothing new: the same
        // attempt, still admitted once.
        try await Task.sleep(for: .milliseconds(20))
        XCTAssertEqual(controller.resumeOwnershipForTesting?.id, ownership.id)
        XCTAssertEqual(controller.resumeOwnershipForTesting?.repairAdmitted, true)
        let generation = controller.openGenerationForTesting
        controller.setPlaybackRequested(false)
        XCTAssertNil(controller.resumeOwnershipForTesting)
        XCTAssertGreaterThan(controller.openGenerationForTesting, generation)
        XCTAssertEqual(player.commands.last, "pause")
        controller.stop()
    }

    func testResumeRepairWaitsForPublicationAndCannotOutliveFinalPause() async throws {
        let gate = ResumeIntentGate()
        let player = ResumePlayer()
        let controller = PlayerController(
            player: player,
            requestPlaybackDecision: { _, _, _, _ in
                try await Task.sleep(for: .seconds(3_600))
                throw CancellationError()
            },
            reportPlaybackIntent: { _ in await gate.report() }
        )
        startEstablished(controller)
        controller.setPlaybackRequested(false)
        try await waitUntil("Pause publication") { gate.calls == 1 }
        gate.releaseNext()
        controller.setPlaybackRequested(true)
        try await waitUntil("Resume publication") { gate.calls == 2 }
        XCTAssertEqual(controller.resumeOwnershipForTesting?.publicationCompleted, false)
        XCTAssertEqual(controller.resumeOwnershipForTesting?.repairAdmitted, false)
        controller.setPlaybackRequested(false)
        try await waitUntil("final Pause publication") { gate.calls == 3 }
        gate.releaseAll()
        await Task.yield()
        XCTAssertNil(controller.resumeOwnershipForTesting)
        XCTAssertEqual(player.commands.last, "pause")
        controller.stop()
    }

    func testOrdinaryWatchdogNudgeUsesImmediatePlayWhenRunwayIsSafe() {
        let player = ResumePlayer()
        player.observedRate = 1.25
        let controller = PlayerController(player: player)
        controller.applyStallRecoveryNudge()
        XCTAssertEqual(player.commands, ["immediate"])
        XCTAssertEqual(player.rate, 1)
        XCTAssertNil(controller.resumeOwnershipForTesting)
    }

    func testUnreadyOrInsufficientItemUsesOrdinaryPlaybackOutsideEstablishedResume() {
        let player = ResumePlayer()
        player.item.ready = false
        let controller = PlayerController(player: player)
        controller.setPlaybackRequested(false)
        controller.setPlaybackRequested(true)
        XCTAssertEqual(player.commands, ["pause", "play"])
    }

    func testPausePublicationRetainsPendingSeekUntilPlayDispatchesOnce() async throws {
        let gate = ResumeIntentGate()
        let player = ResumePlayer()
        let decision = try JSONDecoder().decode(Decision.self, from: Data("""
        {"fileId":1,"method":"direct","playUrl":"/not-fetched",
         "source":{"height":1080,"durationMs":600000},"audio":[],"subtitles":[],"ladder":[]}
        """.utf8))
        let caps = DeviceCaps(client: ClientInfo(kind: "ios", build: "test", ua: "test"),
            video: [], audio: [], containers: [], transports: [], dvTransport: "none",
            display: DisplayCaps(hdr: false, dolbyVision: false))
        var selection = PlayerController.MediaSelectionPreparation()
        selection.audio = { _, _ in nil }
        selection.native = { _, _, _ in .init(hasSubtitleOptions: true, apply: { true }) }
        var nativeSeeks: [Int] = []
        var creates = 0
        let controller = PlayerController(player: player,
            requestPlaybackDecision: { _, _, _, _ in (decision, caps) },
            mediaSelectionPreparation: selection,
            itemPreparation: .init(ready: { _, _ in }, seek: { _, _ in }),
            nativeSeek: { _, target in nativeSeeks.append(target); return true },
            requestHlsSession: { _, _, _ in
                creates += 1
                throw APIError.transport("unexpected create in retained direct seek")
            },
            reportPlaybackIntent: { _ in await gate.report() })
        let model = AppModel()
        let previousQuality = model.playbackQuality
        let previousCredentials = Session.shared.credentials
        model.playbackQuality = .auto
        Session.shared.setCredentials(origin: "http://127.0.0.1:1", token: nil)
        defer {
            controller.stop()
            gate.releaseAll()
            model.playbackQuality = previousQuality
            Session.shared.setCredentials(origin: previousCredentials.origin, token: previousCredentials.token)
        }
        controller.start(model: model, itemId: 1, fileId: 1, startMs: 0,
            durationMs: 600_000, title: "Pause retains destination")
        await controller.loadingTask?.value
        let attachment = controller.openGenerationForTesting
        let quality = controller.selectedHeight
        controller.seek(toMs: 90_000)
        let pending = controller.pendingPlaybackIntentForTesting
        // Pause supersedes the coalesced seek before it can publish or run.
        controller.setPlaybackRequested(false)
        try await waitUntil("Pause publication") { gate.calls == 1 }
        let pausePublication = try XCTUnwrap(controller.pendingTransportTasksForTesting.publication)
        gate.releaseNext(7)
        await pausePublication.value
        await controller.pendingTransportTasksForTesting.seek?.value
        XCTAssertTrue(nativeSeeks.isEmpty, "Pause must not dispatch the retained destination")
        XCTAssertEqual(creates, 0)
        XCTAssertEqual(controller.openGenerationForTesting, attachment)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.targetMs, pending.targetMs)
        XCTAssertEqual(controller.pendingPlaybackIntentForTesting.generation, pending.generation)
        XCTAssertEqual(controller.selectedHeight, quality)
        XCTAssertFalse(controller.wantsPlayback)
        XCTAssertEqual(player.commands.last, "pause")
        let commandsBeforePlay = player.commands.count
        controller.setPlaybackRequested(true)
        try await waitUntil("Play publication") { gate.calls == 2 }
        XCTAssertTrue(nativeSeeks.isEmpty)
        XCTAssertEqual(player.commands.count, commandsBeforePlay,
            "Play must not start the predecessor before its pending seek")
        let playPublication = try XCTUnwrap(controller.pendingTransportTasksForTesting.publication)
        gate.releaseNext(8)
        await playPublication.value
        await controller.pendingTransportTasksForTesting.seek?.value
        XCTAssertEqual(nativeSeeks, [90_000])
        XCTAssertEqual(creates, 0)
        XCTAssertTrue(controller.wantsPlayback)
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

    /// Bounded pause/resume graduated from Developer to Settings → Playback;
    /// its readiness rows stay advisory there.
    func testPlaybackEnablementIsAdvisoryAndNeverDisablesTheToggle() throws {
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/SettingsView.swift"),
            encoding: .utf8
        )
        let section = try XCTUnwrap(source.range(
            of: "Section(\"Bounded pause/resume\")"
        ))
        let tail = String(source[section.lowerBound...].prefix(1_800))
        XCTAssertTrue(tail.contains("Toggle(\"Enable bounded pause/resume\""))
        XCTAssertTrue(tail.contains("never disables or overrides the switch"))
        XCTAssertFalse(tail.contains(".disabled("))
    }
}

/// A rolling session ends 180 s after the viewer pauses (`ROLLING_PAUSE_GRACE`).
/// AVPlayer keeps reloading the live playlist while paused, the reload answers
/// 404/410, and before this the viewer came back to "Playback stopped. resource
/// unavailable" over a player nobody had touched. The sliding-HLS contract
/// (§9.5) wants the client to stay paused and open one replacement at the
/// saved position on resume.
@MainActor
final class PausedRetirementTests: XCTestCase {
    private var sourcesDirectory: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../Sources")
    }

    private func playerSource() throws -> String {
        try String(
            contentsOf: sourcesDirectory.appendingPathComponent("PlayerController.swift"),
            encoding: .utf8
        )
    }

    /// The body of `name`, up to the next declaration at the same indent.
    private func body(of name: String, in source: String) throws -> Substring {
        let start = try XCTUnwrap(source.range(of: name), "\(name) not found")
        let rest = source[start.upperBound...]
        let end = rest.range(of: "\n    private func ")?.lowerBound
            ?? rest.range(of: "\n    func ")?.lowerBound
            ?? rest.endIndex
        return rest[..<end]
    }

    private func parks(
        started: Bool = true,
        wantsPlayback: Bool = false,
        rolling: Bool = true,
        compatibility: Bool = false,
        resume: Bool = false
    ) -> Bool {
        PlayerController.parksPausedItemFailure(
            started: started,
            wantsPlayback: wantsPlayback,
            isRollingSession: rolling,
            isCompatibilityFailure: compatibility,
            resumeInFlight: resume
        )
    }

    func testOnlyAPausedRollingNonCompatibilityFailureParks() {
        XCTAssertTrue(parks())
        XCTAssertFalse(parks(wantsPlayback: true),
                       "a viewer who is watching gets the ordinary ladder and its surface")
        XCTAssertFalse(parks(rolling: false),
                       "VOD and direct play are kept alive while paused; their failure is real")
        XCTAssertFalse(parks(compatibility: true),
                       "a media rejection is a verdict, not a retired session")
        XCTAssertFalse(parks(resume: true), "an explicit resume owns its own repair")
        XCTAssertFalse(parks(started: false))
    }

    func testThePauseGraceRefusalIsRecognisedExactly() {
        XCTAssertTrue(PlayerController.isPauseGraceExpiry("transport:410:pause_grace_expired"))
        XCTAssertFalse(PlayerController.isPauseGraceExpiry("transport:410:media_session_ended"))
        XCTAssertFalse(PlayerController.isPauseGraceExpiry("transport:409:owner_changed"))
        XCTAssertFalse(PlayerController.isPauseGraceExpiry("transport:none:-"))
    }

    func testTheReplacementOpensWhereTheViewerLeftIt() {
        typealias R = PlayerController.PausedRetirement
        let s = "4f9c0a52-0000-4000-8000-000000000001"
        XCTAssertEqual(PlayerController.pausedRetirementReopenPositionMs(
            pendingSeekMs: 90_000, retired: R(sessionId: s, positionMs: 40_000),
            attachedPositionMs: nil, lastObservedMs: 40_000
        ), 90_000, "a seek made while paused wins")
        XCTAssertEqual(PlayerController.pausedRetirementReopenPositionMs(
            pendingSeekMs: nil, retired: R(sessionId: s, positionMs: 40_000),
            attachedPositionMs: 0, lastObservedMs: 12_000
        ), 40_000, "a dead item's clock never replaces the saved position")
        XCTAssertEqual(PlayerController.pausedRetirementReopenPositionMs(
            pendingSeekMs: nil, retired: R(sessionId: s, positionMs: nil),
            attachedPositionMs: 41_500, lastObservedMs: 40_000
        ), 41_500, "a still-attached item's own clock is the most exact")
        XCTAssertEqual(PlayerController.pausedRetirementReopenPositionMs(
            pendingSeekMs: nil, retired: R(sessionId: s, positionMs: nil),
            attachedPositionMs: nil, lastObservedMs: 40_000
        ), 40_000)
    }

    func testAPausedItemFailureParksBeforeFailoverOrAnySurface() throws {
        let source = try playerSource()
        let failure = try body(of: "private func handleItemFailure(", in: source)
        let park = try XCTUnwrap(failure.range(of: "Self.parksPausedItemFailure("))
        for later in ["if let attempt = resumeAttempt", "retryMediaOnNextNode(item)",
                      "stopForBlockingSurface(", "raiseOwnerFault("] {
            let at = try XCTUnwrap(failure.range(of: later), later)
            XCTAssertLessThan(park.lowerBound, at.lowerBound, "\(later) runs before the park")
        }
        XCTAssertTrue(failure.contains("isRollingSession: !isVOD && !isDirectPlayback"))
        XCTAssertTrue(failure.contains("sessionId: retiredSession,"))
    }

    func testResumeConsumesTheLatchBeforeAnyInPlaceResumePath() throws {
        let source = try playerSource()
        // The public `setPlaybackRequested(_:)` only forwards with viewer
        // origin; the body that resumes is its `viewerOrigin:` overload.
        let play = try body(of: "private func setPlaybackRequested(_ requested: Bool, viewerOrigin: Bool) {", in: source)
        let latch = try XCTUnwrap(play.range(
            of: "} else if let retired = currentPausedRetirement, !isChangingStream {"
        ))
        let inPlace = try XCTUnwrap(play.range(of: "beginResumeAttempt("))
        XCTAssertLessThan(latch.lowerBound, inPlace.lowerBound)
        XCTAssertFalse(play.contains("pausedRetirement = nil"),
                       "only the successor's session identity retires the latch")
        XCTAssertTrue(play.contains("await self.reopen(at: position)"))
    }

    /// A create that fails restores the dead item with `sessionId` unchanged,
    /// so the latch must survive `open()` and retire only by identity.
    func testTheLatchRetiresBySessionIdentityNotAtOpen() throws {
        let source = try playerSource()
        let open = try body(of: "    private func open(\n", in: source)
        XCTAssertFalse(open.contains("pausedRetirement = nil"))
        XCTAssertTrue(source.contains(
            "guard let retired = pausedRetirement, retired.sessionId == sessionId else { return nil }"
        ))
        let stop = try body(of: "    func stop(deactivateAudioSession: Bool = true) {", in: source)
        XCTAssertTrue(stop.contains(
            "let position = currentPausedRetirement?.positionMs ?? realPositionMs()"
        ))
        XCTAssertTrue(stop.contains("pausedRetirement = nil"))
    }

    func testTheControlRefusalArmsTheLatchOnlyWhilePaused() throws {
        let source = try playerSource()
        let hook = try XCTUnwrap(source.range(of: "onExchangeFailure: { [weak self] failure in"))
        let tail = source[hook.lowerBound...].prefix(1_100)
        XCTAssertTrue(tail.contains("Self.isPauseGraceExpiry(failure), !self.wantsPlayback"))
        XCTAssertTrue(tail.contains("!self.isChangingStream, self.currentPausedRetirement == nil"))
        XCTAssertTrue(tail.contains("sessionId: retiredSession, positionMs: nil"))
    }
}
