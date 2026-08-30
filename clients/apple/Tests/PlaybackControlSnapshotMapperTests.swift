import Foundation
import XCTest
@testable import plurx

private func selection() -> ClientSelection {
    ClientSelection(
        quality: .auto,
        audioTrack: 0,
        subtitle: SubtitleSelection(mode: .off, track: nil),
        audioOffsetMs: 0,
        codec: .auto,
        dynamicRange: .auto
    )
}

private func capabilities() -> DynamicCapabilities {
    DynamicCapabilities(
        platform: "apple",
        maxHeight: 2_160,
        codecs: [.hevc, .h264],
        dynamicRanges: [.dolbyVision, .hdr10, .sdr],
        dualPlayerPreparation: false
    )
}

/// A player mid-title, playing, with ten seconds of runway. Every test states
/// only what it changes.
private func playing(
    position: Int = 60_000,
    duration: Int = 3_600_000
) -> PlayerControlObservation {
    PlayerControlObservation(
        positionMs: position,
        durationMs: duration,
        bufferedFromMs: position - 5_000,
        bufferedThroughMs: position + 10_000,
        rate: 1,
        isPaused: false,
        isEnded: false,
        isSeeking: false,
        hasStarted: true,
        waitingForMs: nil,
        isLikelyToKeepUp: true,
        errorCode: nil,
        errorDetail: nil,
        droppedFrames: nil,
        observedDownloadBps: nil,
        observationOverride: nil,
        renderOverride: nil,
        selection: selection(),
        capabilities: capabilities()
    )
}

final class PlaybackControlSnapshotMapperTests: XCTestCase {
    private func map(_ observation: PlayerControlObservation) -> PlaybackControlSnapshot {
        PlaybackControlMapping.snapshot(from: observation)
    }

    // MARK: the ordinary states

    func testAPlayingPlayerIsActiveAndRendering() {
        let snapshot = map(playing())
        XCTAssertEqual(snapshot.demand, .active)
        XCTAssertEqual(snapshot.renderState, .rendering)
        XCTAssertEqual(snapshot.positionMs, 60_000)
        XCTAssertEqual(snapshot.bufferedThroughMs, 70_000)
        XCTAssertEqual(snapshot.bufferedFromMs, 55_000)
        XCTAssertNil(snapshot.seekTargetMs)
        XCTAssertTrue(snapshot.isValid)
    }

    func testAPausedPlayerHoldsRatherThanEnding() {
        var observation = playing()
        observation.isPaused = true
        observation.rate = 0
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.demand, .hold, "pause is a hold, not a departure")
        XCTAssertEqual(snapshot.playbackRate, 0)
        XCTAssertTrue(snapshot.isValid)
    }

    func testABeforeFirstFrameStateIsStartingHoweverBusyItLooks() {
        var observation = playing()
        observation.hasStarted = false
        observation.isLikelyToKeepUp = false
        XCTAssertEqual(map(observation).renderState, .starting)
    }

    func testASeekOutranksEveryOtherBusyState() {
        var observation = playing()
        observation.isSeeking = true
        observation.isLikelyToKeepUp = false
        observation.waitingForMs = 20_000
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.renderState, .seeking)
        XCTAssertEqual(
            snapshot.seekTargetMs, snapshot.positionMs,
            "the target is the playhead the viewer asked for"
        )
    }

    func testOnlyASeekReportsATarget() {
        for observation in [playing(), pausedObservation(), startingObservation()] {
            XCTAssertNil(map(observation).seekTargetMs)
        }
    }

    private func pausedObservation() -> PlayerControlObservation {
        var value = playing()
        value.isPaused = true
        return value
    }

    private func startingObservation() -> PlayerControlObservation {
        var value = playing()
        value.hasStarted = false
        return value
    }

    // MARK: waiting versus stalled

    func testAShortWaitIsWaitingAndALongOneIsStalled() {
        var observation = playing()
        observation.waitingForMs = PlaybackControlMapping.persistentStallMs - 1
        XCTAssertEqual(map(observation).renderState, .waiting)
        observation.waitingForMs = PlaybackControlMapping.persistentStallMs
        XCTAssertEqual(map(observation).renderState, .stalled, "the boundary is inclusive")
    }

    func testAPlayerThatCannotKeepUpIsWaitingEvenWithoutAReportedWait() {
        var observation = playing()
        observation.isLikelyToKeepUp = false
        XCTAssertEqual(map(observation).renderState, .waiting)
    }

    func testAPausedPlayerThatCannotKeepUpIsNotWaiting() {
        var observation = playing()
        observation.isPaused = true
        observation.isLikelyToKeepUp = false
        XCTAssertEqual(
            map(observation).renderState, .rendering,
            "a paused player is not waiting for anything"
        )
    }

    func testAStalledPlayerReportsAStarvedDecoder() {
        var observation = playing()
        observation.waitingForMs = 30_000
        XCTAssertEqual(map(observation).observation?.decoderState, .starved)
    }

    // MARK: ending, and not ending

    func testAStreamEndingAtTheTitleEndIsTerminal() {
        var observation = playing(position: 3_600_000, duration: 3_600_000)
        observation.isEnded = true
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.demand, .end)
        XCTAssertEqual(snapshot.renderState, .ended)
    }

    func testAStreamEndingWithinTheSlackIsStillTerminal() {
        var observation = playing(
            position: 3_600_000 - PlaybackControlMapping.endedSlackMs,
            duration: 3_600_000
        )
        observation.isEnded = true
        XCTAssertEqual(map(observation).demand, .end)
    }

    func testATruncatedStreamKeepsDemandingAndReportsFailed() {
        var observation = playing(position: 1_200_000, duration: 3_600_000)
        observation.isEnded = true
        let snapshot = map(observation)
        XCTAssertEqual(
            snapshot.demand, .active,
            "the bytes ran out early; the client reopens rather than leaving"
        )
        XCTAssertEqual(snapshot.renderState, .failed)
    }

    func testAGrowingStreamWithNoKnownDurationEndsTerminally() {
        var observation = playing(position: 90_000, duration: 0)
        observation.isEnded = true
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.demand, .end)
        XCTAssertEqual(snapshot.renderState, .ended)
    }

    func testAnErrorOutranksAnEnd() {
        var observation = playing(position: 3_600_000, duration: 3_600_000)
        observation.isEnded = true
        observation.errorCode = .decoder
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.renderState, .failed)
        XCTAssertEqual(snapshot.observation?.decoderState, .failed)
    }

    // MARK: position and runway

    func testPositionIsClampedToAKnownDuration() {
        let observation = playing(position: 3_700_000, duration: 3_600_000)
        XCTAssertEqual(map(observation).positionMs, 3_600_000)
    }

    func testPositionIsNotClampedWithoutAKnownDuration() {
        let observation = playing(position: 3_700_000, duration: 0)
        XCTAssertEqual(map(observation).positionMs, 3_700_000)
    }

    func testANegativePositionIsFloored() {
        let observation = playing(position: -5, duration: 3_600_000)
        XCTAssertEqual(map(observation).positionMs, 0)
    }

    func testNoContiguousRangeMeansNoRunwayRatherThanAGuess() {
        var observation = playing()
        observation.bufferedFromMs = nil
        observation.bufferedThroughMs = nil
        let snapshot = map(observation)
        XCTAssertNil(snapshot.bufferedFromMs)
        XCTAssertEqual(
            snapshot.bufferedThroughMs, snapshot.positionMs,
            "zero runway, which is exactly what an unbuffered playhead has"
        )
        XCTAssertTrue(snapshot.isValid)
    }

    func testARangeEndingBehindThePlayheadStillReportsZeroRunway() {
        var observation = playing()
        observation.bufferedThroughMs = observation.positionMs - 2_000
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.bufferedThroughMs, snapshot.positionMs)
        XCTAssertTrue(snapshot.isValid, "the server rejects a runway behind the playhead")
    }

    func testARangeStartingAheadOfThePlayheadIsPulledBackToIt() {
        var observation = playing()
        observation.bufferedFromMs = observation.positionMs + 1_000
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.bufferedFromMs, snapshot.positionMs)
        XCTAssertTrue(snapshot.isValid)
    }

    // MARK: rate

    func testAnActivePlayerNeverReportsAZeroRate() {
        var observation = playing()
        observation.rate = 0
        XCTAssertEqual(
            map(observation).playbackRate, 1,
            "zero from an active player reads as a hold nobody asked for"
        )
    }

    func testAnActivePlayersRateIsFlooredAtAQuarter() {
        var observation = playing()
        observation.rate = 0.1
        XCTAssertEqual(map(observation).playbackRate, PlaybackControlMapping.minimumActiveRate)
    }

    func testAnAbsurdRateIsCappedInBothDemands() {
        var observation = playing()
        observation.rate = 64
        XCTAssertEqual(map(observation).playbackRate, PlaybackControlMapping.maximumRate)
        observation.isPaused = true
        XCTAssertEqual(map(observation).playbackRate, PlaybackControlMapping.maximumRate)
    }

    func testANonFiniteRateBecomesZeroRatherThanTravellingToTheServer() {
        var observation = playing()
        observation.isPaused = true
        observation.rate = .nan
        let snapshot = map(observation)
        XCTAssertEqual(snapshot.playbackRate, 0)
        XCTAssertTrue(snapshot.isValid)
    }

    // MARK: observation

    func testABeforeStartPlayerReportsAnUnknownDecoder() {
        var observation = playing()
        observation.hasStarted = false
        XCTAssertEqual(map(observation).observation?.decoderState, .unknown)
    }

    func testDroppedFramesTravelAndANegativeCountDoesNot() {
        var observation = playing()
        observation.droppedFrames = 42
        XCTAssertEqual(map(observation).observation?.droppedFrames, 42)
        observation.droppedFrames = -1
        XCTAssertNil(map(observation).observation?.droppedFrames)
    }

    func testDetailOnlyTravelsWithACode() {
        var observation = playing()
        observation.errorDetail = "something went wrong"
        XCTAssertNil(
            map(observation).observation?.errorDetail,
            "the server rejects a detail with no code"
        )
        observation.errorCode = .network
        XCTAssertEqual(map(observation).observation?.errorDetail, "something went wrong")
    }

    func testARecoveryPathsEvidenceOutranksWhatThePlayerCanSee() {
        var observation = playing()
        observation.errorCode = .media
        observation.errorDetail = "media_code_0"
        observation.observationOverride = ClientObservation(
            droppedFrames: nil,
            decoderState: .starved,
            errorCode: .manifest,
            errorDetail: "manifest_load_error"
        )
        let value = map(observation).observation
        XCTAssertEqual(value?.decoderState, .starved)
        XCTAssertEqual(value?.errorCode, .manifest)
        XCTAssertEqual(value?.errorDetail, "manifest_load_error")
    }

    func testAnOverrideThatSaysNothingChangesNothing() {
        var observation = playing()
        observation.droppedFrames = 7
        observation.observationOverride = ClientObservation()
        let value = map(observation).observation
        XCTAssertEqual(value?.decoderState, .ready)
        XCTAssertEqual(value?.droppedFrames, 7)
        XCTAssertNil(value?.errorCode)
    }

    func testARenderOverrideIsHonouredButNeverOverAnErrorOrAnEnd() {
        var observation = playing()
        observation.renderOverride = .stalled
        XCTAssertEqual(map(observation).renderState, .stalled)

        observation.errorCode = .decoder
        XCTAssertEqual(map(observation).renderState, .failed, "an error outranks an override")

        observation.errorCode = nil
        observation.isEnded = true
        observation.positionMs = 3_600_000
        XCTAssertEqual(map(observation).renderState, .ended, "an end outranks an override")
    }

    // MARK: throughput

    func testAThroughputEstimateTravelsAndAZeroDoesNot() {
        var observation = playing()
        observation.observedDownloadBps = 8_000_000
        XCTAssertEqual(map(observation).observedDownloadBps, 8_000_000)
        observation.observedDownloadBps = 0
        XCTAssertNil(map(observation).observedDownloadBps, "zero is not an estimate")
    }

    // MARK: the whole thing stays sendable

    func testEveryMappedStateIsAcceptedBySnapshotValidation() {
        var observations: [PlayerControlObservation] = []
        for paused in [true, false] {
            for ended in [true, false] {
                for seeking in [true, false] {
                    for started in [true, false] {
                        for waiting in [nil, 100, 30_000] as [Int?] {
                            for position in [0, 60_000, 3_700_000] {
                                var value = playing(position: position)
                                value.isPaused = paused
                                value.isEnded = ended
                                value.isSeeking = seeking
                                value.hasStarted = started
                                value.waitingForMs = waiting
                                observations.append(value)
                            }
                        }
                    }
                }
            }
        }
        for observation in observations {
            let snapshot = PlaybackControlMapping.snapshot(from: observation)
            XCTAssertTrue(
                snapshot.isValid,
                "the server would reject \(snapshot.renderState) at \(snapshot.positionMs)"
            )
        }
        XCTAssertEqual(observations.count, 144)
    }
}
