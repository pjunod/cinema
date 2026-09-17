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
        let expose = try XCTUnwrap(body.range(of: "player.replaceCurrentItem(with: item)"))
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
        let expose = try XCTUnwrap(tail.range(of: "player.replaceCurrentItem(with: item)"))
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
}
