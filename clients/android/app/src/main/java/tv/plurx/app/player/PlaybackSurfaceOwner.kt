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
 * owner has ALREADY stopped. It is discharged here rather than at each call
 * site, and it cannot be forgotten at one: [raise] refuses a blocking class
 * outright and [stopAndRaise] is the only way to reach one. [stopAndRaise]
 * stops the player first and then samples `playbackRequested` to fill
 * `player_stopped`, so deleting the stop does not produce an overlay over a
 * running picture — it produces a `blocking_without_stop` line in the log and
 * no surface at all, which is what the fixture demands.
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
 * touches the player: the only line in this file that does is the stop in
 * [stopAndRaise].
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

    /**
     * Raise a non-blocking fault. A blocking class raised through here is
     * refused by the reducer and logged; use [stopAndRaise], which is the only
     * thing that may show one.
     */
    fun raise(
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
     * The owner's obligation, in one place: **stop the player, then raise.**
     *
     * `player_stopped` is sampled from the player AFTER the stop, so the fault
     * asserts only what is true of the player at that instant.
     */
    fun stopAndRaise(
        source: String,
        context: SurfaceContext,
        attached: Long,
        intent: Long? = null,
        positionMs: Long? = null,
        title: String? = null,
        detail: String? = null,
        actions: List<SurfaceAction>? = null,
    ) {
        player.playbackRequested = false
        dispatch(
            SurfaceEvent.Raise(
                source = source,
                context = context,
                attached = attached,
                intent = intent,
                playerStopped = !player.playbackRequested,
                actions = actions,
                positionMs = positionMs,
                title = title,
                detail = detail,
            ),
        )
    }

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
        when (entry.event) {
            SurfaceLogEvents.RAISED -> {
                // A promotion re-raises an existing fault (`media_owner_lost_410`
                // becoming `stopped`); the row it already has is the one that
                // describes it, so the class moves rather than a second row
                // appearing for the same fault.
                val promoted = if (entry.by != null) openRow(source, attachedAt) else null
                if (promoted != null) {
                    ring[promoted] = ring[promoted].copy(cls = cls)
                    return
                }
                if (ring.size >= HISTORY_LIMIT) ring.removeFirst()
                ring.addLast(
                    SurfaceLedgerRow(
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
                val index = openRow(source, attachedAt) ?: return
                ring[index] = ring[index].copy(clearedAtMs = state.nowMs, clearedBy = entry.by)
            }
            SurfaceLogEvents.DISAGREEMENT -> {
                val index = openRow(source, attachedAt) ?: return
                ring[index] = ring[index].copy(disagreed = true)
            }
            else -> Unit
        }
    }

    /** The newest still-open row for this fault, by index, or null. */
    private fun openRow(source: String, attached: Long): Int? {
        for (index in ring.indices.reversed()) {
            val row = ring[index]
            if (row.source == source && row.attached == attached && row.clearedAtMs == null) return index
        }
        return null
    }

    companion object {
        const val HISTORY_LIMIT = 16
    }
}
