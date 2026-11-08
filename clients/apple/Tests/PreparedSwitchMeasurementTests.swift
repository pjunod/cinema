import XCTest
@testable import plurx

/// M3's arithmetic, which is the half of measuring a switch that a device run
/// cannot check. A hardware run can tell you the number the ledger printed;
/// only this can tell you the number was the right one for the window it named.
///
/// Each case pins one decision: the window edges, the two-sided sum across two
/// access logs, the counter reset, and the "not measured" that must never be
/// rendered as a zero. The pull request names the mutation each one catches.
final class PreparedSwitchMeasurementTests: XCTestCase {
    private typealias M = PreparedSwitchMeasurement

    private func sample(_ atMs: Int, _ count: Int) -> M.Sample {
        M.Sample(atMs: atMs, count: count)
    }

    func testTheTwoHalvesOfTheWindowAreAddedRatherThanCompared() {
        // The predecessor item dropped two frames in its last two seconds and
        // the successor one in its first two. A prepared switch is both, and
        // each item keeps its own log.
        let measured = M.counterDelta(
            before: [sample(8_000, 5), sample(10_000, 7)],
            after: [sample(10_000, 0), sample(12_000, 1)],
            commitAtMs: 10_000
        )
        XCTAssertEqual(measured.count, 3)
        XCTAssertEqual(measured.beforeMs, 2_000)
        XCTAssertEqual(measured.afterMs, 2_000)
        XCTAssertEqual(measured.spanMs, 4_000)
        XCTAssertEqual(
            M.framesRow(measured),
            "3 dropped · ±2.0 s · 4.0 s of 4.0 s sampled"
        )
    }

    func testDropsOutsideTheWindowAreNotTheSwitchs() {
        // Without the window filter the predecessor's whole session — forty
        // dropped frames from a bad stretch minutes ago — is attributed to the
        // switch, and no switch could ever pass.
        let measured = M.counterDelta(
            before: [sample(1_000, 0), sample(7_999, 40), sample(8_500, 41), sample(10_000, 41)],
            after: [],
            commitAtMs: 10_000
        )
        XCTAssertEqual(measured.count, 0)
        XCTAssertEqual(measured.beforeMs, 1_500, "coverage is what was sampled, not the window")
    }

    func testBothEdgesOfTheWindowAreInside() {
        let measured = M.counterDelta(
            before: [sample(8_000, 0), sample(10_000, 2)],
            after: [sample(10_000, 0), sample(12_000, 3)],
            commitAtMs: 10_000
        )
        XCTAssertEqual(measured.count, 5)
        let outside = M.counterDelta(
            before: [sample(7_999, 0), sample(10_000, 2)],
            after: [],
            commitAtMs: 10_000
        )
        XCTAssertNil(outside.count, "and a sample one millisecond earlier is outside it")
    }

    func testACounterThatWentBackwardsWasResetAndNeverGoesNegative() {
        let measured = M.counterDelta(
            before: [],
            after: [sample(10_100, 900), sample(11_000, 4)],
            commitAtMs: 10_000
        )
        XCTAssertEqual(measured.count, 4)
    }

    func testOneReadingPerSideIsNotAMeasurementAndIsNotAZero() {
        let measured = M.counterDelta(before: [sample(9_000, 3)], after: [], commitAtMs: 10_000)
        XCTAssertNil(measured.count)
        XCTAssertEqual(M.framesRow(measured), "Not measured")
        XCTAssertEqual(M.framesRow(nil), "Not measured")
        XCTAssertEqual(M.audioRow(nil), "Not measured")
    }

    func testTheStallRowSaysStallsAndCountsThemTheSameWay() {
        let measured = M.counterDelta(
            before: [sample(8_000, 4), sample(10_000, 4)],
            after: [sample(10_000, 0), sample(12_000, 1)],
            commitAtMs: 10_000
        )
        XCTAssertEqual(
            M.audioRow(measured),
            "1 access-log stall · ±2.0 s · 4.0 s of 4.0 s sampled"
        )
    }

    func testTapToNewQualityRefusesTwoClocksThatDisagree() {
        XCTAssertEqual(M.visibleInMs(tappedAtUnixMs: 1_000, firstFrameUnixMs: 1_412), 412)
        XCTAssertNil(M.visibleInMs(tappedAtUnixMs: 1_000, firstFrameUnixMs: 999))
        XCTAssertNil(M.visibleInMs(tappedAtUnixMs: nil, firstFrameUnixMs: 1_412))
        XCTAssertNil(M.visibleInMs(tappedAtUnixMs: 1_000, firstFrameUnixMs: nil))
        XCTAssertEqual(M.visibleRow(412), "412 ms")
        XCTAssertEqual(M.visibleRow(nil), "Not measured")
    }

    func testTheRecorderPutsReadingsOnTheSideTheItemChangeDecides() {
        // `replaceCurrentItem` is the instant the item changes, so it is the
        // instant that decides which log a reading belongs to. A recorder that
        // sorted by clock alone would put a pre-swap reading taken at the same
        // millisecond on the successor's side.
        var recorder = M.Recorder()
        recorder.note(tappedAtUnixMs: 1_000)
        recorder.note(atMs: 8_000, droppedFrames: 5, stalls: 1)
        recorder.note(atMs: 10_000, droppedFrames: 7, stalls: 1)
        XCTAssertEqual(recorder.reading(), .unmeasured, "no commit, nothing to say")
        recorder.note(commitAtMs: 10_000)
        recorder.note(atMs: 10_000, droppedFrames: 0, stalls: 0)
        recorder.note(atMs: 12_000, droppedFrames: 1, stalls: 0)
        recorder.note(firstFrameUnixMs: 1_412)
        let reading = recorder.reading()
        XCTAssertEqual(reading.frames, "3 dropped · ±2.0 s · 4.0 s of 4.0 s sampled")
        XCTAssertEqual(reading.audio, "0 access-log stalls · ±2.0 s · 4.0 s of 4.0 s sampled")
        XCTAssertEqual(reading.visibleIn, "412 ms")
    }

    func testASecondChangeIsNotMeasuredAgainstTheFirstsSamples() {
        var recorder = M.Recorder()
        recorder.note(atMs: 0, droppedFrames: 0, stalls: 0)
        recorder.note(commitAtMs: 1_000)
        recorder.note(atMs: 1_000, droppedFrames: 0, stalls: 0)
        recorder.note(atMs: 2_000, droppedFrames: 9, stalls: 0)
        XCTAssertEqual(recorder.reading().frames, "9 dropped · ±2.0 s · 1.0 s of 4.0 s sampled")
        // The viewer taps again. The nine frames the last switch dropped belong
        // to the incumbent's history now, not to the switch about to happen.
        recorder.reopened()
        XCTAssertNil(recorder.commitAtMs)
        XCTAssertEqual(recorder.reading(), .unmeasured)
        recorder.note(atMs: 20_000, droppedFrames: 9, stalls: 0)
        recorder.note(commitAtMs: 20_000)
        recorder.note(atMs: 20_000, droppedFrames: 0, stalls: 0)
        recorder.note(atMs: 21_000, droppedFrames: 0, stalls: 0)
        XCTAssertEqual(recorder.reading().frames, "0 dropped · ±2.0 s · 1.0 s of 4.0 s sampled")
    }

    func testTheSampleRingIsBounded() {
        var recorder = M.Recorder()
        for index in 0..<(M.samplesMax * 10) {
            recorder.note(atMs: index * 10, droppedFrames: index, stalls: 0)
        }
        recorder.note(commitAtMs: M.samplesMax * 100)
        XCTAssertFalse(recorder.reading().frames.isEmpty, "still answers from the retained tail")
    }

    func testAMissingCounterIsNotAZero() {
        var recorder = M.Recorder()
        recorder.note(atMs: 0, droppedFrames: nil, stalls: nil)
        recorder.note(atMs: 1_000, droppedFrames: nil, stalls: nil)
        recorder.note(commitAtMs: 1_000)
        let reading = recorder.reading()
        XCTAssertEqual(reading.frames, "Not measured")
        XCTAssertEqual(reading.audio, "Not measured")
    }

    func testTheAudioRequirementsAreAdvisoryAndSayWhy() {
        let requirements = M.audioRequirements(accessLogAvailable: true)
        XCTAssertEqual(requirements.count, 2)
        XCTAssertTrue(requirements[0].met)
        XCTAssertFalse(
            requirements[1].met,
            "an MTAudioProcessingTap sits in the audible path, so it is not shipped"
        )
        for requirement in requirements {
            XCTAssertGreaterThan(requirement.detail.count, 40, requirement.title)
        }
        // And nothing about them changes what the instrument reports.
        XCTAssertEqual(
            M.audioRow(M.counterDelta(
                before: [M.Sample(atMs: 0, count: 0), M.Sample(atMs: 1, count: 0)],
                after: [],
                commitAtMs: 1
            )),
            "0 access-log stalls · ±2.0 s · 0.0 s of 4.0 s sampled"
        )
    }

    // MARK: the seam report (D1)

    func testSeamEvidenceIsCompleteOnlyForAFullySampledWindow() {
        var recorder = M.Recorder()
        recorder.note(atMs: 0, droppedFrames: 4, stalls: 1)
        recorder.note(atMs: 2_000, droppedFrames: 4, stalls: 1)
        recorder.note(commitAtMs: 2_000)
        recorder.note(atMs: 2_000, droppedFrames: 0, stalls: 0)
        XCTAssertFalse(recorder.seamEvidence().framesComplete, "the successor side is not sampled yet")
        recorder.note(atMs: 4_000, droppedFrames: 0, stalls: 0)
        let evidence = recorder.seamEvidence()
        XCTAssertTrue(evidence.framesComplete)
        XCTAssertEqual(evidence.droppedFrames, 0)
        XCTAssertTrue(evidence.audioObserved, "the stall counter was sampled across the window and did not move")
        var stalled = M.Recorder()
        stalled.note(atMs: 0, droppedFrames: 0, stalls: 0)
        stalled.note(atMs: 2_000, droppedFrames: 0, stalls: 1)
        stalled.note(commitAtMs: 2_000)
        stalled.note(atMs: 2_000, droppedFrames: 0, stalls: 0)
        stalled.note(atMs: 4_000, droppedFrames: 0, stalls: 0)
        XCTAssertFalse(stalled.seamEvidence().audioObserved, "a stall at the seam is not continuity")
        XCTAssertFalse(M.Recorder().seamEvidence().framesComplete)
    }

    func testSeamBeaconNamesEachOutcomeAndCountsOnlyACommit() throws {
        let clean = PreparedSeamEvidence(framesComplete: true, droppedFrames: 0, audioObserved: true)
        let committed = try XCTUnwrap(PreparedSeamBeacon.seam(.committed(firstFrameUnixMs: 1), evidence: clean,
                                                              reopened: false))
        XCTAssertEqual(committed.platform, "apple")
        XCTAssertEqual(committed.outcome, "committed")
        XCTAssertTrue(committed.frames_complete && committed.audio_observed && !committed.reopened)
        // Review 3 B2: the reopen leg is what ran, never implied by the outcome.
        let reopenedCommit = try XCTUnwrap(PreparedSeamBeacon.seam(.committed(firstFrameUnixMs: 1),
                                                                   evidence: clean, reopened: true))
        XCTAssertTrue(reopenedCommit.reopened)
        let refused = try XCTUnwrap(PreparedSeamBeacon.seam(.refused, evidence: clean, reopened: false))
        XCTAssertEqual(refused.outcome, "refused")
        XCTAssertTrue(!refused.reopened && !refused.frames_complete && !refused.audio_observed,
                      "a refusal whose reopen never ran is not a reopen")
        XCTAssertTrue(try XCTUnwrap(PreparedSeamBeacon.seam(.refused, evidence: nil, reopened: true)).reopened)
        let blank = try XCTUnwrap(PreparedSeamBeacon.seam(.switchedWithoutAFrame, evidence: clean, reopened: true))
        XCTAssertEqual(blank.outcome, "switched_without_a_frame")
        XCTAssertNil(PreparedSeamBeacon.seam(.failedWithoutReopen, evidence: clean, reopened: false))
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(PreparedSeamBeacon(
            message: "m", detail: "committed", session_id: "s", prepared_seam: committed))) as? [String: Any]
        XCTAssertEqual(json?["event"] as? String, "prepared_seam")
        XCTAssertEqual((json?["prepared_seam"] as? [String: Any])?["outcome"] as? String, "committed")
    }
}
