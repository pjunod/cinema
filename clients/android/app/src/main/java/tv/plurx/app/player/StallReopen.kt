package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.HlsStart

/** Client-owned bound for repeated stall reopens at one resolved rung. */
internal class StallReopenBudget(private val maxNonDowngrades: Int = 3) {
    var predecessorHeight: Int? = null
    /** Number of times [reset] has been called. Cumulative, never decremented,
     *  used by [Controller] for user-action budget resets and guarded by tests. */
    var resetCount: Int = 0
        private set
    var nonDowngradeCount: Int = 0
        private set
    /**
     * Monotonically increasing sequence advanced by every user-initiated
     * restart (VOD seek, subtitle/audio switch, quality change). Each
     * increment invalidates any in-flight stall reopen from the prior
     * cycle. Read by [Controller] for its [isCurrent] guard and by tests
     * that assert production-linked invalidation.
     */
    var userActionSequence: Long = 0L
        private set

    fun canReopen(): Boolean = nonDowngradeCount < maxNonDowngrades

    fun seed(height: Int?) {
        predecessorHeight = height
    }

    fun record(resolvedHeight: Int?) {
        nonDowngradeCount = if (isStrictDowngrade(predecessorHeight, resolvedHeight)) {
            0
        } else {
            nonDowngradeCount + 1
        }
        predecessorHeight = resolvedHeight
    }

    fun reset() {
        predecessorHeight = null
        nonDowngradeCount = 0
        resetCount++
    }

    /**
     * Production seam that both [Controller] and tests use to advance the
     * user-action sequence and reset the stall budget atomically.  Every
     * user-initiated playback restart (VOD seek, subtitle/audio switch,
     * quality change, leaveSessionPlayback) goes through this method so
     * reverting any one of those Controller paths changes the returned
     * sequence number.
     */
    fun resetForUserAction(): Long {
        reset()
        return ++userActionSequence
    }

    companion object {
        fun isStrictDowngrade(predecessorHeight: Int?, resolvedHeight: Int?): Boolean =
            resolvedHeight != null && resolvedHeight > 0 &&
                predecessorHeight != null && resolvedHeight < predecessorHeight
    }
}

/**
 * Owns the controller's monotonically increasing session-request token.
 *
 * The budget sequence is deliberately not the request token: session opens and
 * stall reopens also consume tokens, so copying the smaller budget sequence can
 * make an old token current again.  The path-specific helpers keep the VOD-seek
 * and viewer-seek action directly regression-testable.
 */
internal class ControllerStallGuard(
    private val budget: StallReopenBudget,
    private val stallTracker: OpenPlaybackStallTracker = OpenPlaybackStallTracker(),
) {
    private var requestVersion = 0L
    private var transportVersion = 0L
    private var pendingRequest: Long? = null

    data class Observation(val request: Long, val transport: Long)

    /** Observing a stall must not revoke a viewer's in-flight media create. */
    fun observeStall(): Observation = Observation(requestVersion, transportVersion)

    fun isCurrent(observation: Observation): Boolean =
        observation.request == requestVersion && observation.transport == transportVersion

    /** Pause cancels old stall evidence, not the requested media recipe. */
    fun setPlaybackRequested(intent: PlaybackIntent, requested: Boolean, apply: (Boolean) -> Unit) {
        invalidateObservation()
        intent.setPlaybackRequested(requested)
        apply(intent.playbackRequested)
    }

    /** The sampling coroutine can be suspended in an ask while visibility or
     * transport changes twice. Rearm synchronously; sampling may miss both. */
    fun invalidateObservation() {
        transportVersion++
        stallTracker.reset()
    }

    fun beginRequest(): Long = (++requestVersion).also { pendingRequest = it }

    fun finishRequest(version: Long) {
        if (pendingRequest == version) {
            pendingRequest = null
            if (isCurrent(version)) stallTracker.reset()
        }
    }

    fun defersPredecessorRecovery(recipeReplacementPending: Boolean): Boolean =
        recipeReplacementPending || pendingRequest == requestVersion

    fun isCurrent(version: Long): Boolean = version == requestVersion

    fun invalidateForUserAction() {
        budget.resetForUserAction()
        requestVersion++
        stallTracker.reset()
    }

    /** Invalidate stale recovery ownership without resetting its retry budget. */
    fun invalidateForPlaybackAttempt() {
        requestVersion++
        stallTracker.reset()
    }

    fun viewerSeek(action: () -> Unit) {
        invalidateForUserAction()
        action()
    }
}

/**
 * Serializes every server-mutating create for one playback id. In particular,
 * a user restart that invalidates a stall while its 400 fallback is in flight
 * is sent after that fallback, so the newer user action remains the server's
 * final replacement as well as the controller's final adopted response.
 *
 * Note: [Mutex] is not reentrant. Internal helpers that already hold the lock
 * must call [callCreate] directly rather than going through [create].
 */
internal class SessionCreateCoordinator(
    private val createSession: suspend (CreateSessionReq) -> HlsStart,
    private val isBadRequest: (Throwable) -> Boolean,
    private val freshRequestId: () -> String,
    private val releaseSession: (String) -> Unit,
    // M5's retry needs a clock and a sleep. Injected so the ladder and its
    // absolute deadline are testable on the JVM without a real minute passing.
    private val nowMs: () -> Long = { System.nanoTime() / 1_000_000L },
    private val sleep: suspend (Long) -> Unit = { delay(it) },
) {
    private val createMutex = Mutex()

    suspend fun create(
        body: CreateSessionReq,
        isCurrent: () -> Boolean = { true },
    ): HlsStart? = createMutex.withLock {
        if (isCurrent()) retainIfCurrent(callCreate(body), isCurrent) else null
    }

    private fun retainIfCurrent(hls: HlsStart, isCurrent: () -> Boolean): HlsStart? =
        if (isCurrent()) hls else {
            releaseSession(hls.session_id)
            null
        }

    /** The last ownership check and attachment mutation share one turn. */
    fun attachIfCurrent(hls: HlsStart, isCurrent: () -> Boolean, attach: (HlsStart) -> Unit) {
        retainIfCurrent(hls, isCurrent)?.let(attach)
    }

    /**
     * Invoke the raw [create] lambda without re-entering [createMutex].
     * Must only be called from inside [createMutex.withLock].
     */
    private suspend fun callCreate(body: CreateSessionReq): HlsStart {
        try {
            return createSession(body)
        } catch (cancelled: CancellationException) {
            throw cancelled
        }
    }

    /**
     * M5 addition 1 — the create "not yet" retry
     * (PLAYBACK-SURFACE-CONTRACT.md §3.3 row 6, implementation plan §4.6).
     *
     * Re-post the SAME create, under the SAME request identity, while the
     * server says it is still building this stream. Three properties the
     * review asked for, and each is a line below:
     *
     * * the deadline is ABSOLUTE and runs on a watchdog rather than between
     *   attempts, so a server that holds every create for three minutes cannot
     *   stretch the sequence;
     * * a success that lands AFTER the deadline is released, never attached —
     *   the viewer has already been told this attempt is over, and an encoder
     *   nobody is watching is a hardware slot held for nobody;
     * * a newer intent ends it: [isCurrent] is re-asked before every attempt,
     *   after every failure, and the sleep between attempts is cancelled with
     *   the coroutine that is waiting on it.
     *
     * `null` means "this sequence produced nothing you should attach", and it
     * is NOT a failure the caller should surface: either a newer intent owns
     * the surface now, or [onExhausted] has already raised the prompt. A
     * refusal the ladder does not claim is rethrown, so the caller's existing
     * handling is untouched.
     *
     * The delays happen OUTSIDE the mutex on purpose: a sequence sleeping four
     * seconds must not hold the create lock a newer user action needs.
     */
    suspend fun createRetryingNotYet(
        body: CreateSessionReq,
        isCurrent: () -> Boolean = { true },
        isNotYet: (Throwable) -> Boolean,
        onRetrying: (Throwable) -> Unit = {},
        onExhausted: (String) -> Unit = {},
    ): HlsStart? = coroutineScope {
        val began = nowMs()
        var expired = false
        // ONE identity for the whole sequence: the server persists a create's
        // answer under `request_id`, so replaying one recovers the session it
        // already made instead of spawning a second encoder — which is what
        // makes retrying a create safe at all.
        val request =
            if (body.request_id.isNullOrBlank()) body.copy(request_id = freshRequestId()) else body
        val watchdog = launch {
            sleep(CreateRetry.DEADLINE_MS.toLong())
            if (!expired) {
                expired = true
                if (isCurrent()) onExhausted("deadline")
            }
        }
        var outcome: HlsStart? = null
        try {
            var attempt = 0
            attempts@ while (true) {
                if (!isCurrent() || expired) break@attempts
                var started: HlsStart? = null
                try {
                    started = createMutex.withLock {
                        if (isCurrent()) retainIfCurrent(callCreate(request), isCurrent) else null
                    }
                } catch (cancelled: CancellationException) {
                    throw cancelled
                } catch (failure: Throwable) {
                    if (!isCurrent() || expired) break@attempts
                    when (val step = createRetryStep(attempt, nowMs() - began, isNotYet(failure))) {
                        is CreateRetryStep.Fail -> throw failure
                        is CreateRetryStep.Exhausted -> {
                            onExhausted(step.reason)
                            break@attempts
                        }
                        is CreateRetryStep.Retry -> {
                            // ONE `preparing` fault for the sequence, not one
                            // per attempt: a viewer watching a spinner does not
                            // need four identical rows behind it.
                            if (attempt == 0) onRetrying(failure)
                            sleep(step.delayMs.toLong())
                            attempt += 1
                            continue@attempts
                        }
                    }
                }
                // A session the sequence no longer owns belongs to nobody.
                if (expired) {
                    started?.session_id?.let(releaseSession)
                    break@attempts
                }
                outcome = started
                break@attempts
            }
        } finally {
            watchdog.cancel()
        }
        outcome
    }

    suspend fun reopenAfterStall(
        body: CreateSessionReq,
        isCurrent: () -> Boolean = { true },
    ): HlsStart? =
        createMutex.withLock {
            if (!isCurrent()) return@withLock null
            try {
                retainIfCurrent(callCreate(body), isCurrent)
            } catch (failure: Throwable) {
                if (!isBadRequest(failure)) throw failure
                if (!isCurrent()) return@withLock null
                retainIfCurrent(
                    callCreate(
                        body.copy(
                            request_id = freshRequestId(),
                            previous_session_id = null,
                            reopen_reason = null,
                        ),
                    ),
                    isCurrent,
                )
            }
        }
}
