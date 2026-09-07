package tv.plurx.app.player

/**
 * The client half of a prepared replacement, with no player in it.
 *
 * A prepared handoff is a transaction: the server stages a successor and
 * names it once, the client primes a second pipeline, reports how far it got,
 * switches, and settles. Every rule about *when* an acknowledgement may be sent
 * and *which* one lives here rather than beside the ExoPlayer instances,
 * because none of those rules needs a player to be wrong — and on Android the
 * whole path is unreachable in production today (see
 * [controlCapabilities]'s `dualPlayerPreparation`), so a test is the only place
 * it is ever exercised.
 *
 * `docs/M6-CLIENT-REPLACEMENT-CONTRACT.md` §C7 and §C8 are what this encodes.
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

/**
 * How long a commit waits for the successor's own first rendered frame before
 * settling with the wall clock instead.
 *
 * A prepared successor has no surface, so it renders nothing until the switch
 * puts it on one; `first_frame_unix_ms` is therefore always a moment *after*
 * the swap. If the frame never comes the switch still happened — the viewer is
 * looking at the successor either way — so the commit goes out with a less
 * precise number rather than not at all, which would hold the session's one
 * preparation slot until the server's 330 s deadline.
 */
internal const val PREPARED_COMMIT_FRAME_BOUND_MS = 5_000L

/** Where a preparation is on the ladder. Terminal states are absorbing. */
internal enum class PreparationPhase {
    /** Named by the server, pipeline being built. Nothing reported yet. */
    STAGED,
    METADATA_READY,
    BUFFER_READY,
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

    fun offer(inbound: ControlAction): PreparationOffer {
        if (!inbound.preparedPayloadIsValid) return PreparationOffer.Refuse
        if (isLive && inbound.actionId == actionId) return PreparationOffer.Same
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
     * The successor is on the surface and audible and the predecessor is gone.
     *
     * [firstFrameUnixMs] must be the wall clock of a frame the successor
     * actually rendered. A timer would answer the question the server asked
     * with a number about something else.
     */
    fun committed(firstFrameUnixMs: Long): ActionAcknowledgement? {
        if (!isLive) return null
        if (firstFrameUnixMs <= 0) return null
        phase = PreparationPhase.COMMITTED
        return acknowledgement(
            AcknowledgementState.COMMITTED,
            firstFrameUnixMs = firstFrameUnixMs,
        )
    }

    /** The successor could not be made ready. */
    fun failed(): ActionAcknowledgement? = terminal(PreparationPhase.FAILED)

    /** The viewer seeked, changed quality again, or left. */
    fun aborted(): ActionAcknowledgement? = terminal(PreparationPhase.ABORTED)

    private fun terminal(state: PreparationPhase): ActionAcknowledgement? {
        if (!isLive) return null
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
    ): ActionAcknowledgement? {
        val id = actionId ?: return null
        return ActionAcknowledgement(id, state, bufferedThroughMs, firstFrameUnixMs)
            .takeIf { it.isValid }
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
 * [isTelevision] is the axis M5.5's only hard failure sat on: both phones
 * passed same-codec dual prime 20/20, and the tunneled Google TV failed it 0/3
 * with two successor-prime timeouts and an `ERROR_CODE_AUDIO_TRACK_WRITE_FAILED`
 * — and same-codec is the only kind of change the server ever prepares.
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
            "Televisions are the class that failed. A tunneled Google TV failed " +
                "same-codec dual preparation 0 of 3 — and same-codec is the only " +
                "change the server ever prepares. Phones passed 20 of 20."
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
        label = "Live session",
        met = sessionIsLive,
        detail = when (sessionIsLive) {
            true -> "The server reports delivered throughput for live and " +
                "live-recovery sessions, which the preparation floor needs."
            false -> "The server leaves delivered throughput unreported on " +
                "every VOD session, and the floor needs both numbers — so a " +
                "prepared handoff cannot fire on VOD today, however capable " +
                "this device is. Nothing on this screen changes that."
            null -> "A prepared handoff can only fire on a live or " +
                "live-recovery session. The server leaves delivered " +
                "throughput unreported on every VOD session, and the " +
                "preparation floor needs that number, so turning this on will " +
                "change nothing while you are watching a film."
        },
    ),
    PreparedReplacementRequirement(
        label = "Throughput reported",
        met = if (throughputKnown) observedDownloadBps != null && observedDownloadBps > 0 else null,
        detail = when {
            !throughputKnown ->
                "This device counts bytes off the wire over a rolling window " +
                    "and reports the rate on every exchange. The server wants " +
                    "at least twice what the session is already delivering " +
                    "before it will prime a second pipeline."
            observedDownloadBps != null && observedDownloadBps > 0 ->
                "Measuring ${observedDownloadBps / 1_000_000} Mbit/s off the " +
                    "wire. The server wants at least twice what this session " +
                    "is already delivering."
            else ->
                "Nothing measured yet. A client that reports no throughput is " +
                    "never offered a preparation, whatever its capability says."
        },
    ),
)
