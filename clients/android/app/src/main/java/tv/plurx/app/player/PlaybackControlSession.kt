package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.longOrNull
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import java.io.IOException
import java.util.UUID
import java.util.concurrent.atomic.AtomicReference

/**
 * The playback-control exchange, over HTTP.
 *
 * Separate from [PlaybackControlReporter] because the reporter's rules are
 * about ordering and this one's are about what a server's refusal means. The
 * reporter classifies a failure by `status`, `code` and `retry_after_ms`;
 * producing those faithfully from an HTTP response is this file's whole job,
 * and getting it wrong would make a retryable refusal look terminal.
 */
class PlaybackControlTransport(
    private val origin: String,
    private val client: OkHttpClient = Net.client,
    private val json: Json = Net.json,
) {
    suspend fun send(path: String, request: ControlRequest): ControlResponse {
        // The bootstrap's url is server-relative and already shape-checked, so
        // joining it to this origin cannot reach another host. Re-checking here
        // means no caller can hand this an address the reporter never approved.
        if (!ControlBootstrap.isSessionControlPath(path)) {
            throw ControlProtocolException("url")
        }
        val url = origin.trimEnd('/') + path
        val body = json.encodeToString(ControlRequest.serializer(), request)
            .toRequestBody("application/json".toMediaType())
        val call = Request.Builder().url(url).post(body).build()
        return withContext(Dispatchers.IO) {
            val response = try {
                client.newCall(call).execute()
            } catch (failure: IOException) {
                // No status at all — the reporter treats that as retryable
                // transport, which is right: the server never saw this.
                throw ControlTransportException(status = null, code = null)
            }
            response.use {
                val text = it.body?.string().orEmpty()
                if (!it.isSuccessful) throw failure(it.code, text, json)
                try {
                    json.decodeFromString(ControlResponse.serializer(), text)
                } catch (_: Exception) {
                    // A 2xx that is not a control response is not a transport
                    // problem to retry; it is something other than this
                    // server's control route answering.
                    throw ControlProtocolException("body")
                }
            }
        }
    }

    companion object {
        /**
         * A refusal carries the fields the reporter classifies on. Every one is
         * optional on the wire, and a body that is missing or unparseable still
         * yields the status, which is enough to decide retryable from terminal.
         */
        fun failure(status: Int, body: String, json: Json = Net.json): ControlTransportException {
            var code: String? = null
            var generation: String? = null
            var epoch: Long? = null
            var retryAfter: Long? = null
            try {
                val fields = json.parseToJsonElement(body).jsonObject
                code = fields["code"]?.jsonPrimitive?.contentOrNullSafe()
                generation = fields["generation"]?.jsonPrimitive?.contentOrNullSafe()
                epoch = fields["control_epoch"]?.jsonPrimitive?.longOrNull
                retryAfter = fields["retry_after_ms"]?.jsonPrimitive?.longOrNull
            } catch (_: Exception) {
                // Status only. That is still enough to classify.
            }
            return ControlTransportException(status, code, generation, epoch, retryAfter)
        }

        /** `null` for a JSON null or a non-string, rather than the text "null". */
        private fun kotlinx.serialization.json.JsonPrimitive.contentOrNullSafe(): String? =
            if (isString) content else null
    }
}

/**
 * One player's control reporting, from the bootstrap the server handed back
 * with the session to the last exchange before release.
 *
 * This is the piece [Controller] holds. It exists so the controller deals in
 * "the player changed" rather than in reporters, transports, identities and
 * deadlines.
 */
class PlaybackControlSession(
    private val scope: CoroutineScope,
    private val dispatchSubtitleReady: (() -> Unit) -> Unit = { callback ->
        scope.launch { callback() }
    },
) {
    private var reporter: PlaybackControlReporter? = null
    // Session departure stops callbacks immediately, while a replacement's
    // create still needs the predecessor's final ordering counter.
    private var orderingSource: PlaybackControlReporter? = null
    private var observe: (() -> PlayerControlObservation?)? = null
    // Only caller/player-dispatcher entry points sample the player. The
    // reporter reads this immutable envelope, including on cadence/retry.
    private val latest = AtomicReference<PlaybackControlCapture?>(null)
    private var captureRevision = 0L

    /**
     * Whether an exchange coming back is still allowed to reach the player.
     *
     * Cleared before the player is torn down, and never set again. A stop is
     * asynchronous and an exchange can be in flight across it, so a callback
     * that acquires resources — `onPrepare` builds an ExoPlayer — needs a
     * switch as well as the generation fence.
     */
    @Volatile
    private var dispatching = true

    /**
     * One identity per player instance, not per session: a reopen is the same
     * viewer on the same device continuing, and the server reads a new
     * `client_instance_id` as a different client.
     */
    private val clientInstanceId = UUID.randomUUID().toString()

    val isReporting: Boolean get() = reporter != null

    /**
     * Where this playback sits in the control ordering, for a create that
     * wants to be ordered against the settled destination.
     *
     * The reporter's own counter rather than the accepted sequence: a create
     * can reach the server before the snapshot that justifies it, and a client
     * reporting a *higher* sequence can only look less superseded, which is
     * the safe direction. Null before the first exchange.
     */
    suspend fun controlSequence(): Long? =
        orderingSource?.status()?.sequence?.takeIf { it > 0L }

    /**
     * Every exchange's action, tagged with the sequence of the request it
     * answered. The ask reads the reporter's counter, publishes its evidence,
     * and takes the first answer at or above that floor.
     */
    private val answerLock = Any()
    private var answerAction: ControlAction? = null
    private var answerRequestSequence = 0L
    /**
     * `delivery.preparation` from the same exchange as [answerAction].
     *
     * Kept beside the action rather than derived later, so the word and the
     * action a wait decides on are always from one exchange. Null is a real
     * value here — an older server sends no such field.
     */
    private var answerPreparation: String? = null
    private var answersSeen = 0L
    private var ownerChangesSeen = 0L
    /**
     * The answer slot keeps its own copy of the generation rather than reading
     * the verdict slot's under the wrong lock. Two counters that must agree
     * are a bug waiting for a reason, but one counter read without its lock is
     * one already.
     */
    private var answerGeneration = 0

    private val verdictLock = Any()
    private var verdict: ControlAction? = null
    private var verdictArmedAtMs = 0L
    private var verdictLeaseMs = 0L
    private var verdictGeneration = 0
    private var verdictIntentGeneration = 0L

    /**
     * The last terminal verdict this session was given, if any.
     *
     * It deliberately outlives the reporter. A terminal verdict stops
     * reporting — correctly, since the reporter owns no recovery — so a
     * verdict that died with it would be discarded exactly when it mattered:
     * at the failure it explains, later.
     */
    val terminalVerdict: ControlAction?
        get() = synchronized(verdictLock) {
            val armed = verdict ?: return@synchronized null
            // A verdict outlives its reporter and its session, but not the
            // lease the server gave that session. Past it the session the
            // verdict described is gone, and a confident sentence about a
            // production attempt that ended an hour ago would caption an
            // unrelated failure. The bound is the server's own number rather
            // than one invented here.
            if (monotonicNowMs() - verdictArmedAtMs > verdictLeaseMs) {
                verdict = null
                return@synchronized null
            }
            armed
        }

    /**
     * Publish what a recovery owner is about to act on, then wait — briefly —
     * for the verdict that evidence earns.
     *
     * [reportEvidence] tells the server. This tells the server and listens.
     * The difference is the whole milestone: an owner that only reports still
     * decides for itself, and every one of them decides by guessing toward
     * retry.
     *
     * Returns null when the reporter is gone or stopped, the exchange failed,
     * or nothing arrived in time — and every caller must fall through to its
     * existing behaviour on it. That fallback is not a hedge: a server that
     * has not yet decided must not strand a stalled viewer.
     *
     * `publish` is handed in rather than called first, because the floor has
     * to be read before the evidence goes out: publishing first lets the pump
     * start the next request before the read lands, which makes the floor one
     * too high and rejects the very exchange that carried the evidence.
     */
    suspend fun askForAction(
        boundMs: Long,
        capMs: Long,
        publish: () -> Unit,
        now: () -> Long = ::monotonicNowMs,
    ): ControlAction? {
        val subject = reporter ?: return null
        val status = subject.status()
        if (status.stopped) return null
        val floor = status.sequence + 1
        val seenAtStart = synchronized(answerLock) { answersSeen }
        val ownerChangesAtStart = synchronized(answerLock) { ownerChangesSeen }
        publish()
        val startedAt = now()
        var deadline = startedAt + boundMs
        val hardDeadline = startedAt + capMs
        var seen = seenAtStart
        var extended = false
        // Both conditions. An answer that arrived before this ask cannot be its
        // answer, and a 409 owner reset zeroes the reporter's sequence, so the
        // floor alone is not enough.
        fun ready(): Result<ControlAction?>? = synchronized(answerLock) {
            if (answersSeen > seenAtStart && answerRequestSequence >= floor) {
                Result.success(answerAction)
            } else {
                null
            }
        }
        while (now() < deadline) {
            if (synchronized(answerLock) { ownerChangesSeen > ownerChangesAtStart }) return null
            ready()?.let { return it.getOrNull() }
            val count = synchronized(answerLock) { answersSeen }
            if (count > seen) {
                seen = count
                // An exchange finished and it was not ours, which means the
                // reporter could not have started ours until now: run() picks
                // up `pending` only after the one in flight. One window from
                // this instant, once.
                if (!extended) {
                    extended = true
                    deadline = minOf(now() + boundMs, hardDeadline)
                }
            }
            kotlinx.coroutines.delay(ASK_POLL_MS)
            // Read the slot again before giving up on a reporter that went
            // away. A terminal verdict is answered and then stops the reporter
            // in the same instant, so bailing on `stopped` without re-reading
            // discards the one verdict this ask most needed to see — which is
            // exactly what happened, and what the terminal test now pins.
            val current = reporter
            if (current == null || current.status().stopped) {
                return ready()?.getOrNull()
            }
        }
        return ready()?.getOrNull()
    }

    /**
     * The viewer changed rung; wait for the server to offer a successor.
     *
     * Beside [askForAction] rather than inside it, and on its own numbers.
     * [askForAction]'s bound is stall policy — how long a *frozen* viewer may
     * be made to wait before this client recovers on its own — and widening it
     * to cover a preparation would change what a stall does. This wait is the
     * other case entirely: playback is healthy and continues throughout, so it
     * can afford [PreparedOfferWait.BOUND_MS] and the viewer loses nothing by
     * it.
     *
     * [tappedAtMs] is when the viewer tapped, not when this was called. The
     * bound covers the dispatch exchange and the server's own admission, both
     * of which happen before this is reached.
     *
     * Always returns a terminal step: [PreparedOfferWait.Step.Offered] or
     * [PreparedOfferWait.Step.Reopen]. The caller owns the outcome either way.
     */
    internal suspend fun awaitPreparedOffer(
        tappedAtMs: Long,
        now: () -> Long = ::monotonicNowMs,
    ): PreparedOfferWait.Step {
        val subject = reporter ?: return PreparedOfferWait.Step.Reopen("no_control")
        // The same floor discipline as the ask, and for the same reason. The
        // dispatch that carried the selection is *queued* rather than sent when
        // `reportIntent` returns — `notifyUrgently` hands back `sequence + 1`
        // and leaves the pump to build the request — so the counter read here
        // is still the one before it. One past it is the dispatch's own
        // sequence, and anything below that answered a request from before the
        // tap.
        val floor = subject.status().sequence + 1
        val wait = PreparedOfferWait(tappedAtMs, floor)
        var seen = synchronized(answerLock) { answersSeen }
        var lastNudgeMs = now() - PreparedOfferWait.STAGING_CADENCE_MS
        while (true) {
            val answer = synchronized(answerLock) {
                if (answersSeen > seen) {
                    seen = answersSeen
                    ControlAnswer(answerRequestSequence, answerAction, answerPreparation)
                } else {
                    null
                }
            }
            val observed = wait.observe(answer, now())
            // Advisory only, and recorded wherever the word actually arrives.
            // Nothing reads it back to decide anything.
            PreparedReplacementAdvisory.recordPreparation(wait.lastPreparation)
            when (val step = observed) {
                is PreparedOfferWait.Step.Offered -> return step
                is PreparedOfferWait.Step.Reopen -> return step
                is PreparedOfferWait.Step.KeepWaiting -> {
                    val current = reporter
                    if (current == null || current.status().stopped) {
                        return PreparedOfferWait.Step.Reopen("no_control")
                    }
                    // Nudge only while the server says it is working on it, and
                    // no more often than the cadence. The pump otherwise sleeps
                    // for `next_exchange_ms`, which is long enough that the
                    // whole bound could pass inside one sleep.
                    if (wait.lastPreparation == PREPARATION_STAGING &&
                        now() - lastNudgeMs >= step.nextExchangeMs
                    ) {
                        lastNudgeMs = now()
                        try {
                            reportEvidence()
                        } catch (cancellation: kotlinx.coroutines.CancellationException) {
                            // Re-thrown explicitly. A blanket `Exception` catch
                            // swallows cancellation, and the M6 review found
                            // exactly that trap on this file's paths: the wait
                            // would keep looping after the player was gone.
                            throw cancellation
                        } catch (_: Exception) {
                            // A nudge is best effort. The cadence will try
                            // again, and the bound still ends the wait.
                        }
                    }
                    kotlinx.coroutines.delay(ASK_POLL_MS)
                }
            }
        }
    }

    /**
     * A new title. The old verdict described a source that is no longer
     * playing, so keeping it would show a confident sentence about the wrong
     * film. A reopen deliberately does not clear it: the failure a verdict
     * explains normally arrives on the far side of one.
     */
    fun clearVerdict() {
        synchronized(verdictLock) {
            verdictIntentGeneration += 1
            verdict = null
            latest.set(null)
        }
    }

    /** Test seam: what the verdict's staleness bound is measured against. */
    internal fun verdictArmedAtMsForTest(): Long = synchronized(verdictLock) { verdictArmedAtMs }

    /**
     * Begin reporting for a session the server said is controllable. A
     * bootstrap this client cannot address leaves it silent, which is the
     * passive behaviour rather than a playback failure.
     */
    fun begin(
        bootstrap: ControlBootstrap,
        observe: () -> PlayerControlObservation?,
        transport: PlaybackControlTransport = PlaybackControlTransport(Session.origin),
        onSubtitleReady: () -> Unit = {},
        onPrepare: (ControlAction) -> Unit = {},
        onAcknowledged: (ActionAcknowledgement) -> Unit = {},
        /** The reporter stopped for good. A fact to record, never a surface. */
        onGaveUp: (String) -> Unit = {},
    ) {
        end()
        this.observe = observe
        // `end()` closed the dispatch switch on the generation that just
        // finished. This one is open again; the generation below is what keeps
        // the two apart.
        dispatching = true
        // A generation, not a reset. `end()` stops the old reporter in a
        // launched coroutine, so the stop does not necessarily land before
        // this begin — and an old in-flight exchange completing in that window
        // would otherwise carry a previous generation's verdict into this one.
        val generation = synchronized(verdictLock) { ++verdictGeneration }
        val owner = PlaybackControlCaptureOwner(clientInstanceId, generation)
        synchronized(answerLock) {
            answerGeneration = generation
            answerAction = null
            answerRequestSequence = 0
            answerPreparation = null
            answersSeen = 0
            ownerChangesSeen = 0
        }
        val leaseMs = bootstrap.leaseTimeoutMs
        val subtitleReadiness = SubtitleReadinessRetryState()
        publish()
        val subject = PlaybackControlReporter.create(
            bootstrap = bootstrap,
            clientInstanceId = clientInstanceId,
            owner = owner,
            capture = { latest.get()?.takeIf { it.owner == owner } },
            send = { path, request -> transport.send(path, request) },
            pace = { kotlinx.coroutines.delay(it) },
            now = { System.currentTimeMillis() },
            // The return path. Until now this defaulted to a no-op, so the
            // server could send a verdict the player would never see.
            //
            // Only `terminal` is retained, and retained rather than acted on.
            // `hold` and `retry_resource` are exchange-level and the reporter
            // already honours them; a player acting on them here would be
            // deciding, which is the next slice.
            onExchange = { exchange ->
                if (exchange.capture.owner != PlaybackControlCaptureOwner(clientInstanceId, generation) ||
                    latest.get()?.owner != exchange.capture.owner
                ) {
                    return@create
                }
                // Every exchange advances the counter, including a failed one:
                // an owner that asked must not wait out its whole bound for an
                // exchange that has already come back with nothing.
                synchronized(answerLock) {
                    if (generation == answerGeneration) {
                        answersSeen += 1
                        if (exchange.failure == "transport:409:owner_changed") {
                            ownerChangesSeen += 1
                        }
                        answerAction = exchange.response?.action
                        answerRequestSequence = exchange.request.sequence
                        answerPreparation = exchange.response?.delivery?.preparation
                    }
                }
                if (exchange.capture.hasSameIntent(latest.get()) && subtitleReadiness.record(
                        exchange.response?.delivery?.subtitleReadiness, commitReady = false,
                    )) {
                    dispatchSubtitleReady {
                        // The callback can be queued while begin/end replaces
                        // the reporter. Check ownership when it executes, not
                        // when the response merely schedules it.
                        synchronized(verdictLock) {
                            if (generation == verdictGeneration &&
                                exchange.capture.owner == PlaybackControlCaptureOwner(clientInstanceId, verdictGeneration) &&
                                exchange.intentGeneration == verdictIntentGeneration &&
                                subtitleReadiness.record(exchange.response?.delivery?.subtitleReadiness)
                            ) {
                                onSubtitleReady()
                            }
                        }
                    }
                }
                // An acknowledgement rides on the request, so its delivery is
                // an answer to *this* exchange rather than a separate reply.
                // Only an exchange the server actually answered clears it: a
                // retry replays the exact request, which is what makes a lost
                // commit go again instead of vanishing.
                if (exchange.response != null && generation == answerGeneration) {
                    exchange.request.acknowledgement?.let { onAcknowledged(it) }
                }
                val action = exchange.response?.action
                // A staged successor is named on an ordinary exchange, not only
                // on a stall ask, so the dispatch is here rather than in the
                // player's stall path. The reporter has already refused a
                // malformed payload — this only ever sees one that validated.
                //
                // Behind the generation fence and the dispatch switch, like its
                // neighbours and for a sharper reason than either: this
                // callback *builds an ExoPlayer*. An exchange that was already
                // in flight when the session ended, or when the player was
                // released, would otherwise stand a second pipeline up against
                // a session that no longer exists — with nothing left running
                // to release it.
                if (action != null &&
                    action.type == PlaybackControl.PREPARE_ACTION_TYPE &&
                    generation == answerGeneration &&
                    dispatching
                ) {
                    onPrepare(action)
                }
                if (action != null &&
                    action.type == "terminal" &&
                    !action.message.isNullOrEmpty()
                ) {
                    synchronized(verdictLock) {
                        if (generation == verdictGeneration &&
                            exchange.capture.owner == PlaybackControlCaptureOwner(clientInstanceId, verdictGeneration) &&
                            exchange.intentGeneration == verdictIntentGeneration
                        ) {
                            verdict = action
                            verdictArmedAtMs = monotonicNowMs()
                            verdictLeaseMs = leaseMs
                        }
                    }
                }
            },
            onGaveUp = { reason ->
                // Behind the same generation fence as its neighbours: a
                // reporter this `begin` replaced may still be unwinding, and
                // its give-up is not news about the session running now.
                if (generation == verdictGeneration) onGaveUp(reason)
            },
        ) ?: return
        reporter = subject
        orderingSource = subject
        scope.launch { subject.start(scope) }
    }

    /**
     * The player changed. Cheap enough to call from a one-second tick: the
     * reporter coalesces, so a notification between exchanges costs nothing
     * but replaces what the next exchange will carry.
     */
    fun playerChanged() {
        val capture = publish() ?: return
        val subject = reporter ?: return
        scope.launch { subject.notify(capture) }
    }

    /**
     * A recovery owner published evidence and is about to act on it.
     *
     * Coalescing is right for a position update and wrong for this: the pump
     * sleeps for `next_exchange_ms` and the owner's own reopen normally ends
     * this reporter before it wakes, so the evidence would be discarded rather
     * than sent late. Restricted to callers holding evidence, so the ordinary
     * cadence is unchanged.
     */
    fun reportEvidence() {
        val capture = publish() ?: return
        val subject = reporter ?: return
        scope.launch { subject.notifyUrgently(scope, capture) }
    }

    /**
     * Publish a viewer destination before the media item or server session is
     * replaced. The pending target lives in [PlaybackIntent], so the snapshot
     * remains truthful when the replacement reporter starts as well.
     */
    suspend fun reportIntent(): Long? {
        // Read and map on the caller before the first suspension. Controller
        // actions await this enqueue before touching Media3, so neither a
        // dispatcher hop nor composition teardown can turn "before" into
        // "after". The reporter receives an immutable snapshot value.
        val capture = publish() ?: return null
        val subject = reporter ?: return null
        return subject.notifyUrgently(scope, capture)
    }

    /** Called synchronously by the player dispatcher; never by the reporter. */
    private fun publish(): PlaybackControlCapture? = synchronized(verdictLock) {
        val snapshot = observe?.invoke()?.let(PlaybackControlMapping::snapshot)
        val capture = snapshot?.let {
            PlaybackControlCapture(
                it,
                verdictIntentGeneration,
                PlaybackControlCaptureOwner(clientInstanceId, verdictGeneration),
                ++captureRevision,
            )
        }
        latest.set(capture)
        capture
    }

    /**
     * Stamp the current authority onto a snapshot the caller already holds.
     *
     * [publish] cannot serve the teardown exchange: it reads through `observe`,
     * and the caller closed that observation before calling — deliberately,
     * because every read past that point is against a player being released.
     * The identity is still this session's, so it is taken from the same place
     * [publish] takes it, under the same lock.
     */
    private fun captureOf(snapshot: PlaybackControlSnapshot): PlaybackControlCapture =
        synchronized(verdictLock) {
            val capture = PlaybackControlCapture(
                snapshot,
                verdictIntentGeneration,
                PlaybackControlCaptureOwner(clientInstanceId, verdictGeneration),
                ++captureRevision,
            )
            latest.set(capture)
            capture
        }

    fun end() {
        // Stopping the coroutine is asynchronous. Invalidate every callback
        // synchronously, including an exchange already returning from HTTP.
        dispatching = false
        val generation = synchronized(verdictLock) {
            latest.set(null)
            ++verdictGeneration
        }
        synchronized(answerLock) {
            answerGeneration = generation
            ownerChangesSeen += 1
        }
        val subject = reporter
        reporter = null
        observe = null
        if (subject != null) scope.launch { subject.stop() }
    }

    /**
     * Send one last exchange, then stop — on a scope that outlives the screen.
     *
     * [end] cannot do this and the difference is not a detail. `end()` queues a
     * `stop()`, and a `stop()` queued after an urgent notify still runs before
     * the pump it was meant to let finish: the notify sets `pending` and
     * launches `run()`, `stop()` clears `pending` and cancels that job, and the
     * exchange is never built. On the disposal path it is worse — the
     * composition's scope is cancelled in the same synchronous pass, so nothing
     * launched on it runs at all.
     *
     * That matters for exactly one thing: the terminal acknowledgement a
     * preparation is owed. Leaving it unsent hands the staging to the server's
     * 330 s deadline, which means a real encoder and an actor slot held for
     * five and a half minutes after the viewer closed the player.
     *
     * [finalSnapshot] is captured by the caller *before* it tears its player
     * down, because this exchange outlives that player.
     */
    fun endAfterFinalExchange(
        outerScope: CoroutineScope,
        finalSnapshot: PlaybackControlSnapshot,
        afterFinalExchange: () -> Unit,
    ) {
        dispatching = false
        val subject = reporter
        if (subject == null) {
            afterFinalExchange()
            return
        }
        // Stamped before the invalidation below, from the identity the reporter
        // was built with — it is answering for that session, not for whatever
        // replaces it.
        val settling = captureOf(finalSnapshot)
        // A commit that arrived on the player's ending snapshot is rewritten
        // to active for the CAS. Preserve the original end as a second exact
        // capture; the reporter publishes it only after the commit succeeds.
        val ending = endingSnapshotAfterSettlement(finalSnapshot)?.let(::captureOf)
        // The same synchronous invalidation [end] performs, and for the same
        // reason twice over. A reopen calls this and then begins a new session
        // immediately, so an exchange still returning from HTTP on the old
        // reporter must not reach a callback that would act on it — least of
        // all `onPrepare`, which builds a player. The exchange itself survives
        // the invalidation because a settled reporter reads what it was handed
        // rather than the slot this empties.
        //
        // What does not survive is the delivery callback for the
        // acknowledgement this carries: it is fenced by the generation being
        // bumped here, so the acknowledgement stays pending and may ride the
        // next session's first exchange as well. That duplicate is inert — the
        // server ignores an `action_id` not bound to the session it arrives on
        // — and the alternative is leaving a live reporter un-fenced across a
        // reopen, which is not a trade.
        val generation = synchronized(verdictLock) {
            latest.set(null)
            ++verdictGeneration
        }
        synchronized(answerLock) {
            answerGeneration = generation
            ownerChangesSeen += 1
        }
        reporter = null
        observe = null
        outerScope.launch {
            // `settle`, not `notifyUrgently`. The urgent path deliberately
            // leaves an in-flight exchange alone, because `run()` picks the
            // new snapshot up straight after it — but the pump running that
            // exchange is on the scope that is being cancelled right now, so
            // "straight after it" never arrives, and the coroutine dies inside
            // `send` without ever clearing `inFlight`. Re-homing
            // unconditionally is what makes this path work at all.
            try {
                val handed = subject.settle(outerScope, settling, ending)
                val deadline = monotonicNowMs() + FINAL_EXCHANGE_MS
                while (handed && monotonicNowMs() < deadline) {
                    val status = subject.status()
                    if (status.stopped) break
                    if (!status.inFlight && !status.pending && !status.retrying) break
                    kotlinx.coroutines.delay(ASK_POLL_MS)
                }
            } finally {
                subject.stop()
                // DELETE belongs after the final exchange. Calling it in
                // Controller immediately after this asynchronous hand-off let
                // DELETE retire the predecessor before the commit CAS reached
                // the server.
                afterFinalExchange()
            }
        }
    }

    private companion object {
        /**
         * How often the ask looks. Short enough that it costs a stalled viewer
         * nothing measurable, long enough that it is not a spin.
         *
         * `monotonicNowMs` rather than a wall clock, and rather than
         * `SystemClock`: this file's own telemetry uses it, it cannot be moved
         * by a clock adjustment mid-ask, and it works in the JVM unit lane
         * where the Android framework stubs throw.
         */
        const val ASK_POLL_MS = 25L

        /**
         * How long a teardown waits for its last exchange. Bounded because the
         * viewer has already left: past this the server's own deadline is the
         * fallback, which is the behaviour without this path at all.
         */
        const val FINAL_EXCHANGE_MS = 3_000L
    }

}
