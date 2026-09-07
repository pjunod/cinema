package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.PlaybackQuality

/** The same transport transition, asynchronous create, and final attachment
 * boundary used by Controller. Only the server and Media3 mutations are fake. */
class PlaybackRequestOwnershipTest {
    @Test
    fun delayedRecipeCreateSurvivesPauseAndAttachesPausedBeforeResume() = runBlocking {
        for (stall in listOf(false, true)) {
            val budget = StallReopenBudget()
            budget.seed(720)
            budget.record(720)
            val guard = ControllerStallGuard(budget)
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val request = guard.beginRequest()
            val observation = guard.observeStall()
            val entered = CompletableDeferred<Unit>()
            val finish = CompletableDeferred<Unit>()
            val released = mutableListOf<String>()
            val attachments = mutableListOf<Pair<String, Boolean>>()
            var playerRequested = true
            val coordinator = SessionCreateCoordinator(
                createSession = {
                    entered.complete(Unit)
                    finish.await()
                    response("desired-recipe")
                },
                isBadRequest = { false },
                freshRequestId = { "unused" },
                releaseSession = released::add,
            )
            val create = async {
                try {
                    val result = if (stall) coordinator.reopenAfterStall(body()) { guard.isCurrent(request) }
                        else coordinator.create(body()) { guard.isCurrent(request) }
                    if (result != null) coordinator.attachIfCurrent(result, { guard.isCurrent(request) }) {
                        attachments += it.session_id to intent.playbackRequested
                        playerRequested = intent.playbackRequested
                    }
                } finally {
                    guard.finishRequest(request)
                }
            }
            entered.await()
            guard.setPlaybackRequested(intent, false) { playerRequested = it }
            assertFalse(playerRequested)
            assertFalse("pre-pause stall evidence is stale", guard.isCurrent(observation))
            assertTrue("Pause retains the media request", guard.isCurrent(request))
            finish.complete(Unit)
            create.await()
            assertEquals(listOf("desired-recipe" to false), attachments)
            assertTrue(released.isEmpty())
            guard.setPlaybackRequested(intent, true) { playerRequested = it }
            assertTrue(playerRequested)
            assertFalse("Pause then Resume cannot revive old stall evidence", guard.isCurrent(observation))
            assertEquals("transport cannot reset the recovery budget", 1, budget.nonDowngradeCount)
            assertEquals(0, budget.resetCount)
            assertFalse(guard.defersPredecessorRecovery(false))
        }
    }

    @Test
    fun staleNormalAndFallbackSuccessReleaseTheReturnedResourceExactlyOnce() = runBlocking {
        for (route in listOf("create", "stall", "fallback")) {
            val guard = ControllerStallGuard(StallReopenBudget())
            val request = guard.beginRequest()
            val entered = CompletableDeferred<Unit>()
            val finish = CompletableDeferred<Unit>()
            val released = mutableListOf<String>()
            var calls = 0
            val coordinator = SessionCreateCoordinator(
                createSession = {
                    calls++
                    if (route == "fallback" && calls == 1) throw BadRequest()
                    entered.complete(Unit)
                    finish.await()
                    response("stale-$route")
                },
                isBadRequest = { it is BadRequest },
                freshRequestId = { "fallback" },
                releaseSession = released::add,
            )
            val create = async {
                if (route == "create") coordinator.create(body()) { guard.isCurrent(request) }
                else coordinator.reopenAfterStall(body()) { guard.isCurrent(request) }
            }
            entered.await()
            guard.invalidateForUserAction()
            finish.complete(Unit)
            assertNull(create.await())
            assertEquals(listOf("stale-$route"), released)
            assertEquals(if (route == "fallback") 2 else 1, calls)
        }
    }

    @Test
    fun requestInvalidatedAfterCreateBeforeAttachmentIsReleasedWithoutMutation() = runBlocking {
        val guard = ControllerStallGuard(StallReopenBudget())
        val request = guard.beginRequest()
        val released = mutableListOf<String>()
        val coordinator = SessionCreateCoordinator(
            createSession = { response("late") },
            isBadRequest = { false },
            freshRequestId = { "unused" },
            releaseSession = released::add,
        )
        val result = coordinator.create(body()) { guard.isCurrent(request) }!!
        guard.invalidateForUserAction()
        var attached = false
        coordinator.attachIfCurrent(result, { guard.isCurrent(request) }) { attached = true }
        assertFalse(attached)
        assertEquals(listOf("late"), released)
    }

    @Test
    fun predecessorErrorCannotStealPendingRecipeOrSameRecipeSeekCreate() = runBlocking {
        for (changedRecipe in listOf(false, true)) {
            val guard = ControllerStallGuard(StallReopenBudget())
            val entered = CompletableDeferred<Unit>()
            val finish = CompletableDeferred<Unit>()
            val request = guard.beginRequest()
            val coordinator = SessionCreateCoordinator(
                createSession = {
                    entered.complete(Unit)
                    finish.await()
                    response("newest")
                },
                isBadRequest = { false },
                freshRequestId = { "unused" },
                releaseSession = { error("newest request must not be released") },
            )
            var attached: String? = null
            val create = async {
                try {
                    coordinator.create(body()) { guard.isCurrent(request) }?.let { result ->
                        coordinator.attachIfCurrent(result, { guard.isCurrent(request) }) { attached = it.session_id }
                    }
                } finally {
                    guard.finishRequest(request)
                }
            }
            entered.await()
            // Controller checks this before reporting or failing over a
            // departing item's error, including before media has changed.
            assertTrue(guard.defersPredecessorRecovery(changedRecipe))
            if (!guard.defersPredecessorRecovery(changedRecipe)) guard.invalidateForPlaybackAttempt()
            finish.complete(Unit)
            create.await()
            assertEquals("newest", attached)
            assertFalse(guard.defersPredecessorRecovery(false))
        }
    }

    @Test
    fun transportLocalSelectionAndVisibilityRearmAnAskSuspendedAcrossTheWholeChange() = runBlocking {
        for (action in listOf("pause-resume", "local-subtitle", "background-foreground")) {
            val tracker = OpenPlaybackStallTracker()
            val budget = StallReopenBudget()
            budget.seed(720)
            budget.record(720)
            val guard = ControllerStallGuard(budget, tracker)
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            assertNull(tracker.sample(true, false, true, 10_000, 0))
            assertTrue(tracker.sample(true, false, true, 10_000, 8_000) != null)
            val observation = guard.observeStall()
            val entered = CompletableDeferred<Unit>()
            val finish = CompletableDeferred<Unit>()
            val ask = async {
                entered.complete(Unit)
                finish.await()
                guard.isCurrent(observation)
            }
            entered.await()
            when (action) {
                "pause-resume" -> {
                    guard.setPlaybackRequested(intent, false) {}
                    guard.setPlaybackRequested(intent, true) {}
                }
                "local-subtitle" -> guard.invalidateForUserAction()
                else -> {
                    guard.invalidateObservation()
                    guard.invalidateObservation()
                }
            }
            finish.complete(Unit)
            assertFalse(ask.await())
            // Neither transition was sampled: the exact synchronous guard
            // must have cleared the fired latch even for same-item actions.
            assertNull(tracker.sample(true, false, true, 10_000, 9_000))
            assertTrue("$action must regain bounded recovery", tracker.sample(true, false, true, 10_000, 17_000) != null)
            if (action != "local-subtitle") assertEquals(1, budget.nonDowngradeCount)
        }
    }

    @Test
    fun staleRequestCompletionCannotClearANewerCreatesOwnership() {
        val guard = ControllerStallGuard(StallReopenBudget())
        val first = guard.beginRequest()
        val observation = guard.observeStall()
        val second = guard.beginRequest()
        assertFalse(guard.isCurrent(observation))
        guard.finishRequest(first)
        assertTrue(guard.defersPredecessorRecovery(false))
        guard.finishRequest(second)
        assertFalse(guard.defersPredecessorRecovery(false))
        assertTrue("an unpublished recipe also owns its pending replacement", guard.defersPredecessorRecovery(true))
    }

    private fun body() = CreateSessionReq(playback_id = "playback", request_id = "request", start = 4.0)
    private fun response(id: String) = HlsStart(session_id = id, playlist_url = "/sessions/$id/master.m3u8")
    private class BadRequest : RuntimeException()
}
