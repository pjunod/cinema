import Foundation
import XCTest
@testable import plurx

// MARK: - Fixtures

private let offerStagingId = "3c1a5f90-7d2b-4e6a-9b18-4f0c2d7e5a31"
private let offerSessionId = "5e2b7c41-8a3d-4f10-9c6e-1b8d0a4f2e73"
private let olderStagingId = "1a2b3c4d-5e6f-4a8b-9c0d-1e2f3a4b5c6d"

private func offerSelection() -> EffectiveSelection {
    EffectiveSelection(
        qualityAuto: false,
        height: 1_080,
        audioTrack: nil,
        subtitleBurn: nil,
        audioOffsetMs: 0,
        codec: "server_selected",
        dynamicRange: "sdr"
    )
}

private func prepareAction(
    actionId: String = offerStagingId,
    sessionId: String = offerSessionId
) -> ControlAction {
    ControlAction(
        type: PlaybackControl.prepareActionType,
        actionId: actionId,
        sessionId: sessionId,
        playlistUrl: "/api/v1/hls/\(sessionId)/index.m3u8",
        mediaOriginMs: 0,
        effectiveSelection: offerSelection()
    )
}

private func answer(
    sequence: Int,
    action: ControlAction? = ControlAction(type: "none"),
    preparation: String? = nil
) -> PlaybackControlAnswer {
    PlaybackControlAnswer(
        requestSequence: sequence, action: action, preparation: preparation
    )
}

private func makeWait(tappedAtMs: Int = 10_000, floor: Int = 4) -> PreparedOfferWait {
    PreparedOfferWait(tappedAtMs: tappedAtMs, floorSequence: floor)
}

/// The wait a viewer's directed quality change does with the picture still up.
///
/// Every case here is one of the five rules in the M2 build note, in the order
/// the rules are applied — and the order is the substance. A bound checked
/// after the answer branches never fires on a server that answers nothing; a
/// `prepare` taken before the floor is checked takes a *replay of an older
/// staging* as this change's answer; and a `none` read on the exchange that
/// carried the ask declines a preparation the server has not begun to think
/// about yet.
final class PreparedOfferWaitTests: XCTestCase {

    // MARK: Rule 1 — the bound is checked first

    func testTheBoundExpiresBeforeAnyExchangeHasBeenAccepted() {
        var subject = makeWait(tappedAtMs: 10_000)
        XCTAssertEqual(
            subject.observe(answer: nil, nowMs: 10_000 + PreparedOfferWait.boundMs),
            PreparedOfferWait.Step.reopen(reason: "timed_out"),
            "a server that never answers is exactly what the bound is for"
        )
        XCTAssertFalse(subject.sawAcceptedAnswer)
    }

    func testTheBoundOutranksAnAnswerThatWouldOtherwiseBeTaken() {
        var subject = makeWait(tappedAtMs: 10_000)
        let expired = 10_000 + PreparedOfferWait.boundMs + 5
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 9, action: prepareAction()), nowMs: expired),
            PreparedOfferWait.Step.reopen(reason: "timed_out")
        )
    }

    func testJustInsideTheBoundStillWaits() {
        var subject = makeWait(tappedAtMs: 10_000)
        XCTAssertEqual(
            subject.observe(answer: nil, nowMs: 10_000 + PreparedOfferWait.boundMs - 1),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs)
        )
    }

    // MARK: Rule 2 — an answer from below the floor is not ours

    func testAnAnswerFromBeforeTheAskIsNotEvidenceEitherWay() {
        var subject = makeWait(floor: 4)
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 3, preparation: "none"), nowMs: 10_100),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs),
            "an answer that predates the ask cannot decline it"
        )
        XCTAssertFalse(subject.sawAcceptedAnswer,
                       "and it does not count as the exchange that carried the ask")
    }

    func testAPrepareReplayedOnAnOlderSequenceIsNotTaken() {
        var subject = makeWait(floor: 7)
        let replay = answer(
            sequence: 6, action: prepareAction(actionId: olderStagingId), preparation: "offered"
        )
        XCTAssertEqual(
            subject.observe(answer: replay, nowMs: 10_100),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs),
            "the server replays a prepare until it settles; an older one is not this ask's answer"
        )
    }

    // MARK: Rule 3 — a prepare is the answer whatever the hint says

    func testAPrepareIsTakenWhateverThePreparationFieldSays() {
        for preparation in [nil, "staging", "offered", "none", "something_newer"] {
            var subject = makeWait()
            let step = subject.observe(
                answer: answer(sequence: 4, action: prepareAction(), preparation: preparation),
                nowMs: 10_100
            )
            guard case .offered(let action) = step else {
                XCTFail("preparation=\(preparation ?? "nil") should still offer, got \(step)")
                continue
            }
            XCTAssertEqual(action.actionId, offerStagingId)
        }
    }

    // MARK: Rule 4 — `none`, but only after the dispatch exchange

    func testNoneOnTheExchangeThatCarriedTheAskIsNotADecline() {
        var subject = makeWait()
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 4, preparation: "none"), nowMs: 10_050),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs),
            "the server had only just accepted the ask when it built that response"
        )
        XCTAssertTrue(subject.sawAcceptedAnswer)
    }

    func testNoneOnAnExchangeAfterTheAskDeclines() {
        var subject = makeWait()
        _ = subject.observe(answer: answer(sequence: 4, preparation: "staging"), nowMs: 10_050)
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 5, preparation: "none"), nowMs: 10_900),
            PreparedOfferWait.Step.reopen(reason: "declined"),
            "a server that has looked and says there will be none is a decline"
        )
    }

    /// A3 — the caller polls every 25 ms and is handed the *same* answer back
    /// until the next exchange lands. A rule keyed on "have I observed an
    /// accepted answer before" is therefore satisfied 25 milliseconds later by
    /// the dispatch exchange's own answer, and the client declines a quarter of
    /// a second into a twelve-second bound.
    ///
    /// It is not a theoretical ordering: the server withholds a preparation
    /// purpose while the incumbent is waiting for capacity, which is transient.
    /// A viewer who taps quality in that window would never see the `staging`
    /// the next exchange carries.
    func testTheDispatchAnswerObservedAgainWithinOnePollIsStillNotADecline() {
        var subject = makeWait()
        let dispatch = answer(sequence: 4, preparation: "none")
        XCTAssertEqual(
            subject.observe(answer: dispatch, nowMs: 10_050),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs)
        )
        XCTAssertEqual(
            subject.observe(answer: dispatch, nowMs: 10_075),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs),
            "looking twice is not the same as a second exchange having looked"
        )
    }

    /// The same, over a whole second of polling — the shape the caller actually
    /// produces between two exchanges of a five-second cadence.
    func testTheDeclineNeedsALaterExchangeRatherThanALaterLook() {
        var subject = makeWait(tappedAtMs: 0)
        let dispatch = answer(sequence: 4, preparation: "none")
        for nowMs in stride(from: 25, through: 2_000, by: 25) {
            XCTAssertEqual(
                subject.observe(answer: dispatch, nowMs: nowMs),
                PreparedOfferWait.Step.keepWaiting(
                    nextExchangeMs: PreparedOfferWait.stagingCadenceMs
                ),
                "still the dispatch exchange's own answer at \(nowMs)ms"
            )
        }
        XCTAssertEqual(subject.dispatchSequence, 4)
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 5, preparation: "none"), nowMs: 2_025),
            PreparedOfferWait.Step.reopen(reason: "declined"),
            "a later exchange has looked, and says there will be none"
        )
    }

    /// And the transient case the guard exists for, end to end: the dispatch
    /// exchange says `none` because the incumbent was momentarily waiting for
    /// capacity, and the next one says `staging`.
    func testADispatchNoneThatBecomesStagingIsNeverDeclined() {
        var subject = makeWait(tappedAtMs: 0)
        let dispatch = answer(sequence: 4, preparation: "none")
        for nowMs in stride(from: 25, through: 1_000, by: 25) {
            _ = subject.observe(answer: dispatch, nowMs: nowMs)
        }
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 5, preparation: "staging"), nowMs: 1_025),
            PreparedOfferWait.Step.keepWaiting(
                nextExchangeMs: PreparedOfferWait.stagingCadenceMs
            )
        )
        XCTAssertTrue(subject.lastSaidStaging)
        let step = subject.observe(
            answer: answer(sequence: 6, action: prepareAction(), preparation: "offered"),
            nowMs: 2_025
        )
        guard case .offered = step else {
            return XCTFail("the staging it would never have seen, got \(step)")
        }
    }

    func testTwoConsecutiveNonesDeclineOnTheSecond() {
        var subject = makeWait()
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 4, preparation: "none"), nowMs: 10_050),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs)
        )
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 5, preparation: "none"), nowMs: 10_500),
            PreparedOfferWait.Step.reopen(reason: "declined")
        )
    }

    // MARK: Rule 5 — `staging`, and the old server that says nothing

    func testStagingKeepsTheIncumbentPlayingAtOneHertz() {
        var subject = makeWait()
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 4, preparation: "staging"), nowMs: 10_050),
            PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs)
        )
        XCTAssertTrue(subject.lastSaidStaging,
                      "staging is the only value that earns an extra exchange")
        XCTAssertEqual(PreparedOfferWait.stagingCadenceMs, 1_000)
    }

    func testAnOldServerSendsNoFieldAndIsNotNudgedAndNotDeclined() {
        var subject = makeWait(tappedAtMs: 0)
        var now = 0
        while now < PreparedOfferWait.boundMs {
            let step = subject.observe(answer: answer(sequence: 4 + now / 250), nowMs: now)
            XCTAssertEqual(
                step, PreparedOfferWait.Step.keepWaiting(nextExchangeMs: PreparedOfferWait.stagingCadenceMs),
                "absence is not a decline, at \(now)ms"
            )
            XCTAssertFalse(subject.lastSaidStaging,
                           "absent is not staging: an old server is waited out, not nudged")
            now += 250
        }
        XCTAssertEqual(
            subject.observe(answer: answer(sequence: 60), nowMs: PreparedOfferWait.boundMs),
            PreparedOfferWait.Step.reopen(reason: "timed_out"),
            "the bound is what decides for a server that cannot say"
        )
    }

    func testAStagingThatTurnsIntoAnOfferIsTaken() {
        var subject = makeWait()
        _ = subject.observe(answer: answer(sequence: 4, preparation: "staging"), nowMs: 10_050)
        _ = subject.observe(answer: answer(sequence: 5, preparation: "staging"), nowMs: 11_050)
        let step = subject.observe(
            answer: answer(sequence: 6, action: prepareAction(), preparation: "offered"),
            nowMs: 12_050
        )
        guard case .offered(let action) = step else {
            return XCTFail("the staging finished and should have been offered, got \(step)")
        }
        XCTAssertEqual(action.sessionId, offerSessionId)
    }
}

/// Where a quality-only change reopens when the prepared path hands it back.
final class QualityChangeReopenTests: XCTestCase {
    /// The whole point of M2's position rule: the viewer keeps watching for up
    /// to twelve seconds, so the tap's position is twelve seconds of film
    /// behind the picture by the time the fallback runs.
    func testTheReopenLandsWhereTheFilmGotToNotWhereItWasTapped() {
        let tappedAtPositionMs = 30_000
        // The incumbent kept playing for the whole of the wait, which is the
        // only reason a twelve-second wait is affordable at all.
        let advanced = tappedAtPositionMs + PreparedOfferWait.boundMs
        let positionNow = QualityChangeReopen.positionNowMs(
            pinnedAtTapMs: tappedAtPositionMs,
            livePositionMs: advanced,
            carryingASeek: false
        )
        let target = QualityChangeReopen.target(
            seekGenerationAtTap: 12,
            seekGenerationNow: 12,
            positionNowMs: positionNow
        )
        XCTAssertEqual(target, advanced)
        XCTAssertNotEqual(target, tappedAtPositionMs,
                          "reopening at the tap would rewind the film by the whole wait")
    }

    /// The presentation destination the tap creates is still created — the
    /// progress bar and the stall-recovery suppression are built on it — so
    /// the position has to be read again rather than taken from it.
    func testThePinnedDestinationIsNotTheFallbackPosition() {
        XCTAssertEqual(
            QualityChangeReopen.positionNowMs(
                pinnedAtTapMs: 30_000, livePositionMs: 42_000, carryingASeek: false
            ),
            42_000
        )
    }

    /// A quality change made on top of a seek that has not landed inherits the
    /// viewer's destination. Reading the live clock there would resume at the
    /// position they were leaving.
    func testAChangeMadeOnTopOfAnUnlandedSeekKeepsTheViewersDestination() {
        XCTAssertEqual(
            QualityChangeReopen.positionNowMs(
                pinnedAtTapMs: 600_000, livePositionMs: 42_000, carryingASeek: true
            ),
            600_000
        )
    }

    func testAnUnreadableClockFallsBackToThePinRatherThanToZero() {
        XCTAssertEqual(
            QualityChangeReopen.positionNowMs(
                pinnedAtTapMs: 30_000, livePositionMs: nil, carryingASeek: false
            ),
            30_000
        )
    }

    func testTheFallbackPositionNeverGoesBackwards() {
        XCTAssertEqual(
            QualityChangeReopen.positionNowMs(
                pinnedAtTapMs: 30_000, livePositionMs: 1_000, carryingASeek: false
            ),
            30_000,
            "a clock read mid-rebase must not send the viewer behind the tap"
        )
    }

    func testAViewerSeekDuringTheWaitOwnsThePositionAndTheQualityPathReopensNothing() {
        XCTAssertNil(
            QualityChangeReopen.target(
                seekGenerationAtTap: 12,
                seekGenerationNow: 13,
                positionNowMs: 44_000
            ),
            "the seek carries the new selection itself; a second reopen would fight it"
        )
    }

    func testANegativePositionIsClampedRatherThanPassedOn() {
        XCTAssertEqual(
            QualityChangeReopen.target(
                seekGenerationAtTap: 0, seekGenerationNow: 0, positionNowMs: -1
            ),
            0
        )
    }
}
