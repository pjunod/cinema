package tv.plurx.app.player

import tv.plurx.app.data.PlaybackQuality

/**
 * The client half of a prepared replacement, with no player in it.
 *
 * A prepared handoff is a transaction: the server stages a successor and
 * names it once, the client primes a second pipeline, reports how far it got,
 * switches, and settles. Every rule about *when* an acknowledgement may be sent
 * and *which* one lives here rather than beside the ExoPlayer instances,
 * because none of those rules needs a player to be wrong. Android advertises
 * the path by default; Settings → Developer owns the explicit opt-out and
 * shows device/session evidence as advice rather than changing that choice.
 *
 * `docs/playback-control/M6-CLIENT-REPLACEMENT-CONTRACT.md` §C7 and §C8 are what this encodes.
 */

/**
 * How long a successor gets to become playable before it is abandoned.
 *
 * Short enough that a viewer never waits on a preparation that is not going to
 * arrive, and far inside the server's 330 s reaping deadline so the client is
 * always the one that settles the transaction rather than the deadline.
 */
internal const val PREPARED_READINESS_BOUND_MS = 20_000L

/**
 * How much contiguous runway the successor must hold *ahead of the incumbent's
 * current film position* before it is worth switching to.
 *
 * The switch is only better than the fallback if the successor can carry the
 * viewer through the moment the predecessor stops. Below this the honest answer
 * is to keep priming; a switch onto a pipeline with nothing behind it buys a
 * shorter interruption than the fallback's measured 353–766 ms only by luck.
 */
internal const val PREPARED_SWITCH_RUNWAY_MS = 3_000L

/** Maximum film-time error accepted after the final pre-commit seek. */
internal const val PREPARED_ALIGNMENT_SLACK_MS = 250L

/**
 * How long the viewer's tap waits for the server to offer a successor before
 * the ordinary reopen takes over.
 *
 * Measured from the tap rather than from the exchange that carried it, because
 * the viewer's wait began at the tap: the dispatch exchange, the server's own
 * admission and the first `prepare` all happen inside this number, and a bound
 * anchored anywhere later would hide most of what the viewer is waiting for.
 *
 * It is also the only thing that decides on a server too old to send
 * `delivery.preparation` at all — such a server says nothing either way, so
 * silence is read as "still staging" and this bound is what ends the wait.
 */
internal const val PREPARED_OFFER_BOUND_MS = 12_000L

/** How often the wait re-asks while the server says it is still staging. */
internal const val PREPARED_OFFER_STAGING_CADENCE_MS = 1_000L

/** `delivery.preparation`: a successor is being primed; ask again shortly. */
internal const val PREPARATION_STAGING = "staging"

/** `delivery.preparation`: the `prepare` action is on this exchange. */
internal const val PREPARATION_OFFERED = "offered"

/**
 * `delivery.preparation`: this server will not prepare for this change.
 *
 * Only ever meaningful when the server actually said it. **Absence is not this
 * value** — an older server and every relay in front of one omit the field, and
 * reading that omission as a decline would turn every such server's rung change
 * into an instant reopen while the successor it was priming went to waste.
 */
internal const val PREPARATION_NONE = "none"

/**
 * One exchange's answer, reduced to what the wait decides on.
 *
 * A value rather than the [ControlResponse] it came from: the wait is the one
 * piece of this path the JVM unit lane can drive, and it must not need a
 * transport, a reporter or a player to be exercised.
 */
internal data class ControlAnswer(
    /** The sequence of the request this answered, for the floor comparison. */
    val requestSequence: Long,
    val action: ControlAction?,
    /** `delivery.preparation`, null on a server or relay that never sends it. */
    val preparation: String?,
)

/**
 * The viewer tapped a different rung; wait for the server to offer a successor.
 *
 * Pure, and deliberately in this file rather than in `Controller.kt`: every M6
 * defect that survived review lived in the controller, where no unit test can
 * reach. The five rules below are the whole decision, and each of them is one
 * `observe` call away from a test.
 *
 * Not thread-safe: the session drives it from one coroutine.
 */
internal class PreparedOfferWait(
    private val tappedAtMs: Long,
    /** The reporter's counter at the tap; answers below it are not ours. */
    private val floorSequence: Long,
    private val boundMs: Long = BOUND_MS,
    private val stagingCadenceMs: Long = STAGING_CADENCE_MS,
) {
    sealed interface Step {
        /** Nothing decided yet. [nextExchangeMs] is how long to wait before looking again. */
        data class KeepWaiting(val nextExchangeMs: Long) : Step

        /** The server named a successor. */
        data class Offered(val action: ControlAction) : Step

        /** Take the ordinary reopen. [reason] is `declined` or `timed_out`. */
        data class Reopen(val reason: String) : Step
    }

    /**
     * Whether an answer at or above the floor has already been seen.
     *
     * The exchange that carried the ask is answered before the server has
     * decided anything: a current server says `staging` on it and an older one
     * says nothing at all. Only a *later* exchange's `none` is a decline, so
     * this flag is what separates "the server has not decided yet" from "the
     * server decided no".
     */
    var sawAcceptedAnswer: Boolean = false
        private set

    /** The last `delivery.preparation` this wait accepted, for the dev tab. */
    var lastPreparation: String? = null
        private set

    fun observe(answer: ControlAnswer?, nowMs: Long): Step {
        // First, and before an exchange has ever been accepted. A server that
        // never answers, a reporter that died, a relay that swallowed the
        // dispatch — none of them produce an answer to read, and the viewer is
        // waiting through every one of them.
        if (nowMs - tappedAtMs >= boundMs) return Step.Reopen("timed_out")
        if (answer == null) return Step.KeepWaiting(stagingCadenceMs)
        // An answer to a request older than the tap cannot be about the tap.
        if (answer.requestSequence < floorSequence) return Step.KeepWaiting(stagingCadenceMs)
        val action = answer.action
        // The offer, whatever the server said alongside it. `preparation` is
        // advice about a decision; a `prepare` action *is* the decision.
        if (action != null && action.type == PlaybackControl.PREPARE_ACTION_TYPE) {
            lastPreparation = answer.preparation ?: PREPARATION_OFFERED
            return Step.Offered(action)
        }
        lastPreparation = answer.preparation
        val first = !sawAcceptedAnswer
        sawAcceptedAnswer = true
        // A decline, but never on the exchange that carried the ask: that one
        // is answered before the server has looked, and on an old server it is
        // answered with nothing at all.
        if (answer.preparation == PREPARATION_NONE && !first) return Step.Reopen("declined")
        // `staging`, `offered` without the action yet, an unknown word from a
        // newer server, or the silence of an older one. All of them mean the
        // same thing here: keep waiting, and let the bound decide.
        return Step.KeepWaiting(stagingCadenceMs)
    }

    companion object {
        const val BOUND_MS = PREPARED_OFFER_BOUND_MS
        const val STAGING_CADENCE_MS = PREPARED_OFFER_STAGING_CADENCE_MS
    }
}

/**
 * The viewer's directed change, alive from the tap until it is honoured.
 *
 * Before this, a preparation that failed after the offer released the successor
 * and acknowledged `failed`, and the rung the viewer asked for was never
 * applied — the tap simply evaporated. One owner, from the tap to either a
 * commit or exactly one ordinary reopen, is the whole of the fix.
 *
 * Not thread-safe: [Controller] owns it and touches it from the player's scope.
 */
internal class DirectedChange(val epoch: Long, val quality: PlaybackQuality) {
    private var settled = false

    /** True once this change can no longer produce a reopen. */
    val isSettled: Boolean get() = settled

    /** The successor is on the screen. Nothing more is owed. */
    fun committed() {
        settled = true
    }

    /**
     * Any terminal failure of the offered successor, or the wait ending
     * without an offer: exactly one ordinary reopen, if this change is
     * still the viewer's current intent. Returns whether it routed.
     */
    fun fallBackOnce(reason: String, currentEpoch: Long, route: () -> Unit): Boolean {
        if (settled) return false
        // The media moved on under this change — another quality tap, a title
        // change, a reopen someone else already took. Routing now would drag
        // the viewer back to a rung they have since left.
        if (currentEpoch != epoch) {
            settled = true
            return false
        }
        settled = true
        route()
        return true
    }

    /** A newer viewer action owns the stream now. This one says nothing. */
    fun superseded() {
        settled = true
    }
}

/**
 * How far ahead of the incumbent the successor is parked.
 *
 * Long enough to cover a cold seek on the slowest measured device — the
 * tunneled Google TV of M5.5 — with margin, and short enough that the viewer's
 * tap is honoured within a couple of seconds. It is a *lead*, not a chase: the
 * successor is parked at this point with `playWhenReady = false` and waits for
 * the incumbent to arrive. Letting it run instead does not converge, because
 * two pipelines at the same rate keep whatever gap the seek landed with.
 */
internal const val RENDEZVOUS_LEAD_MS = 1_500L

/** How often the hold looks again while the incumbent is not advancing. */
internal const val RENDEZVOUS_PAUSED_POLL_MS = 500L

/**
 * How often the hold looks for the successor to have landed on the rendezvous.
 *
 * Its own cadence rather than the one-second ladder tick. At 2x a 1 500 ms lead
 * is 750 ms of wall time, so a sampler that looks once a second discovers the
 * successor has arrived only after the incumbent has already gone past — and
 * turns every fast-rate handoff into a re-park.
 */
internal const val RENDEZVOUS_READY_POLL_MS = 100L

/**
 * How many times a missed rendezvous may be re-picked before the prepared path
 * gives up and the directed change takes its one ordinary reopen.
 */
internal const val RENDEZVOUS_MAX_REPARKS = 2

/**
 * The arithmetic of meeting the successor, with no player in it.
 *
 * Two clocks drive this and they are different kinds of thing: *film* time,
 * which both pipelines measure positions in, and *wall* time, which is what a
 * coroutine delay and a seek are measured in. Keeping them apart is the point —
 * at a rate of 0.5x a 1 500 ms lead is 3 000 ms of waiting, and at 2x it is
 * 750 ms.
 *
 * Not thread-safe: the controller drives it from the player's scope.
 */
internal class RendezvousHold(
    private val leadMs: Long = RENDEZVOUS_LEAD_MS,
    private val slackMs: Long = PREPARED_ALIGNMENT_SLACK_MS,
    private val maxReparks: Int = RENDEZVOUS_MAX_REPARKS,
    private val pausedPollMs: Long = RENDEZVOUS_PAUSED_POLL_MS,
) {
    /**
     * Where the successor is sent and whether it runs once it gets there.
     *
     * [playWhenReady] is false and that is the whole design. A successor that
     * plays from the lead point keeps the lead forever.
     */
    data class Park(val rendezvousFilmMs: Long, val playWhenReady: Boolean)

    sealed interface Step {
        /** The incumbent is at the rendezvous and the successor is waiting there. */
        data class Commit(val filmMs: Long) : Step

        /** Not there yet. Look again in [delayMs] of wall time. */
        data class Wait(val delayMs: Long) : Step

        /** The rendezvous was missed. Seek the successor to the new one. */
        data class Repark(val park: Park) : Step

        /** Out of attempts. The directed change takes its ordinary reopen. */
        data class Abandon(val reason: String) : Step
    }

    var rendezvousFilmMs: Long? = null
        private set
    var reparks: Int = 0
        private set

    /**
     * How long the last seek to a rendezvous actually took, in wall time.
     *
     * Measured rather than assumed. [leadMs] is an estimate of a device class;
     * this is what *this* device just did, and a re-park that ignored it would
     * pick a lead the device has already been observed to miss.
     */
    var observedSeekMs: Long = 0L
        private set

    private var parkIssuedAtMs = 0L
    private var ready = false

    /** Pick the first rendezvous and send the successor to it, parked. */
    fun park(nowMs: Long, incumbentFilmMs: Long): Park = repark(nowMs, incumbentFilmMs)

    /** The successor's seek landed and it holds runway through the rendezvous. */
    fun ready(nowMs: Long) {
        if (ready) return
        ready = true
        observedSeekMs = (nowMs - parkIssuedAtMs).coerceAtLeast(0L)
    }

    val isReady: Boolean get() = ready

    /**
     * Wall time until the incumbent reaches the rendezvous.
     *
     * A paused or stopped incumbent never arrives on its own, so the hold looks
     * again on a fixed cadence instead of dividing by zero. That is the whole
     * of "a viewer pause postpones the fire": the rendezvous does not move, the
     * successor stays parked on it, and the delay is recomputed from whatever
     * rate the incumbent has when play resumes.
     */
    fun delayMs(incumbentFilmMs: Long, speed: Double): Long {
        val target = rendezvousFilmMs ?: return pausedPollMs
        if (!speed.isFinite() || speed <= 0.0) return pausedPollMs
        val remaining = target - incumbentFilmMs
        if (remaining <= 0) return 0L
        return (remaining / speed).toLong().coerceAtLeast(0L)
    }

    /**
     * The scheduled moment arrived. Decide from fresh readings of both clocks.
     *
     * [successorFilmMs] is compared against the incumbent and not only against
     * the rendezvous, because those are the same number only when the successor
     * really did park. A successor that ran keeps its lead, and this is where
     * that shows up as a rendezvous that is never accepted.
     */
    fun fire(
        nowMs: Long,
        incumbentFilmMs: Long,
        successorFilmMs: Long,
        successorReady: Boolean,
        speed: Double,
    ): Step {
        val target = rendezvousFilmMs ?: return Step.Repark(repark(nowMs, incumbentFilmMs))
        // Short of the rendezvous: the incumbent is still on its way, or it is
        // paused and will resume. Neither is a miss; recompute and wait.
        if (incumbentFilmMs < target - slackMs) return Step.Wait(delayMs(incumbentFilmMs, speed))
        val aligned = kotlin.math.abs(incumbentFilmMs - target) <= slackMs &&
            kotlin.math.abs(successorFilmMs - incumbentFilmMs) <= slackMs
        if (aligned && successorReady) return Step.Commit(incumbentFilmMs)
        // At or past the rendezvous without a successor waiting there: a seek
        // that took longer than the lead, a forward seek, or a rate change.
        if (reparks >= maxReparks) return Step.Abandon("rendezvous_missed")
        return Step.Repark(repark(nowMs, incumbentFilmMs))
    }

    private fun repark(nowMs: Long, incumbentFilmMs: Long): Park {
        if (rendezvousFilmMs != null) reparks += 1
        // The lead for the next attempt is the larger of the device-class
        // estimate and what this device's last seek actually cost, so a second
        // attempt is never aimed at a point the first one already proved is too
        // close. Slack on top, because the seek still has to land *before* the
        // incumbent arrives rather than with it.
        val lead = maxOf(leadMs, observedSeekMs + slackMs)
        val target = incumbentFilmMs + lead
        rendezvousFilmMs = target
        parkIssuedAtMs = nowMs
        ready = false
        return Park(target, playWhenReady = false)
    }
}

/**
 * What the developer tab says about the last rung change, and nothing else.
 *
 * Advisory in the strict sense Paul's standing rule means: nothing reads these
 * to decide anything. They exist so that a change that reopened instead of
 * handing over leaves a trace a person can read, on a screen that is not inside
 * a playback session and so cannot ask the controller.
 *
 * A process-wide holder because Settings and the player never share an object:
 * the controller lives for one playback and the settings screen is opened
 * afterwards, which is exactly when the answer is wanted.
 */
internal object PreparedReplacementAdvisory {
    data class Advice(
        /** `committed <ms>`, `declined`, `timed out`, `fell back`, or null. */
        val outcome: String? = null,
        /** The last `delivery.preparation` the server sent, verbatim. */
        val preparation: String? = null,
    )

    @Volatile
    var advice: Advice = Advice()
        private set

    fun recordOutcome(outcome: String) {
        advice = advice.copy(outcome = outcome)
    }

    fun recordPreparation(preparation: String?) {
        if (preparation == null) return
        advice = advice.copy(preparation = preparation)
    }

    /** How a `via=` tag becomes the sentence the row shows. */
    fun outcomeLabel(via: String, elapsedMs: Long?): String = when (via) {
        "prepared" -> "committed" + (elapsedMs?.let { " ${it}ms" } ?: "")
        "declined" -> "declined"
        "timed_out" -> "timed out"
        else -> "fell back"
    }
}

/**
 * How long a commit waits for the successor's own first rendered frame before
 * failing the prepared path and taking the ordinary reopen.
 *
 * A prepared successor has no surface, so it renders nothing until the switch
 * puts it on one; `first_frame_unix_ms` is therefore always a moment *after*
 * the swap. If the frame never comes the switch still happened — the viewer is
 * looking at the successor either way — but inventing a frame would commit the
 * server pointer to a black pipeline and drain the working predecessor. A
 * timeout therefore restores the retained predecessor, reports `failed`, and
 * only then reopens through the normal path.
 */
internal const val PREPARED_COMMIT_FRAME_BOUND_MS = 5_000L

/**
 * How long a retired predecessor may sit parked before the watchdog collects it
 * without waiting for the composition.
 *
 * The surface owner normally collects it within a frame, which is the only
 * deterministic answer and the fast one. But a recomposer stops issuing frames
 * whenever the window is not visible, and a paused ExoPlayer still holds its
 * renderers — so a couple of ticks is the bound on how long two live decoders
 * may overlap on a device class M5.5 measured as failing at exactly two.
 */
internal const val PREPARED_RETIRED_COLLECT_MS = 2_500L

/** Where a preparation is on the ladder. Terminal states are absorbing. */
internal enum class PreparationPhase {
    /** Named by the server, pipeline being built. Nothing reported yet. */
    STAGED,
    METADATA_READY,
    BUFFER_READY,

    /**
     * The successor is on the surface and the predecessor is gone, but the
     * commit has not been sent because it is still owed the wall clock of a
     * frame the successor actually rendered.
     *
     * Not terminal — a terminal state is still owed — and **not abortable**:
     * the viewer is looking at this pipeline. Without this phase the window
     * between the swap and the first frame is one where a Back press or a seek
     * publishes `aborted` for the staging the client is at that moment playing,
     * which tells the server to tear down the incarnation its pointer is about
     * to move to.
     */
    SWITCHED,
    COMMITTED,
    FAILED,
    ABORTED,
    ;

    val isTerminal: Boolean
        get() = this == COMMITTED || this == FAILED || this == ABORTED
}

/** What [PreparedReplacementLedger.offer] decided about an inbound `prepare`. */
internal sealed class PreparationOffer {
    /**
     * The same `action_id` as the live preparation. The server replays a
     * staging byte-identically on every exchange until it settles, so this is
     * the ordinary case and it must build nothing: one preparation, one
     * pipeline.
     */
    data object Same : PreparationOffer()

    /**
     * A new preparation. [supersedes] is the acknowledgement owed to the one it
     * replaces — `aborted`, because the client abandoned it — and is null when
     * there was nothing live.
     */
    data class Start(
        val action: ControlAction,
        val supersedes: ActionAcknowledgement?,
    ) : PreparationOffer()

    /**
     * The payload did not validate. The reporter refuses such an action before
     * this is ever reached; this arm exists so a caller that reaches the ledger
     * by another route cannot prime a pipeline on it either.
     */
    data object Refuse : PreparationOffer()
}

/**
 * One live preparation at a time, and the acknowledgements it owes.
 *
 * Not thread-safe by design: it is owned by [Controller], which touches it only
 * from the player's own scope.
 */
internal class PreparedReplacementLedger {
    var actionId: String? = null
        private set
    var phase: PreparationPhase = PreparationPhase.ABORTED
        private set
    var action: ControlAction? = null
        private set

    /** True while a preparation is live and still owed a terminal state. */
    val isLive: Boolean
        get() = actionId != null && !phase.isTerminal

    /**
     * True once the switch has happened and only the commit is outstanding.
     * Nothing may abandon a preparation in this state.
     */
    val isSwitched: Boolean
        get() = phase == PreparationPhase.SWITCHED

    fun offer(inbound: ControlAction): PreparationOffer {
        if (!inbound.preparedPayloadIsValid) return PreparationOffer.Refuse
        // Nothing replaces a preparation the viewer is already watching. Without
        // this the switched-but-unsettled window takes the "supersedes" branch,
        // where `terminal()` correctly refuses to abort it and the branch then
        // reads that refusal as "nothing was owed" — silently discarding a
        // commit and stamping the *next* preparation's id with it.
        if (isSwitched) return PreparationOffer.Refuse
        // Identity first, liveness second. The server replays a staging until
        // it settles, and an exchange that was already in flight when the
        // client settled comes back carrying that same `action_id` — so
        // checking liveness first makes the *ordinary commit* look like a new
        // preparation and builds a third pipeline over the one the viewer is
        // watching. A repeat of an id this ledger has seen is the same
        // preparation whatever became of it.
        if (inbound.actionId == actionId) return PreparationOffer.Same
        // A live preparation the viewer never got is abandoned explicitly.
        // Leaving it to the 330 s deadline holds this session's one preparation
        // slot for the rest of the session, so it gets exactly one, ever.
        val owed = if (isLive) terminal(PreparationPhase.ABORTED) else null
        actionId = inbound.actionId
        action = inbound
        phase = PreparationPhase.STAGED
        return PreparationOffer.Start(inbound, owed)
    }

    /**
     * The successor is playable and its tracks are known.
     *
     * Progress states are optional to the server and are sent anyway: they are
     * how an operator sees how far a preparation got before it died. Null when
     * the ladder has already passed this rung — progress is monotonic, and the
     * server rejects a step backwards by ignoring it.
     */
    fun metadataReady(): ActionAcknowledgement? {
        if (phase != PreparationPhase.STAGED) return null
        phase = PreparationPhase.METADATA_READY
        return acknowledgement(AcknowledgementState.METADATA_READY)
    }

    /**
     * The successor holds enough runway to be worth switching to.
     *
     * [bufferedThroughMs] is film time, not player time: the commit boundary is
     * expressed in film time and so is this.
     */
    fun bufferReady(bufferedThroughMs: Long): ActionAcknowledgement? {
        if (phase != PreparationPhase.STAGED && phase != PreparationPhase.METADATA_READY) {
            return null
        }
        if (bufferedThroughMs < 0) return null
        phase = PreparationPhase.BUFFER_READY
        return acknowledgement(
            AcknowledgementState.BUFFER_READY,
            bufferedThroughMs = bufferedThroughMs.coerceAtMost(PlaybackControl.MAX_MEDIA_MILLIS),
        )
    }

    /**
     * The switch happened: the successor took the surface and the volume.
     *
     * Separate from [committed] because the commit is owed a rendered frame
     * that cannot have happened yet, and the gap between the two is a window
     * nothing may abort.
     */
    fun switched(): Boolean {
        if (!isLive || isSwitched) return false
        phase = PreparationPhase.SWITCHED
        return true
    }

    /**
     * The successor rendered, and the transaction can settle.
     *
     * [firstFrameUnixMs] must be the wall clock of a frame the successor
     * actually rendered. A timer would answer the question the server asked
     * with a number about something else.
     */
    fun committed(firstFrameUnixMs: Long): ActionAcknowledgement? {
        if (!isLive) return null
        if (firstFrameUnixMs <= 0) return null
        // Read off the offer, not off the player. The server compares this
        // against the staged successor's own `media_origin_ms` and refuses a
        // mismatch — that refusal is what makes a commit evidence, and a number
        // this client derived for itself would only ever agree with itself.
        val origin = action?.mediaOriginMs ?: return null
        phase = PreparationPhase.COMMITTED
        return acknowledgement(
            AcknowledgementState.COMMITTED,
            firstFrameUnixMs = firstFrameUnixMs,
            committedMediaOriginMs = origin,
        )
    }

    /**
     * The successor could not be made ready.
     *
     * Failing while still [PreparationPhase.STAGED] means it was never playable
     * at all — the pipeline errored or the readiness bound elapsed before a
     * single track was published — which is a fact about this playback rather
     * than about this attempt. Failing later, from a rung it had reached, is
     * an ordinary failure and the next offer is still worth taking.
     */
    fun failed(): ActionAcknowledgement? {
        return terminal(PreparationPhase.FAILED)
    }

    /**
     * The surface moved, but the successor never rendered a frame.
     *
     * This is the only terminal path out of SWITCHED besides a real commit.
     * It exists separately because ordinary abort/failure must never tear down
     * a successor the viewer is already watching, while a first-frame timeout
     * must settle the server staging without fabricating presentation proof.
     */
    fun failedAfterSwitch(): ActionAcknowledgement? {
        if (!isSwitched) return null
        phase = PreparationPhase.FAILED
        return acknowledgement(AcknowledgementState.FAILED)
    }

    /** The viewer seeked, changed quality again, or left. */
    fun aborted(): ActionAcknowledgement? = terminal(PreparationPhase.ABORTED)

    private fun terminal(state: PreparationPhase): ActionAcknowledgement? {
        if (!isLive) return null
        // You cannot abandon what is already on the screen. A switched
        // preparation is settled by its commit or by nothing.
        if (isSwitched) return null
        phase = state
        return acknowledgement(
            when (state) {
                PreparationPhase.FAILED -> AcknowledgementState.FAILED
                else -> AcknowledgementState.ABORTED
            },
        )
    }

    private fun acknowledgement(
        state: AcknowledgementState,
        bufferedThroughMs: Long? = null,
        firstFrameUnixMs: Long? = null,
        committedMediaOriginMs: Long? = null,
    ): ActionAcknowledgement? {
        val id = actionId ?: return null
        return ActionAcknowledgement(
            actionId = id,
            state = state,
            bufferedThroughMs = bufferedThroughMs,
            committedMediaOriginMs = committedMediaOriginMs,
            firstFrameUnixMs = firstFrameUnixMs,
        ).takeIf { it.isValid }
    }
}

/**
 * Whether the successor has primed far enough to be switched to.
 *
 * [successorBufferedThroughMs] and [incumbentPositionMs] are both film time, so
 * this is one subtraction and not a timeline conversion — which is exactly why
 * `media_origin_ms` is on the action at all.
 */
internal fun successorIsBuffered(
    successorBufferedThroughMs: Long,
    incumbentPositionMs: Long,
    runwayMs: Long = PREPARED_SWITCH_RUNWAY_MS,
): Boolean = successorBufferedThroughMs - incumbentPositionMs >= runwayMs

/**
 * What the dev-tab enable section says about this device, and why.
 *
 * Advisory, never a gate: the switch enables the capability whatever these say.
 * The point of showing them is that a viewer who turns it on can see which of
 * the conditions M5.5 measured their device actually meets, rather than
 * discovering it as a stall.
 */
internal data class PreparedReplacementRequirement(
    val label: String,
    /**
     * Null where this screen cannot honestly answer.
     *
     * Two of the four conditions are properties of a *session*, and Settings is
     * not inside one. Reporting them as unmet would read as "your device
     * fails", which is a different claim; reporting them as met would be a
     * guess. Unknown is the true answer and the row still says what the
     * condition is.
     */
    val met: Boolean?,
    val detail: String,
) {
    val status: String
        get() = when (met) {
            true -> "Met"
            false -> "Not met"
            null -> "Checked during playback"
        }
}

/**
 * The requirement list, derived rather than typed twice.
 *
 * [isTelevision] is the axis M5.5's only observed hard failure sat on: both
 * phones passed dual prime 20/20, and the tunneled Google TV failed it 0/3
 * with two successor-prime timeouts and an `ERROR_CODE_AUDIO_TRACK_WRITE_FAILED`.
 * [tunnelingEnabled] is the suspected cause rather than a measured one: the
 * re-run that would name it (same-codec dual prime with tunneling forced off)
 * has not been taken.
 */
internal fun preparedReplacementRequirements(
    isTelevision: Boolean,
    tunnelingEnabled: Boolean,
    sessionIsLive: Boolean?,
    observedDownloadBps: Long?,
    throughputKnown: Boolean = true,
): List<PreparedReplacementRequirement> = listOf(
    PreparedReplacementRequirement(
        label = "Device class measured to pass",
        met = !isTelevision,
        detail = if (isTelevision) {
            "Televisions are the measured class that failed. A tunneled Google " +
                "TV failed dual preparation 0 of 3. Phones passed 20 of 20. " +
                "This observation is advice, not an enablement gate."
        } else {
            "Both measured phones passed same-codec and codec/HDR dual " +
                "preparation, 20 of 20."
        },
    ),
    PreparedReplacementRequirement(
        label = "Tunneled decoding off",
        met = !tunnelingEnabled,
        detail = if (tunnelingEnabled) {
            "Tunneling was on for the failure. Two identical tunneled pipelines " +
                "appear to contend where two different codecs get distinct " +
                "decoder instances. Suspected, not measured: the re-run that " +
                "would settle it has not been taken."
        } else {
            "Not requested on this device, so two pipelines get distinct " +
                "decoder instances."
        },
    ),
    PreparedReplacementRequirement(
        label = "Server delivery telemetry",
        met = sessionIsLive,
        detail = when (sessionIsLive) {
            true -> "The server is reporting delivered throughput for this " +
                "session. It informs fleet analysis but does not gate handoff."
            false -> "This server reports no delivered throughput for the " +
                "session you are watching. Preparation remains eligible; the " +
                "fleet evidence is incomplete."
            null -> "Settings has no active session to inspect. The server " +
                "records delivery telemetry during playback when available; " +
                "unknown evidence does not disable this switch."
        },
    ),
    PreparedReplacementRequirement(
        label = "Throughput reported",
        met = if (throughputKnown) observedDownloadBps != null && observedDownloadBps > 0 else null,
        detail = when {
            !throughputKnown ->
                "This device counts bytes off the wire over a rolling window " +
                    "and reports the rate on every exchange. The reading is " +
                    "advisory and is not a handoff admission rule."
            observedDownloadBps != null && observedDownloadBps > 0 ->
                "Measuring ${observedDownloadBps / 1_000_000} Mbit/s off the " +
                    "wire. This is fleet evidence, not permission to prepare."
            else ->
                "Nothing measured yet. The explicit client capability and real " +
                    "resource outcomes still decide whether an attempt proceeds."
        },
    ),
)

/**
 * The delivered grade after a prepared handoff.
 *
 * Lifted out of [Controller] so the rule has one statement and one test rather
 * than being spelled at each call site. `adoptSessionDelivery`'s own doc makes
 * the argument for that: two independent `?.let`s keep the predecessor's Dolby
 * Vision profile through a change of grade and paint "SDR - Profile 8" over a
 * handover to a transcode, and each half looks correct on its own.
 *
 * An [EffectiveSelection] carries no profile field, so when the grade moves the
 * only honest profile is "no answer". When the successor names no grade,
 * neither half moves.
 */
internal data class DeliveredGrade(
    val range: String?,
    val dolbyVisionProfile: Int?,
)

internal fun adoptedGrade(
    current: DeliveredGrade,
    successor: EffectiveSelection?,
): DeliveredGrade {
    val range = successor?.dynamicRange ?: return current
    return DeliveredGrade(range, null)
}

/**
 * The snapshot a teardown settles a commit on.
 *
 * A viewer who closes at the end of a title maps to `demand: end`, and an
 * ending exchange may not carry a `committed` — the pairing is a `400` that
 * costs both. This exchange is not what ends the session (`endHlsSession` is,
 * on its own route), so it does not claim to.
 *
 * The rate moves with the demand because the mapper derives one from the other:
 * a held player reports the rate it actually has, which at the end of a title is
 * zero, and the server refuses `active` below 0.25 outright — refusing the whole
 * exchange and the commit with it. `render_state` is left alone, so the one
 * field still saying what the player is doing goes on saying it.
 */
internal fun settlingSnapshot(snapshot: PlaybackControlSnapshot): PlaybackControlSnapshot {
    if (snapshot.acknowledgement?.state != AcknowledgementState.COMMITTED) return snapshot
    if (snapshot.demand != PlaybackDemand.END) return snapshot
    return snapshot.copy(
        demand = PlaybackDemand.ACTIVE,
        playbackRate = maxOf(PlaybackControlMapping.MIN_ACTIVE_RATE, snapshot.playbackRate),
    )
}

/** The terminal exchange owed after an ending commit is accepted. */
internal fun endingSnapshotAfterSettlement(
    snapshot: PlaybackControlSnapshot,
): PlaybackControlSnapshot? {
    if (snapshot.acknowledgement?.state != AcknowledgementState.COMMITTED) return null
    if (snapshot.demand != PlaybackDemand.END) return null
    return snapshot.copy(acknowledgement = null)
}
