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
import kotlinx.serialization.json.JsonObject
import tv.plurx.app.data.SharedBoundSession
import tv.plurx.app.data.SharedControlChannel
import tv.plurx.app.data.SharedControlOutcome
import tv.plurx.app.data.SharedControlState
import tv.plurx.app.data.SharedDecisionClient
import tv.plurx.app.data.SharedPlaybackPlan
import tv.plurx.app.data.SharedPlaybackSubject
import tv.plurx.app.data.SharedProgressResult
import tv.plurx.app.data.SharedSelection
import tv.plurx.app.data.SharedStart
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
    /** The pipeline on the surface. */
    fun snapshot(): SharedRendererSnapshot
    /** Release every pipeline this renderer holds. */
    fun release()

    // A prepared successor: a second pipeline, muted and without a surface
    // until the switch. Shared sessions are VOD, so every position here is
    // film time on both pipelines.

    /** Build the successor on [url] at [positionMs], parked (not playing). */
    fun prepareSuccessor(url: String, positionMs: Long, textEnabled: Boolean)
    /** The successor's own readings, or null when there is none. */
    fun successorSnapshot(): SharedSuccessorSnapshot?
    /** Seek the parked successor to the rendezvous and leave it waiting there. */
    fun parkSuccessor(positionMs: Long)
    /** The successor takes the surface, the volume and the predecessor's
     * transport intent; the predecessor is paused and retained for rollback. */
    fun switchToSuccessor()
    /** Put the retained predecessor back on the surface and drop the successor. */
    fun restorePredecessor()
    /** The commit settled: the retained predecessor is no longer needed. */
    fun releasePredecessor()
    /** Drop a successor that was never switched to. */
    fun releaseSuccessor()
}

internal data class SharedRendererSnapshot(
    val positionMs: Long, val bufferedMs: Long, val durationMs: Long?,
    val playing: Boolean, val renderState: RenderState, val playbackRate: Double = 1.0,
)

internal data class SharedSuccessorSnapshot(
    /** Playable: `STATE_READY` with tracks published. */
    val ready: Boolean,
    val positionMs: Long,
    val bufferedMs: Long,
    /** The last park seek has landed, per the successor's own listener. */
    val seekLanded: Boolean,
    val failed: Boolean,
    /** Wall clock of a frame the successor actually rendered after the switch. */
    val firstFrameUnixMs: Long?,
)

/**
 * One Shared playback, from the first Start to the last DELETE.
 *
 * Every viewer command runs under [commands], so a seek, a pause and a quality
 * change reach B in the order they were asked and never interleave with a
 * reopen. Shared HLS changes the renderer only after B accepts the exchange
 * that asked for it.
 *
 * A directed change (quality, audio, subtitle) is an ask on the current
 * session. Without [preparedHandoff] the channel declares no actions, B answers
 * `preparation: none`, and the owner makes a fresh Start at the sampled
 * position with the new selection, attaches it, and only then DELETEs the
 * predecessor (P0). With it, the channel declares `prepare_replacement` and
 * `shared_prepare_replacement` plus dual-player preparation, and B may stage
 * the successor itself: `staging` until it is published, then `offered` with a
 * `prepare` naming only B's successor session, playlist and control bootstrap.
 * The owner primes a second pipeline with the Local M6 pieces (the offer wait,
 * the ledger, the rendezvous), acknowledges on the predecessor's channel, and
 * after the commit is accepted moves control, status and Shared progress to the
 * successor. B retires the predecessor on that commit, so it is not DELETEd.
 * `none`, a failure or a refused commit takes the P0 reopen exactly once.
 *
 * Direct play has no control route; its renderer is local, and a byte URL B
 * retired after a long pause earns exactly one fresh direct Start per
 * attachment that reached its timeline.
 *
 * Owned work only: every coroutine here is a child of [job], which [stop]
 * cancels and joins before the renderer is released and the session ended.
 */
internal class SharedPlaybackOwner(
    parent: CoroutineScope,
    clientFactory: () -> SharedDecisionClient,
    private val renderer: SharedRenderer,
    private val preparedHandoff: Boolean = false,
    private val clock: () -> Long = { System.nanoTime() / 1_000_000 },
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
    /** One identity per player, across reopens and handoffs: the same viewer continuing. */
    private val clientInstanceId = UUID.randomUUID().toString()
    private var plan: SharedPlaybackPlan? = null
    private var session: SharedBoundSession? = null
    private var channel: SharedControlChannel? = null
    private var sessionEnded = false
    private var directRestartEarned = false
    private var closing = false

    /**
     * The directed change B is preparing a successor for, from the ask to the
     * commit or the one reopen that replaces it. While it lives every exchange
     * on the predecessor carries its selection: an exchange carrying the old
     * ask would tell B the viewer left this one, and B withdraws the successor.
     */
    private class Handoff(val next: SharedSelection, val wait: PreparedOfferWait) {
        val ledger = PreparedReplacementLedger()
        var step: PreparedOfferWait.Step = PreparedOfferWait.Step.KeepWaiting(PREPARED_OFFER_STAGING_CADENCE_MS)
        var offer: JsonObject? = null
        var result: SharedDecisionClient.Result? = null
        var successor: SharedStartedPlayback? = null
        var successorPlan: SharedPlaybackPlan? = null
        var preparedAtMs = 0L
        var hold: RendezvousHold? = null
        /** B answered `none` after the offer: the successor is withdrawn. */
        var withdrawn = false
    }
    private var handoff: Handoff? = null

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
        exchange(state(view))
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
        handoff?.let { live ->
            if (!live.ledger.isSwitched) {
                // A successor parked for the old position is not what the
                // viewer wants any more. Abandon it and honour both the seek
                // and the selection they asked for with the one reopen.
                abandon(live, failed = false)
                reopen(live.next, target, view.playing, allowDirect = false, decided = live.result)
                return@withLock
            }
        }
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
            is SharedControlOutcome.Ended -> if (playing) reopen(handoff?.next ?: requireNotNull(plan).selection, view.positionMs, true, allowDirect = false, decided = handoff?.result)
                else renderer.setPlaying(false)
            else -> refused(outcome)
        }
    }

    /** A directed quality, audio or subtitle change. */
    suspend fun change(next: SharedSelection) = commands.withLock {
        val current = requireNotNull(plan)
        // A newer ask supersedes an unfinished one, which owes B its abort.
        handoff?.let { abandon(it, failed = false) }
        if (next == current.selection) return@withLock
        val view = renderer.snapshot()
        val control = channel
        if (control == null || sessionEnded) {
            reopen(next, view.positionMs, view.playing, allowDirect = false); return@withLock
        }
        val live = if (preparedHandoff) Handoff(next, PreparedOfferWait(clock(), control.sequence + 1, noneOnTheAskDeclines = true)) else null
        handoff = live
        // The ask itself: the new selection, raw, on the current session.
        when (val outcome = exchange(state(view), selection = next.controlSelection())) {
            is SharedControlOutcome.Accepted -> when {
                // P0: B carries no successor for this client, so it answers
                // every evaluated preparation as `none`; absence is an older
                // relay saying the same thing. Nothing is being built: reopen.
                live == null ->
                    if (outcome.preparation == null || outcome.preparation == PREPARATION_NONE) reopen(next, view.positionMs, view.playing, allowDirect = false)
                    else failure.value = "This shared playback change is not available."
                live.step is PreparedOfferWait.Step.Reopen -> fallBack(live)
                else -> scope.launch { drive(live) }
            }
            is SharedControlOutcome.Ended -> { handoff = null; sessionEnded = true; reopen(next, view.positionMs, view.playing, allowDirect = false) }
            else -> { handoff = null; if (outcome != null) refused(outcome) }
        }
    }

    /**
     * The handoff from the ask to its settlement. Owned work, and never inside
     * [commands] while it waits: each step takes the lock for what it does, so
     * a seek or a pause still reaches B while a successor primes. The switch
     * and the commit are one locked step, because nothing may run between a
     * successor taking the surface and B being told.
     */
    private suspend fun drive(h: Handoff) {
        try {
            while (true) {
                when (val step = h.step) {
                    is PreparedOfferWait.Step.Offered -> break
                    is PreparedOfferWait.Step.Reopen -> { commands.withLock { if (handoff === h) fallBack(h) }; return }
                    is PreparedOfferWait.Step.KeepWaiting -> {
                        delay(step.nextExchangeMs)
                        commands.withLock {
                            if (handoff !== h) return
                            // A cadence exchange may have carried the offer meanwhile.
                            if (h.step !is PreparedOfferWait.Step.KeepWaiting) return@withLock
                            val bound = h.wait.observe(null, clock())
                            if (bound is PreparedOfferWait.Step.Reopen) h.step = bound
                            else when (val outcome = exchange(state(renderer.snapshot()))) {
                                is SharedControlOutcome.Ended -> h.step = PreparedOfferWait.Step.Reopen("ended")
                                is SharedControlOutcome.Refused -> { handoff = null; refused(outcome); return }
                                else -> Unit
                            }
                        }
                    }
                }
            }
            commands.withLock { if (handoff === h) beginSuccessor(h) }
            while (handoff === h) {
                delay(RENDEZVOUS_READY_POLL_MS)
                commands.withLock { if (handoff === h) primeStep(h) }
            }
        } catch (error: Throwable) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            report(error)
            // A step failed outright (a transport refusal, a Start that could
            // not be read). The change is still the viewer's: settle what the
            // handoff owes and take its one reopen.
            try { commands.withLock { if (handoff === h) recover(h) } } catch (again: Throwable) {
                if (again is kotlinx.coroutines.CancellationException) throw again
                report(again)
            }
        }
    }

    private suspend fun recover(h: Handoff) {
        val successor = h.successor
        if (h.ledger.isSwitched && successor != null) settleUnknown(h, successor)
        else { abandon(h, failed = true); fallBack(h) }
    }

    /**
     * Nothing is known about the commit: B may have moved to the successor or
     * may still be on the predecessor. A fresh Start with the viewer's
     * selection supersedes both by playback id, and the successor is ended
     * explicitly so neither outcome leaves a session behind.
     */
    private suspend fun settleUnknown(h: Handoff, successor: SharedStartedPlayback) {
        handoff = null
        renderer.releasePredecessor()
        val view = renderer.snapshot()
        reopen(h.next, view.positionMs, view.playing, allowDirect = false, decided = h.result)
        runCatching { client.end(successor) }
    }

    /** The offer arrived: bind B's successor and prime a second pipeline on it. */
    private suspend fun beginSuccessor(h: Handoff) {
        val action = (h.step as PreparedOfferWait.Step.Offered).action
        val current = session as? SharedStartedPlayback
        val offer = h.ledger.offer(action)
        if (offer !is PreparationOffer.Start || current == null) { fallBack(h); return }
        val view = renderer.snapshot()
        try {
            val plan = successorPlan(h, view.positionMs)
            val successor = SharedStart.successor(requireNotNull(h.offer), current, plan.request)
            h.successorPlan = plan; h.successor = successor; h.preparedAtMs = clock()
            renderer.prepareSuccessor(client.playlistUrl(successor), view.positionMs, h.next.subtitle != null)
        } catch (error: Exception) {
            if (error is kotlinx.coroutines.CancellationException) throw error
            // An offer this client cannot bind, or a device that cannot stand
            // up a second pipeline: `failed`, and the viewer's change is still
            // owed through the ordinary reopen.
            if (h.successor != null) renderer.releaseSuccessor()
            h.successor = null
            acknowledge(h.ledger.failed())
            fallBack(h)
        }
    }

    /** One look at the successor: readiness, the park, the rendezvous. */
    private suspend fun primeStep(h: Handoff) {
        val ready = renderer.successorSnapshot()
        if (ready == null || ready.failed || h.withdrawn || sessionEnded || clock() - h.preparedAtMs > PREPARED_READINESS_BOUND_MS) {
            abandon(h, failed = true); fallBack(h); return
        }
        if (!ready.ready) return
        h.ledger.metadataReady()?.let { acknowledge(it) }
        val hold = h.hold ?: RendezvousHold().also { created ->
            h.hold = created
            renderer.parkSuccessor(created.park(clock(), renderer.snapshot().positionMs).rendezvousFilmMs)
            return
        }
        val target = hold.rendezvousFilmMs ?: return
        if (!hold.isReady) {
            if (!ready.seekLanded || !successorIsBuffered(ready.bufferedMs, target)) return
            hold.ready(clock())
            acknowledge(h.ledger.bufferReady(ready.bufferedMs))
            return
        }
        val view = renderer.snapshot()
        when (val step = hold.fire(clock(), view.positionMs, ready.positionMs, hold.isReady, if (view.playing) view.playbackRate else 0.0)) {
            is RendezvousHold.Step.Commit -> switchAndSettle(h)
            is RendezvousHold.Step.Wait -> Unit
            is RendezvousHold.Step.Repark -> renderer.parkSuccessor(step.park.rendezvousFilmMs)
            is RendezvousHold.Step.Abandon -> { abandon(h, failed = true); fallBack(h) }
        }
    }

    /**
     * The successor takes the surface, proves a frame, and the commit goes to
     * B on the predecessor's channel. Accepted: control, status and progress
     * move to the successor's B session. Refused: the predecessor is put back
     * and the change takes its reopen. No answer at all after the same bytes
     * were asked again: nothing is known about the commit, so the change takes
     * a reopen that supersedes both sessions.
     */
    private suspend fun switchAndSettle(h: Handoff) {
        val successor = requireNotNull(h.successor)
        renderer.switchToSuccessor()
        h.ledger.switched()
        val since = clock()
        var frame = renderer.successorSnapshot()?.firstFrameUnixMs
        while (frame == null && clock() - since <= PREPARED_COMMIT_FRAME_BOUND_MS) {
            delay(COMMIT_FRAME_POLL_MS)
            frame = renderer.successorSnapshot()?.firstFrameUnixMs
        }
        if (frame == null) {
            // Never invent a frame: settle `failed` and put the working
            // predecessor back before the change takes its reopen.
            val owed = h.ledger.failedAfterSwitch()
            renderer.restorePredecessor()
            acknowledge(owed)
            fallBack(h); return
        }
        val commit = requireNotNull(h.ledger.committed(frame))
        val control = requireNotNull(channel)
        var outcome = exchange(state(renderer.snapshot()), commit)
        var asked = 0
        while ((outcome is SharedControlOutcome.Unavailable || outcome is SharedControlOutcome.Ended) && asked++ < COMMIT_REPLAYS) {
            delay(PREPARED_OFFER_STAGING_CADENCE_MS)
            outcome = control.replayLast()
        }
        when (outcome) {
            is SharedControlOutcome.Accepted -> {
                handoff = null
                val next = requireNotNull(h.successorPlan)
                plan = next; session = successor
                channel = SharedControlChannel(client, successor, clientInstanceId, sharedControlCapabilities(next.caps, true), prepared = true)
                sessionEnded = false; directRestartEarned = false
                selection.value = next.selection; direct.value = false
                statusSummary.value = null; failure.value = null
                renderer.releasePredecessor()
                // No DELETE: B superseded the predecessor on this commit and
                // its retirement owner sends the Source its End.
            }
            is SharedControlOutcome.Refused -> { renderer.restorePredecessor(); fallBack(h) }
            else -> settleUnknown(h, successor)
        }
    }

    /** Drop an unswitched preparation and owe B its terminal acknowledgement. */
    private suspend fun abandon(h: Handoff, failed: Boolean) {
        if (h.ledger.isSwitched) return
        val owed = if (failed) h.ledger.failed() else h.ledger.aborted()
        if (h.successor != null) renderer.releaseSuccessor()
        h.successor = null
        acknowledge(owed)
        if (!failed && handoff === h) handoff = null
    }

    /** The change's one ordinary reopen: a fresh Start with the new selection. */
    private suspend fun fallBack(h: Handoff) {
        if (handoff === h) handoff = null
        val view = renderer.snapshot()
        reopen(h.next, view.positionMs, view.playing, allowDirect = false, decided = h.result)
    }

    /** The plan the successor stands for, decided as the P0 reopen would decide it. */
    private suspend fun successorPlan(h: Handoff, positionMs: Long): SharedPlaybackPlan {
        val current = requireNotNull(plan)
        val result = h.result ?: client.redecide(current.subject.context, current.caps, h.next.decisionQuery()).also { h.result = it }
        val subject = SharedPlaybackSubject(current.subject.context, current.subject.title, positionMs.coerceAtLeast(0), current.subject.watchSequence)
        return sharedPlaybackPlan(subject, result, h.next, current.request.playback_id, UUID.randomUUID().toString(), allowDirect = false)
    }

    /** Send an acknowledgement now, on the predecessor's channel. */
    private suspend fun acknowledge(acknowledgement: ActionAcknowledgement?) {
        acknowledgement ?: return
        exchange(state(renderer.snapshot()), acknowledgement)
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

    /** The selection every exchange carries: the change B is preparing, else the Start's frozen ask. */
    private fun desiredSelection(): JsonObject = handoff?.next?.controlSelection() ?: requireNotNull(plan).frozenControlSelection()

    /** One exchange on the current channel. While a handoff waits for its
     * offer, every accepted answer is the wait's to read. */
    private suspend fun exchange(state: SharedControlState, acknowledgement: ActionAcknowledgement? = null,
                                 selection: JsonObject? = null): SharedControlOutcome? {
        val control = channel ?: return null
        if (sessionEnded) return SharedControlOutcome.Ended(null)
        val outcome = control.exchange(state, selection ?: desiredSelection(), acknowledgement)
        if (outcome is SharedControlOutcome.Ended) sessionEnded = true
        val h = handoff
        if (h != null && outcome is SharedControlOutcome.Accepted) {
            if (h.successor == null && h.step is PreparedOfferWait.Step.KeepWaiting) {
                val step = h.wait.observe(ControlAnswer(outcome.sequence, outcome.action, outcome.preparation), clock())
                if (step is PreparedOfferWait.Step.Offered) h.offer = outcome.actionWire
                h.step = step
            } else if (h.successor != null && outcome.preparation == PREPARATION_NONE && !h.ledger.isSwitched) {
                h.withdrawn = true
            }
        }
        return outcome
    }

    private suspend fun control(state: SharedControlState): SharedControlOutcome? = exchange(state)

    private fun state(view: SharedRendererSnapshot) = SharedControlState(
        demand = if (view.playing) PlaybackDemand.ACTIVE else PlaybackDemand.HOLD,
        positionMs = view.positionMs.coerceAtLeast(0), bufferedThroughMs = view.bufferedMs.coerceAtLeast(0),
        renderState = view.renderState, playbackRate = view.playbackRate.takeIf { it in 0.25..4.0 } ?: 1.0,
    )

    private fun durationOf(current: SharedBoundSession, view: SharedRendererSnapshot): Long? =
        (current as? SharedStartedPlayback)?.start?.response?.duration_ms ?: view.durationMs?.takeIf { it >= 0 }

    /** A fresh Start of the same file under the accepted login, with no lineage. */
    private suspend fun reopen(next: SharedSelection, positionMs: Long, playWhenReady: Boolean, allowDirect: Boolean,
                               decided: SharedDecisionClient.Result? = null) {
        val current = requireNotNull(plan)
        val subject = SharedPlaybackSubject(current.subject.context, current.subject.title, positionMs.coerceAtLeast(0), current.subject.watchSequence)
        // The same ask keeps its decision (a retired session, an expired direct
        // play); a changed ask is a new question for the Source, asked once.
        val result = decided ?: if (next == current.selection) SharedDecisionClient.Result(current.decision, current.caps)
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
        commit(next, started, SharedControlChannel(client, started, clientInstanceId,
            sharedControlCapabilities(next.caps, preparedHandoff), prepared = preparedHandoff))
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
        // A successor that was switched to but never settled is B's to retire
        // only if the commit landed; end it explicitly so nothing is left.
        val unsettled = handoff?.takeIf { it.ledger.isSwitched }?.successor
        handoff = null
        if (current != null && plan != null) {
            val view = renderer.snapshot()
            val position = view.positionMs.coerceAtLeast(0)
            val duration = durationOf(current, view)
            val result = runCatching { client.orderedProgress(current, plan.subject.watchSequence, position, duration, watched) }.getOrNull()
            if (result == SharedProgressResult.PreviousBeatAcknowledged) runCatching { client.orderedProgress(current, plan.subject.watchSequence, position, duration, watched) }
        }
        renderer.release()
        if (current != null) runCatching { client.end(current) }
        unsettled?.let { runCatching { client.end(it) } }
        session = null; channel = null; statusSummary.value = null
    }

    private companion object {
        /** How often the switch looks for the successor's first rendered frame. */
        const val COMMIT_FRAME_POLL_MS = 50L
        /** How many times a commit with no answer is asked again, byte for byte. */
        const val COMMIT_REPLAYS = 2
    }
}
