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

    // MARK: The order, which is the defect

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
    func testFirstFrameBoundaryUsesActualCadenceForLocalAndShared() {
        XCTAssertTrue(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 7_980, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 7_950, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
        XCTAssertTrue(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 8_050, boundaryMs: 8_000, frameDurationSeconds: 1 / 60))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: 8_000, boundaryMs: 8_000, frameDurationSeconds: 0))
        XCTAssertFalse(PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: .nan, boundaryMs: 8_000, frameDurationSeconds: 1 / 24))
    }

}
