import Foundation
import XCTest
@testable import plurx

/// A2 — the commit's rendezvous: where the successor has to be standing before
/// it is exposed, what bounds the seek that puts it there, and what a seek that
/// never comes back costs.
///
/// The ordering is asserted against the source rather than against
/// AVFoundation, for the reason `RecordingHost` exists at all: the half of the
/// commit that can be driven in a test returns a canned outcome, and the half
/// that cannot is exactly the part whose *order* is the defect. This repository
/// already holds one ordering still that way (see the recipe-revision test in
/// `AppleClientTests`); a mutation that exposes before aligning fails it.
@MainActor
final class PreparedCommitRendezvousTests: XCTestCase {
    func testFirstFrameBudgetSuspendsExplicitPauseAndResumesRemainingTime() {
        var budget = PreparedActiveWallBudget(boundMs: 6_000, nowMs: 100, playbackRequested: true)
        XCTAssertFalse(budget.update(nowMs: 2_100, playbackRequested: false))
        XCTAssertEqual(budget.remainingMs, 4_000)
        XCTAssertFalse(budget.update(nowMs: 62_100, playbackRequested: true))
        XCTAssertEqual(budget.remainingMs, 4_000)
        XCTAssertFalse(budget.update(nowMs: 66_099, playbackRequested: true))
        XCTAssertTrue(budget.update(nowMs: 66_100, playbackRequested: true))
        XCTAssertEqual(budget.remainingMs, 0)
    }

    func testPreparationAndAlignmentConsumeTheSamePhysicalOverlapDeadline() {
        var budget = PreparedActiveWallBudget(boundMs: 6_000, nowMs: 11_100,
            playbackRequested: false, overlapBoundMs: 12_000, overlapStartedAtMs: 100)
        XCTAssertEqual(budget.remainingOverlapMs, 1_000)
        XCTAssertFalse(budget.update(nowMs: 12_099, playbackRequested: false))
        XCTAssertTrue(budget.update(nowMs: 12_100, playbackRequested: false))
        XCTAssertEqual(budget.remainingMs, 6_000)
        let spent = PreparedActiveWallBudget(boundMs: 6_000, nowMs: 20_100,
            playbackRequested: true, overlapBoundMs: 12_000, overlapStartedAtMs: 100)
        XCTAssertEqual(spent.remainingOverlapMs, 0)
    }

    func testFirstFrameBudgetBoundsStallsAndIgnoresBackwardClockSamples() {
        var budget = PreparedActiveWallBudget(boundMs: 6_000, nowMs: 100, playbackRequested: true)
        XCTAssertFalse(budget.update(nowMs: 3_100, playbackRequested: true))
        XCTAssertFalse(budget.update(nowMs: 2_100, playbackRequested: true))
        XCTAssertEqual(budget.remainingMs, 3_000)
        XCTAssertTrue(budget.update(nowMs: 6_100, playbackRequested: true))
        XCTAssertEqual(budget.remainingMs, 0)
        XCTAssertTrue(budget.update(nowMs: 90_000, playbackRequested: false))
    }

    private func playerControllerSource() throws -> String {
        let sources = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent()
            .appendingPathComponent("../Sources", isDirectory: true)
            .standardizedFileURL
        return try String(
            contentsOf: sources.appendingPathComponent("PlayerController.swift"),
            encoding: .utf8
        )
    }

    private func commitBody() throws -> Substring {
        let source = try playerControllerSource()
        let start = try XCTUnwrap(
            source.range(of: "func commitPreparedSuccessor("),
            "the commit has been renamed; this test names it deliberately"
        )
        return source[start.upperBound...]
    }

    func testDecodedAlignmentRejectsStaleInvalidAndEmptySamples() {
        let rendezvous = PreparedCommitRendezvous.plan(
            stagedFilmPositionMs: 1_000, incumbentFilmPositionMs: 10_000, mediaOriginMs: 2_000
        )
        XCTAssertTrue(rendezvous.acceptsDecodedAlignment(displaySeconds: 8, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertTrue(rendezvous.acceptsDecodedAlignment(displaySeconds: 8 + 1 / 24, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: 7.95, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: 8.05, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: 8.2, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: 8, width: 1280, height: 720, frameDurationSeconds: 0))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: .nan, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: .infinity, width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(rendezvous.acceptsDecodedAlignment(displaySeconds: 8, width: 0, height: 720, frameDurationSeconds: 1 / 24))
    }

    // MARK: The rendezvous itself

    func testTheSwitchHappensWhereTheIncumbentGotToNotWhereTheOfferStagedIt() {
        let plan = PreparedCommitRendezvous.plan(
            stagedFilmPositionMs: 30_000,
            incumbentFilmPositionMs: 42_000,
            mediaOriginMs: 30_000
        )
        XCTAssertEqual(plan.filmPositionMs, 42_000)
        XCTAssertEqual(
            plan.itemPositionMs, 12_000,
            "the successor's own zero is its media_origin_ms, not the film's"
        )
    }

    func testLoadedRendezvousCapsTheFuturePointAtActualRunway() throws {
        let plan = try XCTUnwrap(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 386_065, incumbentFilmPositionMs: 385_966,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [386...390.010]))
        XCTAssertEqual(plan.filmPositionMs, 388_010)
        XCTAssertEqual(plan.itemPositionMs, 388_010)
        XCTAssertLessThan(plan.filmPositionMs, 393_866,
            "the maximum lead must not force an exact seek beyond loaded target media")
        XCTAssertNotNil(plan.clockAnchor(incumbentFilmMs: 386_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
    }

    func testStaticOriginalRunwayWaitsForMovingIncumbentFutureCoverage() throws {
        let original = 5_416
        let initiallyLoaded = [4.0...7.989333333333334]
        XCTAssertTrue(PreparedSeekPolicy.originalTargetHasRunway(
            filmPositionMs: original, mediaOriginMs: 0, ranges: initiallyLoaded))
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: original, incumbentFilmPositionMs: 7_944,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 10_579,
            loadedItemRanges: initiallyLoaded),
            "the measured original runway cannot admit the advancing incumbent's future point")
        // Bytes published after another second must cover the then-current
        // clock, rather than admitting the stale earlier readiness sample.
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: original, incumbentFilmPositionMs: 8_944,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 9_579,
            loadedItemRanges: [4...11.5]))
        let expanded = try XCTUnwrap(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: original, incumbentFilmPositionMs: 8_944,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 9_579,
            loadedItemRanges: [4...12]))
        XCTAssertEqual(expanded.filmPositionMs, 10_000)
        XCTAssertEqual(expanded.itemPositionMs, 10_000)
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: original, incumbentFilmPositionMs: 8_944,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_000,
            loadedItemRanges: [4...12]), "new bytes cannot extend the physical overlap reserve")
    }

    func testCadencePreparedBeforeFreezeRejectsConsumedFragmentAndAdmitsFreshFuturePoint() throws {
        let old = PreparedDecodedCadence(frameDurationSeconds: 1 / 24, itemInterval: 314..<316)
        XCTAssertNil(preparedRendezvousWithCadence(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_500,
            loadedItemRanges: [308...320], cadence: old),
            "the inspected old interval cannot prove the next moving target")
        let fresh = PreparedDecodedCadence(frameDurationSeconds: 1 / 24, itemInterval: 316..<318)
        let plan = try XCTUnwrap(preparedRendezvousWithCadence(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_500,
            loadedItemRanges: [308...320], cadence: fresh))
        XCTAssertEqual(plan.filmPositionMs, 317_176)
        XCTAssertTrue(fresh.covers(itemPositionMs: plan.itemPositionMs))
        XCTAssertGreaterThanOrEqual(plan.filmPositionMs - 315_776, 1_000)
        XCTAssertNil(preparedRendezvousWithCadence(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 4_094,
            loadedItemRanges: [308...320], cadence: fresh),
            "cadence preparation never invents the physical reserve lost in the measured failure")
    }

    func testCadenceProbeStaysInsideActualReachableFutureEnvelope() throws {
        let probe = try XCTUnwrap(preparedCadenceProbeItemMs(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_500,
            loadedItemRanges: [308...320]))
        XCTAssertEqual(probe, 317_176)
        XCTAssertTrue((316.0..<318.0).contains(Double(probe) / 1_000))
        XCTAssertFalse((318.0..<320.0).contains(Double(probe) / 1_000),
            "four seconds of speculative lead would inspect an unreachable fragment")
        XCTAssertEqual(preparedCadenceProbeItemMs(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_500,
            loadedItemRanges: [308...318]), 317_176,
            "actual loaded media can prepare cadence before it has two seconds of final runway")
        XCTAssertNil(preparedCadenceProbeItemMs(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_000,
            loadedItemRanges: [308...320]))
        XCTAssertNil(preparedCadenceProbeItemMs(
            stagedFilmPositionMs: 308_088, incumbentFilmPositionMs: 315_776,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_500,
            loadedItemRanges: [308...310, 316...320]),
            "an unrelated loaded island cannot provide the original interval's cadence")
    }

    func testFragmentCadenceCannotAuthorizeTheNextIntervalBoundary() throws {
        let cadence = PreparedDecodedCadence(frameDurationSeconds: 1 / 24, itemInterval: 10..<12)
        let plan = try XCTUnwrap(preparedRendezvousWithCadence(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [10...20], cadence: cadence))
        XCTAssertLessThan(plan.itemPositionMs, 12_000)
        XCTAssertTrue(cadence.covers(itemPositionMs: plan.itemPositionMs))
        XCTAssertNil(preparedRendezvousWithCadence(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 11_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [10...20], cadence: cadence))
    }

    func testLoadedRendezvousMapsOriginAndActualPlaybackRate() throws {
        let plan = try XCTUnwrap(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 610_000, incumbentFilmPositionMs: 610_000,
            mediaOriginMs: 600_000, playbackRate: 2, overlapRemainingMs: 9_000,
            loadedItemRanges: [10...15]))
        XCTAssertEqual(plan.filmPositionMs, 613_000)
        XCTAssertEqual(plan.itemPositionMs, 13_000)
        XCTAssertEqual(Double(plan.filmPositionMs - 610_000) / 2, 1_500)
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 610_000, incumbentFilmPositionMs: 610_000,
            mediaOriginMs: 600_000, playbackRate: 2, overlapRemainingMs: 9_000,
            loadedItemRanges: [10...13.999]))
    }

    func testLoadedRendezvousRefusesHolesAndInsufficientFutureLead() {
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [10...11.5, 11.501...13]))
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [10...12.999]))
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 20_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [10...30]), "original staging is above the existing maximum lead")
    }

    func testLoadedRendezvousRejectsDisconnectedFutureIsland() {
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [9...11, 12...20]),
            "the original interval is short; the later island cannot supply its runway")
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [12...20]),
            "the original target must still belong to the loaded interval")
    }

    func testLoadedRendezvousMergesTouchingRangesWithoutIncreasingLead() throws {
        let plan = try XCTUnwrap(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 9_000,
            loadedItemRanges: [14...20, 10...14]))
        XCTAssertEqual(plan.filmPositionMs, 14_900,
            "loaded media cannot extend the original lead or consume the four-second reserve")
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 10_000, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 5_000,
            loadedItemRanges: [10...20]))
    }

    func testLoadedRendezvousRejectsInvalidClocksRatesAndOverflow() {
        for rate in [0, -1, Double.infinity, Double.nan, Double.greatestFiniteMagnitude] {
            XCTAssertNil(PreparedCommitRendezvous.planLoaded(
                stagedFilmPositionMs: 0, incumbentFilmPositionMs: 10_000,
                mediaOriginMs: 0, playbackRate: rate, overlapRemainingMs: 12_000,
                loadedItemRanges: [0...20]))
        }
        for invalid in [
            (staged: -1, current: 10_000, origin: 0),
            (staged: 0, current: -1, origin: 0),
            (staged: 0, current: 10_000, origin: -1),
            (staged: 0, current: Int.max, origin: 0)
        ] {
            XCTAssertNil(PreparedCommitRendezvous.planLoaded(
                stagedFilmPositionMs: invalid.staged, incumbentFilmPositionMs: invalid.current,
                mediaOriginMs: invalid.origin, playbackRate: 1, overlapRemainingMs: 12_000,
                loadedItemRanges: [0...20]))
        }
        XCTAssertNil(PreparedCommitRendezvous.planLoaded(
            stagedFilmPositionMs: 0, incumbentFilmPositionMs: 10_000,
            mediaOriginMs: 0, playbackRate: 1, overlapRemainingMs: 12_000,
            loadedItemRanges: [0...Double.infinity, -1...20]))
    }

    func testFutureRendezvousParksAheadWithoutChangingTheSuccessorOrigin() {
        let plan = PreparedCommitRendezvous.plan(stagedFilmPositionMs: 30_000,
            incumbentFilmPositionMs: 42_000, mediaOriginMs: 30_000, leadMs: 2_000)
        XCTAssertEqual(plan.filmPositionMs, 44_000)
        XCTAssertEqual(plan.itemPositionMs, 14_000)
        let negative = PreparedCommitRendezvous.plan(stagedFilmPositionMs: 30_000,
            incumbentFilmPositionMs: 42_000, mediaOriginMs: 30_000, leadMs: -2_000)
        XCTAssertEqual(negative.filmPositionMs, 42_000)
        let overflow = PreparedCommitRendezvous.plan(stagedFilmPositionMs: 0,
            incumbentFilmPositionMs: Int.max, mediaOriginMs: 0, leadMs: 1_000)
        XCTAssertEqual(overflow.filmPositionMs, Int.max)
    }

    func testTheSuccessorIsNeverAskedToSeekBehindWhereItWasPrimed() {
        let plan = PreparedCommitRendezvous.plan(
            stagedFilmPositionMs: 42_000,
            incumbentFilmPositionMs: 41_500,
            mediaOriginMs: 30_000
        )
        XCTAssertEqual(plan.filmPositionMs, 42_000)
        XCTAssertEqual(plan.itemPositionMs, 12_000)
    }

    func testAnOriginPastTheSwitchPointClampsRatherThanGoingNegative() {
        let plan = PreparedCommitRendezvous.plan(
            stagedFilmPositionMs: 0,
            incumbentFilmPositionMs: 500,
            mediaOriginMs: 30_000
        )
        XCTAssertEqual(plan.itemPositionMs, 0)
    }

    func testTheLeadKeepsThePostSwapReserveOfTheOverlap() {
        XCTAssertEqual(PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: 12_000),
                       PreparedReplacementBounds.alignmentMs * 2 - PreparedReplacementBounds.pollMs)
        XCTAssertEqual(PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: 9_000), 4_900,
                       "reserve the original four seconds plus one sampling poll before choosing the lead")
        XCTAssertNil(PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: 5_000),
                     "a commit that cannot keep its reserve is refused before it starts")
        XCTAssertEqual(PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: .max),
                       PreparedReplacementBounds.alignmentMs * 2)
    }

    func testPlannedAnchorToleratesSmallMediaLagWithoutSpendingItsPostSwapReserve() throws {
        let remainingMs = 9_000
        let lead = try XCTUnwrap(PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: remainingMs))
        let plan = PreparedCommitRendezvous.plan(stagedFilmPositionMs: 30_000,
            incumbentFilmPositionMs: 42_000, mediaOriginMs: 30_000, leadMs: lead)
        // During one second of preparation the incumbent clock advances only
        // 950ms. The planned margin absorbs that lag, not the frame tolerance.
        let afterLag = try XCTUnwrap(plan.clockAnchor(incumbentFilmMs: 42_950,
            hostSeconds: 101, rate: 1, frameDurationSeconds: 1 / 24,
            overlapRemainingMs: remainingMs - 1_000))
        XCTAssertEqual(afterLag.hostSeconds, 104.95, accuracy: 0.000_001)
        let exactRequiredMs = plan.filmPositionMs - 42_950 + PreparedCommitRendezvous.postSwapReserveMs
        XCTAssertNotNil(plan.clockAnchor(incumbentFilmMs: 42_950, hostSeconds: 101,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: exactRequiredMs))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_950, hostSeconds: 101,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: exactRequiredMs - 1),
            "one millisecond below the unchanged post-swap reserve still refuses")
    }

    func testClockAnchorMapsFilmLeadThroughPlaybackRateWithoutChangingOrigin() throws {
        let plan = PreparedCommitRendezvous.plan(stagedFilmPositionMs: 30_000,
            incumbentFilmPositionMs: 42_000, mediaOriginMs: 30_000, leadMs: 2_000)
        for (rate, wallLead) in [(0.5, 4.0), (1.0, 2.0), (2.0, 1.0)] {
            let anchor = try XCTUnwrap(plan.clockAnchor(incumbentFilmMs: 42_000,
                hostSeconds: 100, rate: rate, frameDurationSeconds: 1 / 24,
                overlapRemainingMs: 10_000))
            XCTAssertEqual(anchor.hostSeconds, 100 + wallLead, accuracy: 0.000_001)
            XCTAssertEqual(anchor.earliestItemMs, 12_000, accuracy: 0.000_001,
                "the lower item bound follows film origin, independently of playback rate")
        }
    }

    func testClockAnchorRefusesConsumedLeadAndPreservesPostSwapDeadline() {
        let plan = PreparedCommitRendezvous(filmPositionMs: 44_000, itemPositionMs: 14_000)
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 44_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 44_020, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 43_980, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000),
            "less than one actual frame of lead cannot schedule a safe rendezvous")
        let required = PreparedCommitRendezvous.postSwapReserveMs + 2_000
        XCTAssertNotNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: required))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: required - 1))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 0.5, frameDurationSeconds: 1 / 24, overlapRemainingMs: required),
            "the overlap pays wall time rather than unscaled film milliseconds")
    }

    func testClockAnchorRejectsInvalidClocksRatesAndHostTimeOverflow() {
        let plan = PreparedCommitRendezvous(filmPositionMs: 44_000, itemPositionMs: 14_000)
        for invalid in [Double.nan, .infinity, -.infinity, -1] {
            XCTAssertNil(plan.clockAnchor(incumbentFilmMs: invalid, hostSeconds: 100,
                rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
            XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: invalid,
                rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
        }
        for invalid in [Double.nan, .infinity, -.infinity, -1, 0] {
            XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
                rate: invalid, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
            XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
                rate: 1, frameDurationSeconds: invalid, overlapRemainingMs: 10_000))
        }
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000,
            hostSeconds: Double(Int64.max) / 1_000_000_000,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1.01, overlapRemainingMs: 10_000))
        XCTAssertNil(plan.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 0))
        let invalidOrigin = PreparedCommitRendezvous(filmPositionMs: 44_000, itemPositionMs: 1_000)
        XCTAssertNil(invalidOrigin.clockAnchor(incumbentFilmMs: 42_000, hostSeconds: 100,
            rate: 1, frameDurationSeconds: 1 / 24, overlapRemainingMs: 10_000))
    }

    func testMatchingClocksUseActualCadenceAndRejectBackwardStartupClock() {
        let plan = PreparedCommitRendezvous(filmPositionMs: 44_000, itemPositionMs: 14_000)
        XCTAssertTrue(plan.clocksMatch(incumbentFilmMs: 44_020,
            successorItemMs: 14_000, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: 44_020,
            successorItemMs: 14_000, frameDurationSeconds: 1 / 60),
            "a 60 fps stream cannot borrow a 24 fps clock tolerance")
        XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: 44_050,
            successorItemMs: 14_050, frameDurationSeconds: 1 / 24),
            "matching each other is insufficient after the rendezvous has passed")
        XCTAssertTrue(plan.acceptsDecodedAlignment(displaySeconds: 14,
            width: 1280, height: 720, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: 44_000,
            successorItemMs: 13_930, frameDurationSeconds: 1 / 24),
            "an aligned parked sample does not prove a running clock that rebased backward")
    }

    func testMatchingClocksRejectInvalidObservations() {
        let plan = PreparedCommitRendezvous(filmPositionMs: 44_000, itemPositionMs: 14_000)
        for invalid in [Double.nan, .infinity, -.infinity, -1] {
            XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: invalid,
                successorItemMs: 14_000, frameDurationSeconds: 1 / 24))
            XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: 44_000,
                successorItemMs: invalid, frameDurationSeconds: 1 / 24))
        }
        for invalid in [Double.nan, .infinity, -.infinity, -1, 0, 1.01] {
            XCTAssertFalse(plan.clocksMatch(incumbentFilmMs: 44_000,
                successorItemMs: 14_000, frameDurationSeconds: invalid))
        }
    }

    func testTheSharedCommitPlansItsSwitchPointBeforeInspectingIt() throws {
        let source = try playerControllerSource()
        let shared = try XCTUnwrap(source.range(of: "final class SharedPlayerController"))
        let tail = source[shared.upperBound...]
        let commit = try XCTUnwrap(tail.range(of: "func commitPreparedSuccessor("))
        let body = tail[commit.upperBound...]
        let plan = try XCTUnwrap(body.range(of: "PreparedCommitRendezvous.plan("))
        let inspect = try XCTUnwrap(body.range(of: "preparedDecodedFrameDuration(of: item"))
        let target = try XCTUnwrap(body.range(of: "targetItemSeconds: Double(rendezvous.itemPositionMs)"))
        let wait = try XCTUnwrap(body.range(of: "awaitSharedRendezvous("))
        let swap = try XCTUnwrap(body.range(of: "player.replaceCurrentItem(with: item)"))
        XCTAssertTrue(plan.lowerBound < inspect.lowerBound,
                      "a point taken after inspecting lands past the inspected fragment")
        XCTAssertTrue(inspect.lowerBound < target.lowerBound)
        XCTAssertTrue(wait.lowerBound < swap.lowerBound,
                      "the parked successor meets the incumbent before the swap")
    }

    // MARK: The order, which is the defect

    func testPreparedMetricsBindSuccessorBeforeObservationAndRestoreIncumbentOnRollback() throws {
        let body = try commitBody()
        let install = try XCTUnwrap(body.range(of: "installItemObserver(for: item)"))
        let session = try XCTUnwrap(body.range(of: "sessionId = action.sessionId"))
        let candidate = try XCTUnwrap(body.range(of: "autoActiveCandidateId = action.effectiveSelection.candidateId"))
        XCTAssertTrue(session.lowerBound < install.lowerBound && candidate.lowerBound < install.lowerBound,
            "completed-body observation captures the successor identity even without an Auto proposal")
        let proposal = try XCTUnwrap(body.range(of: "if let requested = autoDesiredCandidate"))
        XCTAssertTrue(install.lowerBound < proposal.lowerBound,
            "manual-to-Auto has no desired candidate; observation cannot depend on that proposal")
        XCTAssertNotNil(body.range(of: "candidateId: autoActiveCandidateId"),
            "rollback retains the exact incumbent candidate beside its session")
        let restore = try XCTUnwrap(body.range(of: "autoActiveCandidateId = incumbentState.candidateId"))
        let restoreSession = try XCTUnwrap(body.range(of: "sessionId = incumbentState.sessionId"))
        let restoreObserver = try XCTUnwrap(body.range(of: "installItemObserver(for: incumbent)"))
        XCTAssertTrue(restore.lowerBound < restoreObserver.lowerBound && restoreSession.lowerBound < restoreObserver.lowerBound,
            "failed first-frame proof restores both incumbent identities before its observation resumes")
    }

    func testCadencePreparationFinishesBeforeOneTargetIsFrozenAndDisposalCancelsIt() throws {
        let source = try playerControllerSource()
        let body = try commitBody()
        let freeze = try XCTUnwrap(body.range(of: "guard let rendezvous = loadedRendezvous, let cadence = loadedCadence"))
        let align = try XCTUnwrap(body.range(of: "awaitPreparedAlignment(of: item"))
        XCTAssertFalse(body[..<align.lowerBound].contains("awaitPreparedFrameDuration"))
        XCTAssertTrue(freeze.lowerBound < align.lowerBound)
        let discard = try XCTUnwrap(source.range(of: "func discardPreparedSuccessor()"))
        XCTAssertTrue(source[discard.upperBound...].hasPrefix(" {\n        clearPreparedCadence()"))
        XCTAssertTrue(source.contains("self.preparedCadenceGeneration == generation"))
        XCTAssertTrue(source.contains("self.viewerActionEpoch == epoch"))
        XCTAssertTrue(source.contains("self.preparedReplacement.activeAction?.actionId == action.actionId"))
    }

    func testTheCommitFinishesTheAlignmentBeforeItExposesTheItem() throws {
        let body = try commitBody()
        let align = try XCTUnwrap(body.range(of: "awaitPreparedAlignment(of: item"))
        let boundary = try XCTUnwrap(body.range(of: "let boundaryMs = rendezvous.filmPositionMs"))
        // The successor is exposed by promoting its warm player onto the
        // surface (continuous quality, #774), not by swapping an item into
        // the incumbent player.
        let expose = try XCTUnwrap(body.range(of: "playbackSurface?.promote(successor)"))
        XCTAssertTrue(
            align.lowerBound < expose.lowerBound,
            "exposing first shows the viewer the staged position, which is behind the picture"
        )
        XCTAssertTrue(
            align.lowerBound < boundary.lowerBound && boundary.lowerBound < expose.lowerBound,
            "the boundary is the position the alignment reached, read before the swap"
        )
    }

    func testAnAlignmentThatCannotLandRefusesInsteadOfSwitching() throws {
        let body = try commitBody()
        let gate = try XCTUnwrap(body.range(of: "guard await awaitPreparedAlignment(of: item"))
        let tail = body[gate.upperBound...]
        let refusal = try XCTUnwrap(
            tail.range(of: "PreparedCommitRendezvous.outcomeWhenAlignmentCannotLand")
        )
        let expose = try XCTUnwrap(tail.range(of: "playbackSurface?.promote(successor)"))
        XCTAssertTrue(refusal.lowerBound < expose.lowerBound)
        XCTAssertEqual(
            PreparedCommitRendezvous.outcomeWhenAlignmentCannotLand,
            PreparedCommitOutcome.refused,
            "nothing was switched, so the incumbent is untouched and the tap still gets its change"
        )
    }

    func testTheAlignmentIsBoundedAndTheBoundIsBelowTheFirstFrameOne() throws {
        let body = try commitBody()
        XCTAssertNotNil(body.range(of: "awaitBoundedValue"), "via awaitPreparedAlignment")
        XCTAssertEqual(PreparedReplacementBounds.alignmentMs, 4_000)
        XCTAssertLessThan(
            PreparedReplacementBounds.alignmentMs,
            PreparedReplacementBounds.firstFrameMs,
            "a switch that has not happened yet can still be given up on cheaply"
        )
    }

    // MARK: The bound

    func testABoundedWaitGivesUpOnAValueThatNeverArrives() async {
        var nowMs = 0
        let value: Bool? = await awaitBoundedValue(
            boundMs: PreparedReplacementBounds.alignmentMs,
            pollMs: PreparedReplacementBounds.pollMs,
            now: { nowMs },
            sleep: { nowMs += $0 },
            read: { nil }
        )
        XCTAssertNil(value, "a seek that never comes back is not an answer")
        XCTAssertGreaterThanOrEqual(nowMs, PreparedReplacementBounds.alignmentMs)
        XCTAssertLessThan(
            nowMs,
            PreparedReplacementBounds.alignmentMs + 4 * PreparedReplacementBounds.pollMs,
            "and it is given up on at the bound, not some way past it"
        )
    }

    func testABoundedWaitTakesTheValueTheMomentItLands() async {
        var nowMs = 0
        var landed: Bool?
        let value = await awaitBoundedValue(
            boundMs: PreparedReplacementBounds.alignmentMs,
            pollMs: PreparedReplacementBounds.pollMs,
            now: { nowMs },
            sleep: {
                nowMs += $0
                if nowMs >= 300 { landed = true }
            },
            read: { landed }
        )
        XCTAssertEqual(value, true)
        XCTAssertLessThan(nowMs, 1_000, "it does not wait out a bound it has already beaten")
    }

    func testAValueThatLandsInsideTheFinalPollIsStillTaken() async {
        var nowMs = 0
        var landed: Bool?
        let value = await awaitBoundedValue(
            boundMs: 400,
            pollMs: 100,
            now: { nowMs },
            sleep: {
                nowMs += $0
                if nowMs >= 400 { landed = false }
            },
            read: { landed }
        )
        XCTAssertEqual(
            value, false,
            "a seek that comes back in the last poll answered; it did not time out"
        )
    }
    func testSharedInitialParkingUsesBoundedGenerationAndAbortOrFailureSettlement() throws {
        let source = try playerControllerSource()
        let start = try XCTUnwrap(source.range(of: "private func prime(_ offer: SharedPreparedOffer"))
        let end = try XCTUnwrap(source.range(of: "private func adopt(_ adoption:", range: start.upperBound..<source.endIndex))
        let prime = source[start.lowerBound..<end.lowerBound]
        XCTAssertFalse(prime.contains("await item.seek"), "AVFoundation completion is not a readiness bound")
        XCTAssertTrue(prime.contains("coordinator.readinessRemainingMs()"))
        XCTAssertTrue(prime.contains("guard await alignPrepared(item, to: preparedFilmPositionMs, boundMs: remaining)"))
        XCTAssertTrue(prime.contains("coordinator.abandonWithoutFallback(.aborted)"))
        XCTAssertTrue(prime.contains("coordinator.abandon(.failed)"))
        let alignment = try XCTUnwrap(source.range(of: "private func alignPrepared(_ item:"))
        let tail = source[alignment.upperBound...]
        XCTAssertTrue(tail.contains("preparedAlignment.generation &+= 1"))
        XCTAssertTrue(tail.contains("item.cancelPendingSeeks()"))
        XCTAssertTrue(tail.contains("!self.preemptRequested, !self.closing, !Task.isCancelled"))
    }

    func testFirstFrameBoundaryUsesActualCadenceForLocalAndShared() {
        XCTAssertTrue(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 7_980, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 7_950, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
        XCTAssertTrue(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 8_050, boundaryMs: 8_000, frameDurationSeconds: 1 / 60))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 8_000, boundaryMs: 8_000, frameDurationSeconds: 0))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: .nan, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
    }

}
