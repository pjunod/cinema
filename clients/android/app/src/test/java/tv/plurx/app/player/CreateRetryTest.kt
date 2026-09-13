package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.RefusalException

/**
 * M5 addition 1 — the create "not yet" retry, at this client's own seam.
 *
 * The ladder is pure arithmetic and is tested directly; the sequence around it
 * is [SessionCreateCoordinator.createRetryingNotYet] with an injected clock and
 * sleep, so the absolute deadline is exercised without a real minute passing.
 *
 * UNRUN: there is no Android toolchain on the machine this was written on. See
 * the PR's "Needs an Android toolchain or a device" list.
 */
class CreateRetryTest {
    @Test
    fun ladderIsOneSecondTwoSecondsFourSecondsAndThenSpent() {
        assertEquals(listOf(1_000, 2_000, 4_000), CreateRetry.backoffMs)
        assertEquals(60_000, CreateRetry.DEADLINE_MS)
        assertEquals(CreateRetryStep.Retry(1_000), createRetryStep(0, 0L, true))
        assertEquals(CreateRetryStep.Retry(2_000), createRetryStep(1, 1_000L, true))
        assertEquals(CreateRetryStep.Retry(4_000), createRetryStep(2, 3_000L, true))
        // Three rungs, not "4 s for ever".
        assertEquals(CreateRetryStep.Exhausted("ladder_spent"), createRetryStep(3, 7_000L, true))
        // The delay is a function of the attempt index, so the backoff cannot
        // restart: rung 1 answers 2 s however long the sequence has been going.
        assertEquals(CreateRetryStep.Retry(2_000), createRetryStep(1, 20_000L, true))
    }

    @Test
    fun deadlineIsAbsoluteAndNeverSchedulesARungPastItself() {
        assertEquals(CreateRetryStep.Exhausted("deadline"), createRetryStep(0, 60_000L, true))
        assertEquals(
            "a rung that could only START after the deadline is not scheduled",
            CreateRetryStep.Exhausted("deadline"),
            createRetryStep(0, 59_999L, true),
        )
        assertEquals(CreateRetryStep.Retry(1_000), createRetryStep(0, 58_999L, true))
        // A slow server spends the whole deadline on one attempt, which is the
        // point of measuring from the FIRST attempt rather than the last.
        assertEquals(CreateRetryStep.Exhausted("deadline"), createRetryStep(1, 90_000L, true))
    }

    @Test
    fun onlyANotYetAnswerIsRetriedAtAll() {
        assertEquals(CreateRetryStep.Fail, createRetryStep(0, 0L, false))
        assertEquals(
            setOf("startup_timeout", "media_owner_transition", "vod_index_pending", "vod_engine_unattested"),
            CreateRetry.codes,
        )
    }

    @Test
    fun theSequenceReplaysOneIdentityOnTheContractsLadder() = runBlocking {
        val clock = TestClock()
        val posts = mutableListOf<Pair<String?, Long>>()
        val retried = mutableListOf<String>()
        val exhausted = mutableListOf<String>()
        val coordinator = coordinator(clock) { body ->
            posts += body.request_id to clock.now
            throw stillBuilding("startup_timeout")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            isNotYet = { failure -> (failure as? RefusalException)?.code in CreateRetry.codes },
            onRetrying = { failure -> retried += failure.message ?: "" },
            onExhausted = { reason -> exhausted += reason },
        )
        assertNull("a spent sequence produces nothing to attach", outcome)
        assertEquals(4, posts.size)
        assertEquals(listOf(0L, 1_000L, 3_000L, 7_000L), posts.map { it.second })
        assertTrue("every attempt replays ONE identity", posts.all { it.first == "request" })
        assertEquals("one `preparing` fault for the sequence", 1, retried.size)
        assertEquals(listOf("ladder_spent"), exhausted)
    }

    @Test
    fun aSuccessOnARetryIsAttachedAndNothingIsReleased() = runBlocking {
        val clock = TestClock()
        val released = mutableListOf<String>()
        var calls = 0
        val coordinator = coordinator(clock, released) {
            calls += 1
            if (calls == 1) throw stillBuilding("vod_index_pending") else response("ready")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            isNotYet = { failure -> (failure as? RefusalException)?.code in CreateRetry.codes },
        )
        assertEquals("ready", outcome?.session_id)
        assertEquals(2, calls)
        assertTrue(released.isEmpty())
        assertEquals(1_000L, clock.now)
    }

    @Test
    fun aSuccessAfterTheAbsoluteDeadlineIsReleasedAndNeverAttached() = runBlocking {
        val clock = TestClock()
        val released = mutableListOf<String>()
        val exhausted = mutableListOf<String>()
        val entered = CompletableDeferred<Unit>()
        val finish = CompletableDeferred<Unit>()
        val coordinator = coordinator(clock, released) {
            entered.complete(Unit)
            finish.await()
            response("late")
        }
        val sequence = async {
            coordinator.createRetryingNotYet(
                body = body(),
                isNotYet = { true },
                onExhausted = { reason -> exhausted += reason },
            )
        }
        entered.await()
        // The watchdog is the only thing waiting on the clock, so releasing the
        // deadline releases it and nothing else.
        clock.release(CreateRetry.DEADLINE_MS.toLong())
        // One turn of the event loop for the watchdog to resume on.
        yield()
        assertEquals(listOf("deadline"), exhausted)
        finish.complete(Unit)
        assertNull("a late session is not attached", sequence.await())
        assertEquals(listOf("late"), released)
    }

    @Test
    fun aRefusalTheLadderDoesNotClaimIsRethrownUnchanged() = runBlocking {
        val clock = TestClock()
        var calls = 0
        val coordinator = coordinator(clock) {
            calls += 1
            throw stillBuilding("vod_disabled")
        }
        var thrown: Throwable? = null
        try {
            coordinator.createRetryingNotYet(
                body = body(),
                isNotYet = { failure -> (failure as? RefusalException)?.code in CreateRetry.codes },
            )
        } catch (failure: Throwable) {
            thrown = failure
        }
        assertEquals(1, calls)
        assertEquals("vod_disabled", (thrown as? RefusalException)?.code)
    }

    @Test
    fun aNewerIntentEndsTheSequenceWithoutRaisingAnything() = runBlocking {
        val clock = TestClock()
        val exhausted = mutableListOf<String>()
        var current = true
        var calls = 0
        val coordinator = coordinator(clock) {
            calls += 1
            current = false
            throw stillBuilding("startup_timeout")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            isCurrent = { current },
            isNotYet = { true },
            onExhausted = { reason -> exhausted += reason },
        )
        assertNull(outcome)
        assertEquals("a superseded sequence never posts again", 1, calls)
        assertTrue("a cancelled sequence is not an exhausted one", exhausted.isEmpty())
    }

    // ---- harness -------------------------------------------------------------

    /**
     * A clock the sequence's sleeps drive. Every `sleep(ms)` advances it and
     * returns immediately, except one the test holds with [release] — which is
     * how the deadline watchdog is made to fire while a create is still in
     * flight.
     */
    private class TestClock {
        var now: Long = 0L
            private set
        private val held = mutableMapOf<Long, CompletableDeferred<Unit>>()

        suspend fun sleep(ms: Long) {
            val gate = held[ms]
            if (gate != null) {
                gate.await()
                return
            }
            now += ms
        }

        /** Hold, then release, a sleep of exactly [ms]. Used for the deadline. */
        fun hold(ms: Long) {
            held[ms] = CompletableDeferred()
        }

        fun release(ms: Long) {
            now += ms
            held.remove(ms)?.complete(Unit)
        }
    }

    private fun coordinator(
        clock: TestClock,
        released: MutableList<String> = mutableListOf(),
        create: suspend (CreateSessionReq) -> HlsStart,
    ): SessionCreateCoordinator {
        clock.hold(CreateRetry.DEADLINE_MS.toLong())
        return SessionCreateCoordinator(
            createSession = create,
            isBadRequest = { false },
            freshRequestId = { "minted" },
            releaseSession = released::add,
            nowMs = { clock.now },
            sleep = { ms -> clock.sleep(ms) },
        )
    }

    private fun stillBuilding(code: String) =
        RefusalException(status = 503, code = code, message = "still building", positionMs = null)

    private fun body() = CreateSessionReq(playback_id = "playback", request_id = "request", start = 0.0)

    private fun response(id: String) =
        HlsStart(session_id = id, playlist_url = "/sessions/$id/master.m3u8")
}
