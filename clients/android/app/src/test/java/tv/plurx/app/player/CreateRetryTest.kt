package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.runTest
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
 * The ladder is pure arithmetic and is tested directly. The sequence around it
 * is [SessionCreateCoordinator.createRetryingNotYet] driven by `runTest`'s
 * virtual clock: the sleeps are `delay`, so a sixty-second deadline costs the
 * test nothing and is still exercised exactly, and the coordinator's one clock
 * seam is pointed at `TestScope.currentTime` so its arithmetic sees the same
 * time the scheduler does.
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
    fun theSequenceReplaysOneIdentityOnTheContractsLadder() = runTest {
        val posts = mutableListOf<Pair<String?, Long>>()
        val retried = mutableListOf<String>()
        val exhausted = mutableListOf<String>()
        val coordinator = coordinator(this) { body ->
            posts += body.request_id to currentTime
            throw stillBuilding("startup_timeout")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            startContext = true,
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
    fun aSuccessOnARetryIsAttachedAndNothingIsReleased() = runTest {
        val released = mutableListOf<String>()
        var calls = 0
        val coordinator = coordinator(this, released) {
            calls += 1
            if (calls == 1) throw stillBuilding("vod_index_pending") else response("ready")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            startContext = true,
            isNotYet = { failure -> (failure as? RefusalException)?.code in CreateRetry.codes },
        )
        assertEquals("ready", outcome?.session_id)
        assertEquals(2, calls)
        assertTrue(released.isEmpty())
        assertEquals("one rung, one second", 1_000L, currentTime)
    }

    @Test
    fun aSuccessAfterTheAbsoluteDeadlineIsReleasedAndNeverAttached() = runTest {
        val released = mutableListOf<String>()
        val exhausted = mutableListOf<String>()
        val hold = CompletableDeferred<Unit>()
        val coordinator = coordinator(this, released) {
            hold.await()
            response("late")
        }
        val sequence = async {
            coordinator.createRetryingNotYet(
                body = body(),
                startContext = true,
                isNotYet = { true },
                onExhausted = { reason -> exhausted += reason },
            )
        }
        // Virtual time runs to the watchdog and no further: the create is still
        // in flight, so the deadline is the only thing pending.
        advanceUntilIdle()
        assertEquals(
            "the prompt lands on the clock, not when the slow create returns",
            listOf("deadline"),
            exhausted,
        )
        assertEquals(CreateRetry.DEADLINE_MS.toLong(), currentTime)
        hold.complete(Unit)
        assertNull("a late session is not attached", sequence.await())
        assertEquals(listOf("late"), released)
        assertEquals("releasing it does not raise a second prompt", 1, exhausted.size)
    }

    @Test
    fun aRefusalTheLadderDoesNotClaimIsRethrownUnchanged() = runTest {
        var calls = 0
        val coordinator = coordinator(this) {
            calls += 1
            throw stillBuilding("vod_disabled")
        }
        var thrown: Throwable? = null
        try {
            coordinator.createRetryingNotYet(
                body = body(),
                startContext = true,
                isNotYet = { failure -> (failure as? RefusalException)?.code in CreateRetry.codes },
            )
        } catch (failure: Throwable) {
            thrown = failure
        }
        assertEquals(1, calls)
        assertEquals("vod_disabled", (thrown as? RefusalException)?.code)
    }

    @Test
    fun aNewerIntentEndsTheSequenceWithoutRaisingAnything() = runTest {
        val exhausted = mutableListOf<String>()
        var current = true
        var calls = 0
        val coordinator = coordinator(this) {
            calls += 1
            current = false
            throw stillBuilding("startup_timeout")
        }
        val outcome = coordinator.createRetryingNotYet(
            body = body(),
            startContext = true,
            isCurrent = { current },
            isNotYet = { true },
            onExhausted = { reason -> exhausted += reason },
        )
        assertNull(outcome)
        assertEquals("a superseded sequence never posts again", 1, calls)
        assertTrue("a cancelled sequence is not an exhausted one", exhausted.isEmpty())
    }

    // ---- the change context arms nothing at all ------------------------------

    @Test
    fun aChangeContextCreateIsNotRetriedAndArmsNoWatchdogAtAll() = runTest {
        // The blocker this case exists for. `openSession` is also the create
        // path for seeks and quality changes, and arming a sixty-second
        // watchdog there would stop a player the viewer is watching and release
        // the session the change was going to attach — §7 recipe (b)'s
        // explicitly not-allowed outcome. This client's own read timeout is
        // sixty seconds, so a change against a cold NAS races that boundary.
        val released = mutableListOf<String>()
        val exhausted = mutableListOf<String>()
        val retried = mutableListOf<String>()
        val hold = CompletableDeferred<Unit>()
        val coordinator = coordinator(this, released) {
            hold.await()
            response("the-change")
        }
        val sequence = async {
            coordinator.createRetryingNotYet(
                body = body(),
                startContext = false,
                isNotYet = { true },
                onRetrying = { failure -> retried += failure.message ?: "" },
                onExhausted = { reason -> exhausted += reason },
            )
        }
        advanceUntilIdle()
        assertTrue("a change context arms no deadline watchdog", exhausted.isEmpty())
        assertEquals("and nothing is waiting on a clock at all", 0L, currentTime)
        hold.complete(Unit)
        assertEquals("the change attaches its session", "the-change", sequence.await()?.session_id)
        assertTrue("and nothing is released", released.isEmpty())
        assertTrue(retried.isEmpty())
    }

    @Test
    fun aChangeContextRefusalIsRethrownRatherThanRetried() = runTest {
        var calls = 0
        val coordinator = coordinator(this) {
            calls += 1
            throw stillBuilding("startup_timeout")
        }
        var thrown: Throwable? = null
        try {
            coordinator.createRetryingNotYet(
                body = body(),
                startContext = false,
                isNotYet = { true },
            )
        } catch (failure: Throwable) {
            thrown = failure
        }
        assertEquals("row 7 beats row 6: a refused change is not retried", 1, calls)
        assertEquals("startup_timeout", (thrown as? RefusalException)?.code)
        assertEquals(0L, currentTime)
    }

    // ---- harness -------------------------------------------------------------

    /**
     * The coordinator with its one clock seam pointed at the test scheduler, so
     * the ladder's arithmetic and the scheduler's virtual time agree.
     */
    private fun coordinator(
        scope: TestScope,
        released: MutableList<String> = mutableListOf(),
        create: suspend (CreateSessionReq) -> HlsStart,
    ): SessionCreateCoordinator = SessionCreateCoordinator(
        createSession = create,
        isBadRequest = { false },
        freshRequestId = { "minted" },
        releaseSession = released::add,
        nowMs = { scope.currentTime },
    )

    private fun stillBuilding(code: String) =
        RefusalException(status = 503, code = code, message = "still building", positionMs = null)

    private fun body() = CreateSessionReq(playback_id = "playback", request_id = "request", start = 0.0)

    private fun response(id: String) =
        HlsStart(session_id = id, playlist_url = "/sessions/$id/master.m3u8")
}
