package tv.plurx.app.player

import tv.plurx.app.data.PlaybackQuality
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The wait between the viewer's tap and the server's answer.
 *
 * This is the piece the M6 handoff never had, and it is deliberately in
 * `PreparedReplacement.kt` rather than in `Controller.kt` so that these can
 * exist at all: every M6 defect that survived review lived in the controller,
 * where the JVM unit lane cannot reach.
 *
 * The rules under test, in the order [PreparedOfferWait.observe] applies them:
 *
 *  1. the bound, from the tap, before anything else;
 *  2. an answer below the floor is not ours;
 *  3. a `prepare` is the offer, whatever `preparation` says;
 *  4. `none` declines, but never on the exchange that carried the ask;
 *  5. `staging` — or silence from an older server — keeps waiting.
 */
private const val PREPARE = PlaybackControl.PREPARE_ACTION_TYPE

private fun prepareAction(actionId: String = "11111111-1111-4111-8111-111111111111") =
    ControlAction(
        type = PREPARE,
        actionId = actionId,
        sessionId = "22222222-2222-4222-8222-222222222222",
        playlistUrl = "/hls/x/index.m3u8",
        mediaOriginMs = 0,
    )

private fun holdAction() = ControlAction(type = "hold", reason = "no_room")

private fun answer(
    sequence: Long,
    preparation: String? = null,
    action: ControlAction? = holdAction(),
) = ControlAnswer(requestSequence = sequence, action = action, preparation = preparation)

/** The tap is at 0 and the floor is 7 throughout, so neither is ever incidental. */
private fun wait() = PreparedOfferWait(tappedAtMs = 0L, floorSequence = 7L)

class PreparedOfferWaitTest {

    // ---------------------------------------------------------------- bound

    @Test
    fun theBoundIsMeasuredFromTheTapAndEndsTheWait() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(answer(7, PREPARATION_STAGING), PreparedOfferWait.BOUND_MS - 1),
        )
        assertEquals(
            PreparedOfferWait.Step.Reopen("timed_out"),
            subject.observe(answer(8, PREPARATION_STAGING), PreparedOfferWait.BOUND_MS),
        )
    }

    /**
     * The bound is checked first, and that is not a stylistic choice.
     *
     * A server that never answers, a reporter that died with the exchange in
     * flight, a relay that swallowed the dispatch: none of them produce an
     * answer to read, and the viewer is waiting through every one of them. A
     * bound that only ran after an accepted exchange would never fire in
     * exactly the cases it exists for.
     */
    @Test
    fun theBoundFiresBeforeAnyExchangeHasBeenAccepted() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.Reopen("timed_out"),
            subject.observe(null, PreparedOfferWait.BOUND_MS + 1),
        )
        assertFalse(subject.sawAcceptedAnswer)
    }

    @Test
    fun theBoundBeatsAnOfferOnTheSameObservation() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.Reopen("timed_out"),
            subject.observe(
                answer(9, PREPARATION_OFFERED, prepareAction()),
                PreparedOfferWait.BOUND_MS,
            ),
        )
    }

    // ---------------------------------------------------------------- floor

    @Test
    fun anAnswerBelowTheFloorIsNotOurs() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(answer(6, PREPARATION_NONE), 10),
        )
        // And it did not count as the exchange that carried the ask, so the
        // next `none` is still the first accepted answer rather than a decline.
        assertFalse(subject.sawAcceptedAnswer)
        assertNull(subject.lastPreparation)
    }

    @Test
    fun anOfferBelowTheFloorIsNotOursEither() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(answer(6, PREPARATION_OFFERED, prepareAction()), 10),
        )
    }

    // ---------------------------------------------------------------- offer

    @Test
    fun aPrepareAtOrAboveTheFloorIsTheOffer() {
        val action = prepareAction()
        assertEquals(
            PreparedOfferWait.Step.Offered(action),
            wait().observe(answer(7, PREPARATION_OFFERED, action), 10),
        )
    }

    /**
     * The action is the decision; `preparation` is advice about one. A server
     * that sends a `prepare` alongside `staging`, or alongside nothing at all,
     * has still named a successor.
     */
    @Test
    fun aPrepareIsTheOfferWhateverPreparationSays() {
        val action = prepareAction()
        for (preparation in listOf(null, PREPARATION_STAGING, PREPARATION_NONE, "something_new")) {
            assertEquals(
                PreparedOfferWait.Step.Offered(action),
                wait().observe(answer(7, preparation, action), 10),
                "preparation=$preparation",
            )
        }
    }

    // -------------------------------------------------------------- decline

    /**
     * The exchange that carried the ask is answered before the server has
     * looked, so its `none` is "not yet", not "no". Declining on it would
     * reopen instantly on every rung change.
     */
    @Test
    fun theFirstAcceptedAnswerNeverDeclines() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(answer(7, PREPARATION_NONE), 10),
        )
        assertTrue(subject.sawAcceptedAnswer)
        assertEquals(PREPARATION_NONE, subject.lastPreparation)
    }

    @Test
    fun aLaterNoneDeclines() {
        val subject = wait()
        subject.observe(answer(7, PREPARATION_STAGING), 10)
        assertEquals(
            PreparedOfferWait.Step.Reopen("declined"),
            subject.observe(answer(8, PREPARATION_NONE), 20),
        )
    }

    @Test
    fun aNoneAfterASilentFirstExchangeStillDeclines() {
        val subject = wait()
        subject.observe(answer(7, null), 10)
        assertEquals(
            PreparedOfferWait.Step.Reopen("declined"),
            subject.observe(answer(8, PREPARATION_NONE), 20),
        )
    }

    // -------------------------------------------------------------- staging

    @Test
    fun stagingKeepsWaitingOnTheStagingCadence() {
        val subject = wait()
        subject.observe(answer(7, PREPARATION_STAGING), 10)
        val step = subject.observe(answer(8, PREPARATION_STAGING), 20)
        assertEquals(PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS), step)
        // The word itself, not merely the decision: the developer row shows
        // what the server said, and the nudge cadence is armed off it.
        assertEquals(PREPARATION_STAGING, subject.lastPreparation)
    }

    @Test
    fun stagingNeverDeclinesHoweverManyTimesItIsRepeated() {
        val subject = wait()
        var now = 0L
        var sequence = 7L
        repeat(10) {
            now += 500
            assertEquals(
                PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
                subject.observe(answer(sequence++, PREPARATION_STAGING), now),
                "at ${now}ms",
            )
        }
    }

    /**
     * An older server, and every relay in front of one, sends no `preparation`
     * at all. Absence is not a decline: such a server may well be priming a
     * successor, and reading its silence as a refusal would reopen on top of
     * the very preparation the client asked for.
     */
    @Test
    fun anOldServerThatSendsNoPreparationIsNotADecline() {
        val subject = wait()
        var now = 0L
        var sequence = 7L
        repeat(10) {
            now += 500
            assertEquals(
                PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
                subject.observe(answer(sequence++, null), now),
                "at ${now}ms",
            )
        }
        assertNull(subject.lastPreparation)
        // The bound is what decides for such a server, and it still does.
        assertEquals(
            PreparedOfferWait.Step.Reopen("timed_out"),
            subject.observe(answer(sequence, null), PreparedOfferWait.BOUND_MS),
        )
    }

    @Test
    fun aWordThisClientHasNeverHeardIsANewerServerRatherThanARefusal() {
        val subject = wait()
        subject.observe(answer(7, PREPARATION_STAGING), 10)
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(answer(8, "reserving"), 20),
        )
    }

    @Test
    fun noAnswerAtAllKeepsWaitingInsideTheBound() {
        val subject = wait()
        assertEquals(
            PreparedOfferWait.Step.KeepWaiting(PreparedOfferWait.STAGING_CADENCE_MS),
            subject.observe(null, 10),
        )
    }
}

/**
 * The viewer's rung, from the tap until it is honoured.
 *
 * The defect these pin is that a preparation which failed after the offer used
 * to release the successor and acknowledge `failed` — and the rung the viewer
 * chose was never applied to anything. The tap evaporated.
 */
class DirectedChangeTest {
    private val quality = PlaybackQuality.Auto

    @Test
    fun aCommittedChangeNeverFallsBack() {
        val change = DirectedChange(epoch = 4, quality = quality)
        change.committed()
        var routes = 0
        assertFalse(change.fallBackOnce("fallback", currentEpoch = 4) { routes += 1 })
        assertEquals(0, routes)
    }

    @Test
    fun aFailureFallsBackExactlyOnce() {
        val change = DirectedChange(epoch = 4, quality = quality)
        var routes = 0
        assertTrue(change.fallBackOnce("fallback", currentEpoch = 4) { routes += 1 })
        assertEquals(1, routes)
    }

    @Test
    fun twoFailuresStillFallBackOnlyOnce() {
        val change = DirectedChange(epoch = 4, quality = quality)
        var routes = 0
        assertTrue(change.fallBackOnce("timed_out", currentEpoch = 4) { routes += 1 })
        assertFalse(change.fallBackOnce("fallback", currentEpoch = 4) { routes += 1 })
        assertFalse(change.fallBackOnce("declined", currentEpoch = 4) { routes += 1 })
        assertEquals(1, routes)
    }

    @Test
    fun aSupersededChangeSaysNothing() {
        val change = DirectedChange(epoch = 4, quality = quality)
        change.superseded()
        var routes = 0
        assertFalse(change.fallBackOnce("fallback", currentEpoch = 4) { routes += 1 })
        assertEquals(0, routes)
    }

    /**
     * The media moved on underneath: another rung, another title, a reopen
     * somebody else already took. Routing now would drag the viewer back to a
     * rung they have since left.
     */
    @Test
    fun aChangeFromAnOlderEpochDoesNotRoute() {
        val change = DirectedChange(epoch = 4, quality = quality)
        var routes = 0
        assertFalse(change.fallBackOnce("fallback", currentEpoch = 5) { routes += 1 })
        assertEquals(0, routes)
        assertTrue(change.isSettled)
    }

    @Test
    fun anEpochMismatchAlsoSpendsTheOneReopen() {
        val change = DirectedChange(epoch = 4, quality = quality)
        var routes = 0
        change.fallBackOnce("fallback", currentEpoch = 5) { routes += 1 }
        // Even if the epoch came back — it cannot, epochs only rise — the
        // change is spent. One owner, one outcome.
        assertFalse(change.fallBackOnce("fallback", currentEpoch = 4) { routes += 1 })
        assertEquals(0, routes)
    }
}
