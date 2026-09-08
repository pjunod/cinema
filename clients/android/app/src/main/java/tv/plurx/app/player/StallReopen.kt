package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
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
