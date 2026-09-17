package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

/**
 * The rendezvous, driven by two simulated clocks.
 *
 * The two clocks are not decoration. *Film* time is what both pipelines measure
 * positions in and what the 250 ms commit window is expressed in; *wall* time is
 * what a coroutine delay and a seek are measured in. They run at the same rate
 * only at 1x, and the whole arithmetic under test is the conversion between
 * them — a 1 500 ms lead is 3 000 ms of waiting at 0.5x and 750 ms at 2x.
 *
 * What is being proved is **convergence**, not that a target was computed. An
 * earlier draft of this milestone proposed seeking the successor 1.5 s ahead
 * and letting it run; that arithmetic produces the same first target this one
 * does and never converges, because two pipelines at the same rate keep
 * whatever gap the seek landed with. [Sim] therefore models the successor's own
 * clock and honours [RendezvousHold.Park.playWhenReady], so a policy that lets
 * the successor run is a policy these tests fail.
 */

/** How often the readiness poll looks, matching the controller's own cadence. */
private const val POLL_MS = RENDEZVOUS_READY_POLL_MS

/** Nobody's seek or wait is allowed to take longer than this in simulation. */
private const val SIM_LIMIT_MS = 120_000L

private sealed interface Outcome {
    data class Committed(val wallMs: Long, val filmMs: Long, val reparks: Int) : Outcome
    data class Abandoned(val reason: String, val reparks: Int) : Outcome
    data object NeverSettled : Outcome
}

/**
 * Two pipelines and a wall clock.
 *
 * The incumbent always plays at the rate [speedAt] gives for the current wall
 * time. The successor advances only while it is running — which, under the
 * rendezvous, it never is until the swap.
 */
private class Sim(
    private val seekMs: Long,
    private val speedAt: (Long) -> Double,
    /** How far past its own position the successor holds contiguous media. */
    private val runwayMs: Long = 60_000L,
) {
    var wallMs = 0L
        private set

    private var incumbentFilm = 0.0
    private var successorFilm = 0.0
    private var successorRuns = false
    private var seekLandsAtMs: Long? = null
    private var seekTargetMs = 0L

    var landed = false
        private set

    val incumbentFilmMs: Long get() = incumbentFilm.toLong()
    val successorFilmMs: Long get() = successorFilm.toLong()
    val speed: Double get() = speedAt(wallMs)
    val bufferedThroughMs: Long get() = successorFilmMs + runwayMs

    /** One millisecond at a time, so a rate change lands where it happened. */
    fun advance(deltaMs: Long) {
        repeat(deltaMs.coerceAtLeast(0L).toInt()) {
            val rate = speedAt(wallMs)
            wallMs += 1
            incumbentFilm += rate
            if (successorRuns) successorFilm += rate
            val lands = seekLandsAtMs
            if (lands != null && wallMs >= lands) {
                successorFilm = seekTargetMs.toDouble()
                seekLandsAtMs = null
                landed = true
            }
        }
    }

    fun seek(park: RendezvousHold.Park) {
        seekTargetMs = park.rendezvousFilmMs
        seekLandsAtMs = wallMs + seekMs
        landed = false
        // The policy's own answer, honoured. This is where "parked" and
        // "chasing" become two different simulations.
        successorRuns = park.playWhenReady
    }
}

/**
 * Drive a hold to a terminal outcome, exactly as the controller drives it:
 * park, poll for the seek and the runway, then wait out the film distance and
 * fire.
 */
private fun run(sim: Sim, hold: RendezvousHold = RendezvousHold()): Outcome {
    sim.seek(hold.park(sim.wallMs, sim.incumbentFilmMs))
    while (sim.wallMs < SIM_LIMIT_MS) {
        if (!hold.isReady) {
            sim.advance(POLL_MS)
            val target = hold.rendezvousFilmMs ?: return Outcome.NeverSettled
            if (sim.landed && successorIsBuffered(sim.bufferedThroughMs, target)) {
                hold.ready(sim.wallMs)
            }
            continue
        }
        sim.advance(hold.delayMs(sim.incumbentFilmMs, sim.speed))
        val step = hold.fire(
            nowMs = sim.wallMs,
            incumbentFilmMs = sim.incumbentFilmMs,
            successorFilmMs = sim.successorFilmMs,
            successorReady = hold.isReady,
            speed = sim.speed,
        )
        when (step) {
            is RendezvousHold.Step.Commit ->
                return Outcome.Committed(sim.wallMs, step.filmMs, hold.reparks)
            is RendezvousHold.Step.Wait -> Unit
            is RendezvousHold.Step.Repark -> sim.seek(step.park)
            is RendezvousHold.Step.Abandon ->
                return Outcome.Abandoned(step.reason, hold.reparks)
        }
    }
    return Outcome.NeverSettled
}

private fun committed(outcome: Outcome): Outcome.Committed {
    assertTrue(outcome is Outcome.Committed, "expected a commit, got $outcome")
    return outcome
}

class RendezvousModelTest {

    /**
     * The commit happens with both pipelines on the same frame, and that is the
     * only thing that makes the swap invisible. The successor is parked at the
     * meeting point from the moment it is seeked there; the incumbent plays all
     * the way to it without ever being stopped.
     */
    @Test
    fun aFastSeekMeetsTheIncumbentAtTheLead() {
        val sim = Sim(seekMs = 200, speedAt = { 1.0 })
        val outcome = committed(run(sim))
        assertEquals(0, outcome.reparks)
        assertEquals(RENDEZVOUS_LEAD_MS, outcome.filmMs)
        assertTrue(
            kotlin.math.abs(sim.successorFilmMs - outcome.filmMs) <= PREPARED_ALIGNMENT_SLACK_MS,
            "successor at ${sim.successorFilmMs}, commit at ${outcome.filmMs}",
        )
    }

    /**
     * A seek slower than the lead misses the first meeting point, which is what
     * the re-park is for. The second one is aimed using what this device's seek
     * actually cost rather than the device-class estimate that has just been
     * observed to be too short.
     */
    @Test
    fun aSlowSeekConvergesAfterOneRepark() {
        val sim = Sim(seekMs = 2_000, speedAt = { 1.0 })
        val outcome = committed(run(sim))
        assertEquals(1, outcome.reparks)
        assertTrue(
            kotlin.math.abs(sim.successorFilmMs - outcome.filmMs) <= PREPARED_ALIGNMENT_SLACK_MS,
            "successor at ${sim.successorFilmMs}, commit at ${outcome.filmMs}",
        )
    }

    /**
     * Film distance is fixed; wall distance is not. Half speed doubles the
     * wait and double speed halves it, and the meeting point is the same frame
     * in all three.
     */
    @Test
    fun theLeadIsFilmTimeAndTheWaitIsWallTime() {
        for (rate in listOf(0.5, 1.0, 2.0)) {
            val sim = Sim(seekMs = 200, speedAt = { rate })
            val outcome = committed(run(sim))
            assertEquals(RENDEZVOUS_LEAD_MS, outcome.filmMs, "rate=$rate")
            assertEquals(0, outcome.reparks, "rate=$rate")
            // Wall time to cover a fixed film distance scales with the rate.
            assertEquals(
                (RENDEZVOUS_LEAD_MS / rate).toLong(),
                outcome.wallMs,
                "rate=$rate",
            )
        }
    }

    /**
     * A viewer who pauses during the hold postpones the swap and nothing else.
     * The meeting point does not move, the successor stays parked on it, and
     * the wait resumes from wherever the playhead is when play does.
     */
    @Test
    fun aPauseDuringTheHoldPostponesTheSwap() {
        val resumeAtMs = 5_000L
        val sim = Sim(
            seekMs = 200,
            speedAt = { wall -> if (wall in 500 until resumeAtMs) 0.0 else 1.0 },
        )
        val outcome = committed(run(sim))
        assertEquals(RENDEZVOUS_LEAD_MS, outcome.filmMs)
        assertEquals(0, outcome.reparks)
        // 500 ms of film before the pause, 1 000 ms after it resumes.
        assertTrue(outcome.wallMs >= resumeAtMs, "swapped at ${outcome.wallMs}ms, before the resume")
        assertEquals(resumeAtMs + (RENDEZVOUS_LEAD_MS - 500), outcome.wallMs)
    }

    /**
     * An incumbent running far faster than the successor can be parked ahead of
     * outruns every meeting point. That is bounded rather than endless: two
     * misses is enough evidence, and the viewer's rung is then owed the
     * ordinary reopen.
     */
    @Test
    fun anIncumbentThatOutrunsTheLeadFailsAfterTwoReparks() {
        val sim = Sim(seekMs = 2_000, speedAt = { 8.0 })
        val outcome = run(sim)
        assertTrue(outcome is Outcome.Abandoned, "expected an abandon, got $outcome")
        assertEquals(RENDEZVOUS_MAX_REPARKS, outcome.reparks)
        assertEquals("rendezvous_missed", outcome.reason)
    }

    /**
     * The incumbent is never stopped, at any point, for any reason. The freeze
     * this replaces — `playWhenReady = false` on the pipeline the viewer is
     * watching, while the successor seeked onto its position — was a visible
     * pause on every prepared switch, which is the one thing the handoff exists
     * to avoid.
     */
    @Test
    fun theSuccessorWaitsAndTheIncumbentIsNeverAskedToStop() {
        val hold = RendezvousHold()
        val park = hold.park(nowMs = 0, incumbentFilmMs = 10_000)
        assertEquals(false, park.playWhenReady)
        assertEquals(10_000 + RENDEZVOUS_LEAD_MS, park.rendezvousFilmMs)
    }

    /** A hold that has not been parked yet asks to be parked. */
    @Test
    fun firingBeforeAParkParksInstead() {
        val hold = RendezvousHold()
        val step = hold.fire(
            nowMs = 0,
            incumbentFilmMs = 4_000,
            successorFilmMs = 0,
            successorReady = false,
            speed = 1.0,
        )
        assertTrue(step is RendezvousHold.Step.Repark, "got $step")
        assertEquals(4_000 + RENDEZVOUS_LEAD_MS, step.park.rendezvousFilmMs)
        assertEquals(0, hold.reparks)
    }

    /** A stopped incumbent never arrives on its own, so the hold looks again. */
    @Test
    fun aStoppedIncumbentGetsAPollRatherThanADivisionByZero() {
        val hold = RendezvousHold()
        hold.park(nowMs = 0, incumbentFilmMs = 0)
        assertEquals(RENDEZVOUS_PAUSED_POLL_MS, hold.delayMs(0, 0.0))
        assertEquals(RENDEZVOUS_PAUSED_POLL_MS, hold.delayMs(0, Double.NaN))
    }
}
