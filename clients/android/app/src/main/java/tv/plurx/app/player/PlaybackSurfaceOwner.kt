package tv.plurx.app.player

/**
 * The recovery owner's surface arm.
 *
 * [PlaybackSurfaceReducer] is the presenter and is pure. This is the other
 * half of contract §3.0: the code that owns the player, carries the reducer's
 * state forward, publishes the surface, keeps the ledger ring and emits the
 * client log — and that discharges the owner's one new obligation.
 *
 * The obligation is that a blocking fault is raised only over a player the
 * owner has ALREADY stopped. Every site in `Controller` that raises anything is
 * a NAMED method here, and each blocking one does its own stop before it
 * raises — deliberate duplication, because it is what makes deleting one stop
 * fail exactly one test instead of all of them. The two private entry points
 * are the only general ones, and `raiseBlocking` samples `playbackRequested`
 * AFTER the caller's stop: a site that lost its stop does not produce an
 * overlay over a running picture, it produces a `blocking_without_stop` line
 * and no surface at all, which is what the fixture demands.
 *
 * docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3.0, §3.2, §3.4, §5.
 */

/** The slice of the player the owner touches. Nothing here is presentation proof. */
internal interface SurfaceOwnerPlayer {
    /** `ExoPlayer.playWhenReady`. Setting it false IS the owner's stop. */
    var playbackRequested: Boolean

    /** The film position, for the ledger's `player_at_raise`. */
    val positionMs: Long

    /** The transport rate, for the ledger's `player_at_raise`. */
    val rate: Float
}

/** What the player looked like when a fault was raised. Diagnostics, not evidence. */
internal data class SurfacePlayerSample(
    val rate: Float,
    val positionMs: Long,
    val presenting: Boolean,
    val stoppedByOwner: Boolean,
)

/** One row of the bounded ring the Playback debug ledger renders. */
internal data class SurfaceLedgerRow(
    /** The fault's raise order; two live faults of one source are told apart by it. */
    val seq: Long,
    val cls: SurfaceClass,
    val source: String,
    val attached: Long,
    val intent: Long?,
    val raisedAtMs: Long,
    val playerAtRaise: SurfacePlayerSample,
    val clearedAtMs: Long? = null,
    val clearedBy: String? = null,
    /** The agreement rule demoted this fault: the picture was moving under it. */
    val disagreed: Boolean = false,
)

/**
 * Carries the reducer's state, publishes its surface, keeps the last
 * [HISTORY_LIMIT] faults and emits the contract's four client-log events.
 *
 * `publish` is the single surface write; `emit` is the client log. Neither
 * touches the player: the only lines in this file that do are the one stop each
 * blocking site owns.
 */
internal class PlaybackSurfaceOwner(
    private val player: SurfaceOwnerPlayer,
    private val nowMs: () -> Long,
    private val publish: (PlaybackSurface) -> Unit,
    private val emit: (SurfaceLog) -> Unit = {},
    private val reducer: PlaybackSurfaceReducer = PlaybackSurfaceReducer(),
) {
    private var state = SurfaceState()
    private val ring = ArrayDeque<SurfaceLedgerRow>()

    var current: PlaybackSurface = PlaybackSurface.None
        private set

    /** Newest last, at most [HISTORY_LIMIT] rows. */
    val history: List<SurfaceLedgerRow> get() = ring.toList()

    /** The generation every fault raised from here is about. */
    val attached: Long? get() = state.attached

    /** The last presentation sample the owner fed in. */
    val isPresenting: Boolean get() = state.presenting

    // ------------------------------------------------------------- identity

    fun attach(generation: Long) = dispatch(SurfaceEvent.Attach(generation))

    fun retire(generation: Long) = dispatch(SurfaceEvent.Retire(generation))

    fun intentSettled(intent: Long) = dispatch(SurfaceEvent.IntentSettled(intent))

    fun intentSuperseded(intent: Long) = dispatch(SurfaceEvent.IntentSuperseded(intent))

    fun ownerSuccess(generation: Long) = dispatch(SurfaceEvent.OwnerSuccess(generation))

    // ------------------------------------------------------------- evidence

    /**
     * A presentation sample. The caller supplies the client's existing proof —
     * a `realPosition()` advance or a rendered video frame on [generation],
     * foreground, `playWhenReady` true — and nothing else.
     */
    fun presenting(presenting: Boolean, generation: Long?) =
        dispatch(SurfaceEvent.Presenting(presenting, generation))

    /** `presentationForeground` inverted: Android's stand-in for a hidden page. */
    fun hidden(hidden: Boolean) = dispatch(SurfaceEvent.Hidden(hidden))

    /** A platform callback that is not proof of anything. It moves nothing. */
    fun inert(name: String) = dispatch(SurfaceEvent.Inert(name))

    fun tick() = dispatch(SurfaceEvent.Tick)

    fun userAction(action: SurfaceAction) = dispatch(SurfaceEvent.UserAction(action))

    // --------------------------------------------------------------- raising
    //
    // One named method per site in `Controller` that raises anything. There is
    // no general entry point reachable from outside this file: [raiseNotice]
    // and [raiseBlocking] are private, and the fence fails on any generic
    // raising spelling against this owner from a scanned file. A new site
    // therefore has to add itself here, where a test can name it.
    //
    // Each of the blocking sites does its OWN stop. That is deliberate
    // duplication: it is what makes deleting one stop fail exactly one test
    // instead of all of them.

    /** `onStall`: this playback's one sessionless recovery was already spent. */
    fun exhaustedAfterSessionlessStall(attached: Long, positionMs: Long) {
        player.playbackRequested = false
        raiseBlocking(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = positionMs,
            detail = SESSIONLESS_STALL_SPENT,
        )
    }

    /** `onStall`: the reopen budget is spent at the ladder floor. */
    fun exhaustedAfterReopenBudget(attached: Long, positionMs: Long) {
        player.playbackRequested = false
        raiseBlocking(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = positionMs,
            detail = REOPEN_BUDGET_SPENT,
        )
    }

    /** The target-presentation deadline fired with no rung left to try. */
    fun exhaustedAfterTargetDeadline(attached: Long, targetMs: Long) {
        player.playbackRequested = false
        raiseBlocking(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = targetMs,
            detail = TARGET_NEVER_PRESENTED,
        )
    }

    /** `onPlayerError` reached the end of the ladder. */
    fun stoppedAfterPlaybackError(
        attached: Long,
        source: String,
        positionMs: Long?,
        detail: String,
    ) {
        player.playbackRequested = false
        raiseBlocking(
            source = source,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = positionMs,
            detail = detail,
        )
    }

    /** A session create this playback was waiting on failed, with nothing behind it. */
    fun stoppedAfterSessionCreate(
        attached: Long,
        source: String,
        positionMs: Long?,
        detail: String,
    ) {
        player.playbackRequested = false
        raiseBlocking(
            source = source,
            context = SurfaceContext.Start,
            attached = attached,
            positionMs = positionMs,
            detail = detail,
        )
    }

    /**
     * The control plane ruled this stall terminal.
     *
     * Ruling D1 keeps the verdict from tearing anything down — no reopen starts
     * and no budget is spent — but the owner has nothing left to try either, so
     * it stops and says so in the server's words. See the amendment to §3.4's
     * Android row in PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md.
     */
    fun stoppedAfterControlVerdict(attached: Long, positionMs: Long, detail: String) {
        player.playbackRequested = false
        raiseBlocking(
            source = SurfaceSources.OWNER_STOPPED,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = positionMs,
            detail = detail,
        )
    }

    // ---- notices: nothing here stops the player, and nothing here may block

    /** A viewer-requested replacement failed over a predecessor still playing. */
    fun refusedChange(attached: Long, intent: Long?, positionMs: Long?, detail: String) =
        raiseNotice(
            source = SurfaceSources.CHANGE_FAILED,
            context = SurfaceContext.Change,
            attached = attached,
            intent = intent,
            positionMs = positionMs,
            detail = detail,
        )

    /** The control plane asked for a brief wait. A stall is about attached media. */
    fun controlHold(attached: Long, detail: String) =
        raiseNotice(SurfaceSources.CONTROL_HOLD, SurfaceContext.Attached, attached, detail = detail)

    /** An automatic downshift, an HDR-subtitle refusal, a PGS or PiP failure. */
    fun degradedNotice(attached: Long, context: SurfaceContext, detail: String) =
        raiseNotice(SurfaceSources.DEGRADED_NOTICE, context, attached, detail = detail)

    /** A playlist or segment 503 the owner is recovering from. */
    fun recoveringSegmentRefusal(attached: Long, positionMs: Long?, detail: String) =
        raiseNotice(
            SurfaceSources.SEGMENT_503_NOT_YET,
            SurfaceContext.Attached,
            attached,
            positionMs = positionMs,
            detail = detail,
        )

    /** A 410: the media owner is gone, and it named where to resume. */
    fun recoveringMediaOwnerLost(
        attached: Long,
        context: SurfaceContext,
        positionMs: Long?,
        detail: String,
        actions: List<SurfaceAction>? = null,
    ) = raiseNotice(
        SurfaceSources.MEDIA_OWNER_LOST_410,
        context,
        attached,
        positionMs = positionMs,
        detail = detail,
        actions = actions,
    )

    /** A node failover, a compatibility rung, a stall reopen. */
    fun recoveringOwnerStep(attached: Long, context: SurfaceContext, detail: String) =
        raiseNotice(SurfaceSources.OWNER_RECOVERY_STEP, context, attached, detail = detail)

    /** A readiness deadline that fired with a rung still left (§3.3 row 14). */
    fun recoveringReadinessDeadline(attached: Long, context: SurfaceContext, detail: String) =
        raiseNotice(
            SurfaceSources.READINESS_DEADLINE_RUNGS_LEFT,
            context,
            attached,
            detail = detail,
        )

    // ------------------------------------------------------------- internals

    /**
     * A fault the owner has NOT stopped the player for. A blocking class raised
     * through here is refused by the reducer and logged; the named sites above
     * are the only things that may show one.
     */
    private fun raiseNotice(
        source: String,
        context: SurfaceContext,
        attached: Long,
        intent: Long? = null,
        positionMs: Long? = null,
        title: String? = null,
        detail: String? = null,
        actions: List<SurfaceAction>? = null,
    ) = dispatch(
        SurfaceEvent.Raise(
            source = source,
            context = context,
            attached = attached,
            intent = intent,
            playerStopped = false,
            actions = actions,
            positionMs = positionMs,
            title = title,
            detail = detail,
        ),
    )

    /**
     * `player_stopped` is sampled from the player AFTER its caller's stop, so
     * the fault asserts only what is true of the player at that instant. A
     * caller that lost its stop produces a `blocking_without_stop` log line and
     * no surface at all — never an overlay over a moving picture.
     */
    private fun raiseBlocking(
        source: String,
        context: SurfaceContext,
        attached: Long,
        positionMs: Long? = null,
        detail: String? = null,
    ) = dispatch(
        SurfaceEvent.Raise(
            source = source,
            context = context,
            attached = attached,
            playerStopped = !player.playbackRequested,
            positionMs = positionMs,
            detail = detail,
        ),
    )

    // ----------------------------------------------------------------- plumb

    private fun dispatch(event: SurfaceEvent) {
        val step = reducer.apply(state, event, nowMs())
        state = step.state
        for (entry in step.log) {
            record(entry)
            emit(entry)
        }
        if (current != step.surface) {
            current = step.surface
            publish(step.surface)
        }
    }

    private fun record(entry: SurfaceLog) {
        val source = entry.source ?: return
        val cls = entry.cls ?: return
        val attachedAt = entry.attached ?: return
        val seq = entry.seq ?: return
        when (entry.event) {
            SurfaceLogEvents.RAISED -> {
                // A promotion re-raises an existing fault (`media_owner_lost_410`
                // becoming `stopped`); the row it already has is the one that
                // describes it, so the class moves rather than a second row
                // appearing for the same fault.
                val promoted = if (entry.by != null) openRow(seq) else null
                if (promoted != null) {
                    ring[promoted] = ring[promoted].copy(cls = cls)
                    return
                }
                if (ring.size >= HISTORY_LIMIT) ring.removeFirst()
                ring.addLast(
                    SurfaceLedgerRow(
                        seq = seq,
                        cls = cls,
                        source = source,
                        attached = attachedAt,
                        intent = entry.intent,
                        raisedAtMs = state.nowMs,
                        playerAtRaise = SurfacePlayerSample(
                            rate = player.rate,
                            positionMs = player.positionMs,
                            presenting = state.presenting,
                            stoppedByOwner = entry.playerStopped,
                        ),
                    ),
                )
            }
            SurfaceLogEvents.CLEARED -> {
                val index = openRow(seq) ?: return
                ring[index] = ring[index].copy(clearedAtMs = state.nowMs, clearedBy = entry.by)
            }
            SurfaceLogEvents.DISAGREEMENT -> {
                val index = openRow(seq) ?: return
                ring[index] = ring[index].copy(disagreed = true)
            }
            else -> Unit
        }
    }

    /**
     * This exact fault's row, by index, or null.
     *
     * Keyed on the fault's own raise order rather than on (source, attached):
     * two live `degraded_notice` faults on one generation would otherwise pair
     * their clears with whichever row happened to be newest.
     */
    private fun openRow(seq: Long): Int? {
        for (index in ring.indices.reversed()) {
            if (ring[index].seq == seq && ring[index].clearedAtMs == null) return index
        }
        return null
    }

    companion object {
        const val HISTORY_LIMIT = 16

        // The sentences each exhausted site says, here rather than at the call
        // site so a test can name the site by its words.
        const val SESSIONLESS_STALL_SPENT =
            "Playback stopped responding after retrying this stream."
        const val REOPEN_BUDGET_SPENT =
            "Playback stopped responding after exhausting recovery attempts."
        const val TARGET_NEVER_PRESENTED =
            "Playback couldn't reach the requested position after retrying. Your place is saved."
    }
}
