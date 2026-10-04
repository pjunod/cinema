package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import tv.plurx.app.data.SharedBoundSession
import tv.plurx.app.data.SharedControlChannel
import tv.plurx.app.data.SharedControlOutcome
import tv.plurx.app.data.SharedControlState
import tv.plurx.app.data.SharedDecisionClient
import tv.plurx.app.data.SharedPlaybackPlan
import tv.plurx.app.data.SharedPlaybackSubject
import tv.plurx.app.data.SharedProgressResult
import tv.plurx.app.data.SharedSelection
import tv.plurx.app.data.SharedStartedDirect
import tv.plurx.app.data.SharedStartedPlayback
import tv.plurx.app.data.sharedControlCapabilities
import tv.plurx.app.data.sharedPlaybackPlan
import tv.plurx.app.data.frozenControlSelection
import java.util.UUID

/** The renderer half of a Shared player. Only the owner below changes it. */
internal interface SharedRenderer {
    /** Replace the current media with a Shared HLS playlist (account-bearing transport). */
    fun attachHls(url: String, positionMs: Long, playWhenReady: Boolean)
    /** Replace the current media with B's direct byte URL (no account headers). */
    fun attachDirect(url: String, positionMs: Long, playWhenReady: Boolean)
    fun seekTo(positionMs: Long)
    fun setPlaying(playing: Boolean)
    fun snapshot(): SharedRendererSnapshot
    fun release()
}

internal data class SharedRendererSnapshot(
    val positionMs: Long, val bufferedMs: Long, val durationMs: Long?,
    val playing: Boolean, val renderState: RenderState, val playbackRate: Double = 1.0,
)

/**
 * One Shared playback, from the first Start to the last DELETE.
 *
 * Every viewer command runs under [commands], so a seek, a pause and a quality
 * change reach B in the order they were asked and never interleave with a
 * reopen. Shared HLS changes the renderer only after B accepts the exchange
 * that asked for it. A directed change (quality, audio, subtitle) is an ask B
 * declines with `preparation: none`, since a Shared session is never replaced
 * in place: the owner then makes a fresh Start at the sampled position with the
 * new selection, attaches it, and only then DELETEs the predecessor. Direct
 * play has no control route; its renderer is local, and a byte URL B retired
 * after a long pause earns exactly one fresh direct Start per attachment that
 * reached its timeline.
 *
 * Owned work only: every coroutine here is a child of [job], which [stop]
 * cancels and joins before the renderer is released and the session ended.
 */
internal class SharedPlaybackOwner(
    parent: CoroutineScope,
    clientFactory: () -> SharedDecisionClient,
    private val renderer: SharedRenderer,
) {
    /** Captured at the first Start, under the login that opened the player. */
    private val client by lazy(LazyThreadSafetyMode.NONE, clientFactory)
    val failure = MutableStateFlow<String?>(null)
    val starting = MutableStateFlow(false)
    val statusSummary = MutableStateFlow<String?>(null)
    val selection = MutableStateFlow<SharedSelection?>(null)
    val direct = MutableStateFlow(false)

    private val job = SupervisorJob(parent.coroutineContext[Job])
    private val scope = CoroutineScope(parent.coroutineContext + job)
    private val commands = Mutex()
    /** One identity per player, across reopens: the same viewer continuing. */
    private val clientInstanceId = UUID.randomUUID().toString()
    private var plan: SharedPlaybackPlan? = null
    private var session: SharedBoundSession? = null
    private var channel: SharedControlChannel? = null
    private var sessionEnded = false
    private var directRestartEarned = false
    private var closing = false

    val currentSession: SharedBoundSession? get() = session
    val currentPlan: SharedPlaybackPlan? get() = plan

    /** Run [block] as owned work. Each command takes [commands] itself, and the
     * mutex is fair, so launches reach B in the order they were made. */
    fun launch(block: suspend SharedPlaybackOwner.() -> Unit): Job = scope.launch {
        if (!closing) runCatching { block() }.onFailure { report(it) }
    }

    /** First Start, then the owned cadence. */
    fun begin(initial: SharedPlaybackPlan): Job = launch { start(initial); startCadence() }

    private fun report(error: Throwable) {
        if (error is kotlinx.coroutines.CancellationException) throw error
        if (!closing) failure.value = error.message ?: "Shared playback failed"
    }

    suspend fun start(initial: SharedPlaybackPlan) = commands.withLock {
        check(plan == null && !closing)
        starting.value = true
        try { attach(initial, initial.subject.resumeMs, playWhenReady = true) } finally { starting.value = false }
    }

    /** Start the owned exchange cadence: control renewals at B's own interval,
     * progress and status every second tick. A tick that finds a command
     * running skips rather than queueing behind it. */
    fun startCadence(intervalMs: Long = 5_000L): Job = scope.launch {
        var tick = 0L
        while (isActive && !closing) {
            delay(intervalMs)
            if (!commands.tryLock()) continue
            try { tick(++tick % 2 == 0L) } catch (error: Throwable) {
                if (error is kotlinx.coroutines.CancellationException) throw error
            } finally { commands.unlock() }
        }
    }

    /** One cadence exchange; public for tests. Caller holds no lock. */
    suspend fun tickNow(progress: Boolean) = commands.withLock { tick(progress) }

    private suspend fun tick(progress: Boolean) {
        val current = session ?: return
        val plan = plan ?: return
        val view = renderer.snapshot()
        channel?.takeIf { !sessionEnded }?.let { control ->
            val outcome = control.exchange(state(view), plan.frozenControlSelection())
            if (outcome is SharedControlOutcome.Ended) sessionEnded = true
        }
        if (!progress) return
        runCatching { client.orderedProgress(current, plan.subject.watchSequence, view.positionMs.coerceAtLeast(0), durationOf(current, view)) }
        if (current is SharedStartedPlayback && !sessionEnded) {
            val status = runCatching { client.status(current) }.getOrNull()
            // Bound to the retained playback: an answer for a session this
            // owner has since replaced is not this player's status.
            if (status != null && status.sessionId == session?.sessionId) statusSummary.value = status.summary
        }
    }

    suspend fun seek(targetMs: Long) = commands.withLock {
        val view = renderer.snapshot()
        val target = targetMs.coerceAtLeast(0)
        when (val outcome = control(state(view).copy(renderState = RenderState.SEEKING, seekTargetMs = target))) {
            null, is SharedControlOutcome.Accepted -> renderer.seekTo(target)
            is SharedControlOutcome.Ended -> reopen(requireNotNull(plan).selection, target, view.playing, allowDirect = false)
            else -> refused(outcome)
        }
    }

    suspend fun setPlaying(playing: Boolean) = commands.withLock {
        val view = renderer.snapshot()
        val demand = if (playing) PlaybackDemand.ACTIVE else PlaybackDemand.HOLD
        when (val outcome = control(state(view).copy(demand = demand))) {
            null, is SharedControlOutcome.Accepted -> renderer.setPlaying(playing)
            is SharedControlOutcome.Ended -> if (playing) reopen(requireNotNull(plan).selection, view.positionMs, true, allowDirect = false) else renderer.setPlaying(false)
            else -> refused(outcome)
        }
    }

    /** A directed quality, audio or subtitle change. */
    suspend fun change(next: SharedSelection) = commands.withLock {
        val current = requireNotNull(plan)
        if (next == current.selection) return@withLock
        val view = renderer.snapshot()
        val control = channel
        if (control == null || sessionEnded) {
            reopen(next, view.positionMs, view.playing, allowDirect = false); return@withLock
        }
        when (val outcome = control.exchange(state(view), next.controlSelection())) {
            is SharedControlOutcome.Accepted ->
                // B carries no Source successor, so it answers every evaluated
                // preparation as `none`; absence is an older relay saying the
                // same thing. Either way nothing is being built: reopen now.
                if (outcome.preparation == null || outcome.preparation == "none") reopen(next, view.positionMs, view.playing, allowDirect = false)
                else failure.value = "This shared playback change is not available."
            is SharedControlOutcome.Ended -> { sessionEnded = true; reopen(next, view.positionMs, view.playing, allowDirect = false) }
            else -> refused(outcome)
        }
    }

    /** The renderer could not read its media. Returns the owned restart, if one was earned. */
    fun rendererFailed(httpStatus: Int?, message: String?): Job? {
        val current = session
        if (current is SharedStartedDirect && (httpStatus == 404 || httpStatus == 410) && directRestartEarned) {
            // B retires a direct session after 300 s with no byte request. The
            // allowance is spent here and re-earned only when the new
            // attachment reaches its own timeline: no timer, no loop.
            directRestartEarned = false
            return launch {
                commands.withLock {
                    val view = renderer.snapshot()
                    reopen(requireNotNull(plan).selection, view.positionMs, playWhenReady = true, allowDirect = true)
                }
            }
        }
        if (!closing) failure.value = message ?: "Shared playback failed"
        return null
    }

    /** The current attachment reached its timeline. */
    fun timelineReached() { if (session is SharedStartedDirect) directRestartEarned = true }

    private fun refused(outcome: SharedControlOutcome) {
        failure.value = when (outcome) {
            is SharedControlOutcome.Refused -> "The server refused this playback change (${outcome.code ?: outcome.status})."
            is SharedControlOutcome.Unavailable -> "The server could not take this playback change right now."
            else -> "Shared playback failed"
        }
    }

    private suspend fun control(state: SharedControlState): SharedControlOutcome? {
        val control = channel ?: return null
        if (sessionEnded) return SharedControlOutcome.Ended(null)
        val outcome = control.exchange(state, requireNotNull(plan).frozenControlSelection())
        if (outcome is SharedControlOutcome.Ended) sessionEnded = true
        return outcome
    }

    private fun state(view: SharedRendererSnapshot) = SharedControlState(
        demand = if (view.playing) PlaybackDemand.ACTIVE else PlaybackDemand.HOLD,
        positionMs = view.positionMs.coerceAtLeast(0), bufferedThroughMs = view.bufferedMs.coerceAtLeast(0),
        renderState = view.renderState, playbackRate = view.playbackRate.takeIf { it in 0.25..4.0 } ?: 1.0,
    )

    private fun durationOf(current: SharedBoundSession, view: SharedRendererSnapshot): Long? =
        (current as? SharedStartedPlayback)?.start?.response?.duration_ms ?: view.durationMs?.takeIf { it >= 0 }

    /** A fresh Start of the same file under the accepted login, with no lineage. */
    private suspend fun reopen(next: SharedSelection, positionMs: Long, playWhenReady: Boolean, allowDirect: Boolean) {
        val current = requireNotNull(plan)
        val subject = SharedPlaybackSubject(current.subject.context, current.subject.title, positionMs.coerceAtLeast(0), current.subject.watchSequence)
        // The same ask keeps its decision (a retired session, an expired direct
        // play); a changed ask is a new question for the Source.
        val result = if (next == current.selection) SharedDecisionClient.Result(current.decision, current.caps)
            else client.redecide(current.subject.context, current.caps, next.decisionQuery())
        val replacement = sharedPlaybackPlan(subject, result, next, current.request.playback_id, UUID.randomUUID().toString(), allowDirect)
        attach(replacement, subject.resumeMs, playWhenReady)
    }

    /** Start, attach, then release the predecessor: make before break. */
    private suspend fun attach(next: SharedPlaybackPlan, positionMs: Long, playWhenReady: Boolean) {
        val previous = session
        var started: SharedBoundSession
        if (next.direct) {
            val direct = client.startDirect(next.subject.context, next.request)
            if (direct.playable) {
                started = direct
                renderer.attachDirect(client.directUrl(direct), positionMs, playWhenReady)
                commit(next, direct, null)
            } else {
                // A type ExoPlayer cannot read as a file: release it and take
                // the same decision as Copy HLS once.
                runCatching { client.end(direct) }
                val hls = sharedPlaybackPlan(next.subject, SharedDecisionClient.Result(next.decision, next.caps), next.selection,
                    next.request.playback_id, UUID.randomUUID().toString(), allowDirect = false)
                started = attachHls(hls, positionMs, playWhenReady)
            }
        } else started = attachHls(next, positionMs, playWhenReady)
        if (previous != null && previous.sessionId != started.sessionId) runCatching { client.end(previous) }
    }

    private suspend fun attachHls(next: SharedPlaybackPlan, positionMs: Long, playWhenReady: Boolean): SharedStartedPlayback {
        val started = client.start(next.subject.context, next.request)
        renderer.attachHls(client.playlistUrl(started), positionMs, playWhenReady)
        commit(next, started, SharedControlChannel(client, started, clientInstanceId, sharedControlCapabilities(next.caps)))
        return started
    }

    private fun commit(next: SharedPlaybackPlan, started: SharedBoundSession, control: SharedControlChannel?) {
        plan = next; session = started; channel = control
        sessionEnded = false; directRestartEarned = false
        selection.value = next.selection; direct.value = started is SharedStartedDirect
        statusSummary.value = null; failure.value = null
    }

    /** Cancel and join every owned job, send the last ordered beat, release the
     * renderer, then end the B session. Never called from inside [scope]. */
    suspend fun stop(watched: Boolean = false) {
        if (closing) return
        closing = true
        renderer.setPlaying(false)
        job.cancelAndJoin()
        val current = session; val plan = plan
        if (current != null && plan != null) {
            val view = renderer.snapshot()
            val position = view.positionMs.coerceAtLeast(0)
            val duration = durationOf(current, view)
            val result = runCatching { client.orderedProgress(current, plan.subject.watchSequence, position, duration, watched) }.getOrNull()
            if (result == SharedProgressResult.PreviousBeatAcknowledged) runCatching { client.orderedProgress(current, plan.subject.watchSequence, position, duration, watched) }
        }
        renderer.release()
        if (current != null) runCatching { client.end(current) }
        session = null; channel = null; statusSummary.value = null
    }
}
