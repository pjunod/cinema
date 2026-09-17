package tv.plurx.app.player

/**
 * The playback surface presenter — a pure projection of the player, not a
 * message channel.
 *
 * One reducer, fed the same ordered event sequences as the web and Apple
 * presenters (`tests/playback/playback-surface-contract.json`, run by
 * [PlaybackSurfaceReducerTest]). It has NO side effects: it never pauses,
 * resumes, reopens, prepares, seeks, cancels a timer or reports anything. The
 * recovery owner — `Controller`'s ladders, budgets and deadlines — keeps every
 * one of those powers and gains exactly one obligation: stop the player before
 * raising a blocking fault, which this reducer refuses to render without.
 *
 * docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3;
 * docs/clients/PLAYBACK-SURFACE-CONTRACT-IMPLEMENTATION.md §3.3.
 *
 * The tables below are the fixture's `classes`, `sources`, `timings` and
 * `severity_rank`, transcribed. [PlaybackSurfaceReducerTest] asserts they are
 * the fixture verbatim, so a drift here is a test failure rather than a third
 * client quietly disagreeing with the other two.
 */

/** Rank orders which drawable fault owns the surface; highest wins. */
internal enum class SurfaceSeverity(val wire: String, val rank: Int) {
    Notice("notice", 1),
    Progress("progress", 2),
    Prompt("prompt", 3),
    Terminal("terminal", 4),
}

/** How a class covers the picture. `WhileNotPresenting` is the progress rule. */
internal enum class SurfaceBlocking {
    Never,
    WhileNotPresenting,
    Always,
}

/** The reasons a class admits for being retired. A property of the CLASS. */
internal enum class SurfaceRetirement(val wire: String) {
    Presenting("presenting"),
    PresentingAfterRaise("presenting_after_raise"),
    PresentingContinuousMs("presenting_continuous_ms"),
    IntentSettled("intent_settled"),
    IntentSuperseded("intent_superseded"),
    AttachedRetired("attached_retired"),
    OwnerSuccess("owner_success"),

    /**
     * The viewer no longer wants media.
     *
     * On `buffering` and on NO other class: a `preparing` start has not been
     * paused by a viewer who has not seen it yet, and a blocking prompt is
     * answered by the viewer rather than by a transport change.
     */
    PlaybackNotRequested("playback_not_requested"),
    Timer("timer"),
    User("user"),
}

/** The fixture's whole action vocabulary. The owner sets them when it raises. */
internal enum class SurfaceAction(val wire: String) {
    KeepWaiting("keep_waiting"),
    Retry("retry"),
    Close("close"),
    ForceTranscode("force_transcode"),
    SignIn("sign_in"),
}

/** Which context a fault was raised in; a source row matches on it. */
internal enum class SurfaceContext(val wire: String) {
    Start("start"),
    Attached("attached"),
    Change("change"),
}

/** The contract's timings, in milliseconds. */
internal object SurfaceTimings {
    const val BUFFERING_MIN_MS = 350L
    const val HOLD_NOTICE_MS = 30_000L
    const val DEGRADED_NOTICE_MS = 5_000L

    /** CONTINUOUS presenting on the attached generation, not accumulated playback. */
    const val REFUSED_PROGRESS_MS = 10_000L

    /** Retires the "Playback recovered" banner a demotion leaves behind. */
    const val DISAGREEMENT_NOTICE_MS = 30_000L
}

internal enum class SurfaceClass(
    val wire: String,
    val severity: SurfaceSeverity,
    val blocking: SurfaceBlocking,
    val retiredBy: Set<SurfaceRetirement>,
    /** Debounces the SURFACE, not the fault: it exists from the moment it is raised. */
    val minMs: Long? = null,
    val timedMs: Long? = null,
    val timerPausedWhileActions: Boolean = false,
    val requiresPlayerStopped: Boolean = false,
    val title: String? = null,
    val defaultActions: List<SurfaceAction> = emptyList(),
    /** The bound a `presenting_continuous_ms` retirement is measured against. */
    val continuousMs: Long? = null,
) {
    Preparing(
        wire = "preparing",
        severity = SurfaceSeverity.Progress,
        blocking = SurfaceBlocking.WhileNotPresenting,
        retiredBy = setOf(
            SurfaceRetirement.Presenting,
            SurfaceRetirement.IntentSettled,
            SurfaceRetirement.AttachedRetired,
        ),
    ),
    Buffering(
        wire = "buffering",
        severity = SurfaceSeverity.Progress,
        blocking = SurfaceBlocking.WhileNotPresenting,
        // A `buffering` fault is about a player that WANTS media. Presentation
        // evidence is the only other thing that retires it, and a paused picture
        // produces no more samples — so without `playback_not_requested` a
        // buffer that fills while the viewer has paused leaves a spinner over a
        // still frame until the generation changes, which is the overlay
        // outliving the thing it described. Ruled upstream in 1e19b133.
        retiredBy = setOf(
            SurfaceRetirement.Presenting,
            SurfaceRetirement.PlaybackNotRequested,
            SurfaceRetirement.AttachedRetired,
        ),
        minMs = SurfaceTimings.BUFFERING_MIN_MS,
    ),
    Recovering(
        wire = "recovering",
        severity = SurfaceSeverity.Progress,
        blocking = SurfaceBlocking.WhileNotPresenting,
        retiredBy = setOf(
            SurfaceRetirement.PresentingAfterRaise,
            SurfaceRetirement.OwnerSuccess,
            SurfaceRetirement.AttachedRetired,
        ),
    ),
    Hold(
        wire = "hold",
        severity = SurfaceSeverity.Notice,
        blocking = SurfaceBlocking.Never,
        // A server hold over a picture that never stopped expires on its timer,
        // not on the picture that was already there.
        retiredBy = setOf(SurfaceRetirement.Timer, SurfaceRetirement.PresentingAfterRaise),
        timedMs = SurfaceTimings.HOLD_NOTICE_MS,
    ),
    Degraded(
        wire = "degraded",
        severity = SurfaceSeverity.Notice,
        blocking = SurfaceBlocking.Never,
        retiredBy = setOf(SurfaceRetirement.Timer, SurfaceRetirement.PresentingContinuousMs),
        timedMs = SurfaceTimings.DEGRADED_NOTICE_MS,
        timerPausedWhileActions = true,
        continuousMs = SurfaceTimings.DISAGREEMENT_NOTICE_MS,
    ),
    Refused(
        wire = "refused",
        severity = SurfaceSeverity.Notice,
        blocking = SurfaceBlocking.Never,
        retiredBy = setOf(
            SurfaceRetirement.IntentSuperseded,
            SurfaceRetirement.PresentingContinuousMs,
        ),
        defaultActions = listOf(SurfaceAction.Retry),
        continuousMs = SurfaceTimings.REFUSED_PROGRESS_MS,
    ),
    Exhausted(
        wire = "exhausted",
        severity = SurfaceSeverity.Prompt,
        blocking = SurfaceBlocking.Always,
        retiredBy = setOf(SurfaceRetirement.User),
        requiresPlayerStopped = true,
        title = "Playback is stalled.",
        defaultActions = listOf(SurfaceAction.KeepWaiting, SurfaceAction.Retry, SurfaceAction.Close),
    ),
    Stopped(
        wire = "stopped",
        severity = SurfaceSeverity.Terminal,
        blocking = SurfaceBlocking.Always,
        retiredBy = setOf(SurfaceRetirement.User),
        requiresPlayerStopped = true,
        defaultActions = listOf(SurfaceAction.Retry, SurfaceAction.Close),
    ),
    ;

    companion object {
        fun ofWire(wire: String): SurfaceClass? = entries.firstOrNull { it.wire == wire }
    }
}

/**
 * One row of the fixture's `sources` table.
 *
 * [contextWire] is `"any"` or one of [SurfaceContext]'s wires. Rows are scanned
 * in order and the first whose id AND context both match wins.
 */
internal data class SurfaceSourceRow(
    val id: String,
    val contextWire: String,
    val cls: SurfaceClass?,
    val requiresPlayerStopped: Boolean = false,
    val actions: List<SurfaceAction>? = null,
    val codes: List<String> = emptyList(),
    val thenWhenStopped: SurfaceClass? = null,
    val carriesPositionMs: Boolean = false,
    val retryable: Boolean = false,
) {
    fun matches(context: SurfaceContext): Boolean =
        contextWire == "any" || contextWire == context.wire
}

/** Source ids the adapter and the owner name when they raise. */
internal object SurfaceSources {
    const val OWNER_STOPPED = "owner_stopped"
    const val OWNER_EXHAUSTED = "owner_exhausted"
    const val STARTUP_EXHAUSTED = "startup_exhausted"
    const val HLS_INIT_INVALID = "hls_init_invalid"
    const val HLS_INIT_UNSUPPORTED = "hls_init_unsupported"
    const val AUTH_401_403 = "auth_401_403"
    const val VOD_SOURCE_RESCAN_REQUIRED = "vod_source_rescan_required"
    const val VOD_SOURCE_UNSUPPORTED = "vod_source_unsupported"
    const val VOD_TRANSCODE_UNAVAILABLE = "vod_transcode_unavailable"
    const val VOD_SUBTITLE_BURN_UNAVAILABLE = "vod_subtitle_burn_unavailable"
    const val VOD_DISABLED = "vod_disabled"
    const val CREATE_503_NOT_YET = "create_503_not_yet"
    const val CLIENT_PREPARING = "client_preparing"
    const val CHANGE_FAILED = "change_failed"
    const val SEGMENT_503_NOT_YET = "segment_503_not_yet"
    const val MEDIA_OWNER_LOST_410 = "media_owner_lost_410"
    const val CONTROL_HOLD = "control_hold"
    const val MEDIA_WAITING = "media_waiting"
    const val OWNER_RECOVERY_STEP = "owner_recovery_step"
    const val READINESS_DEADLINE_RUNGS_LEFT = "readiness_deadline_rungs_left"
    const val DECODER_FAILED = "decoder_failed"
    const val BLACK_FRAME_LADDER_SPENT = "black_frame_ladder_spent"
    const val REPEATED_EARLY_END = "repeated_early_end"
    const val DEGRADED_NOTICE = "degraded_notice"
    const val LOG_ONLY = "log_only"
}

/** The fixture's `sources`, in fixture order. Order is the precedence rule. */
internal val SURFACE_SOURCES: List<SurfaceSourceRow> = listOf(
    SurfaceSourceRow(
        SurfaceSources.OWNER_STOPPED,
        "any",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.OWNER_EXHAUSTED,
        "any",
        SurfaceClass.Exhausted,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.STARTUP_EXHAUSTED,
        "any",
        SurfaceClass.Exhausted,
        requiresPlayerStopped = true,
        actions = listOf(SurfaceAction.Close, SurfaceAction.Retry),
    ),
    SurfaceSourceRow(
        SurfaceSources.HLS_INIT_INVALID,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.HLS_INIT_UNSUPPORTED,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.AUTH_401_403,
        "any",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
        actions = listOf(SurfaceAction.SignIn, SurfaceAction.Close),
    ),
    SurfaceSourceRow(
        SurfaceSources.VOD_SOURCE_RESCAN_REQUIRED,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.VOD_SOURCE_UNSUPPORTED,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.VOD_TRANSCODE_UNAVAILABLE,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.VOD_SUBTITLE_BURN_UNAVAILABLE,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.VOD_DISABLED,
        "start",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.CREATE_503_NOT_YET,
        "start",
        SurfaceClass.Preparing,
        codes = listOf(
            "startup_timeout",
            "media_owner_transition",
            "vod_index_pending",
            "vod_engine_unattested",
        ),
        retryable = true,
    ),
    SurfaceSourceRow(SurfaceSources.CLIENT_PREPARING, "any", SurfaceClass.Preparing),
    SurfaceSourceRow(
        SurfaceSources.CHANGE_FAILED,
        "change",
        SurfaceClass.Refused,
        actions = listOf(SurfaceAction.Retry),
    ),
    SurfaceSourceRow(
        SurfaceSources.SEGMENT_503_NOT_YET,
        "attached",
        SurfaceClass.Recovering,
        // The 503 codes a playlist or segment request can actually come back
        // with. Transcribed from the fixture, which is where the argument for
        // each of them lives; on those two resources a 503 is only ever a "not
        // yet", and the terminal answers are 404/410/502.
        codes = listOf(
            "startup_timeout",
            "playlist_state_changed",
            "segment_pending",
            "segment_wait_busy",
            "node_wait_capacity",
            "media_owner_transition",
            "vod_resurrection_unavailable",
            "response_owner_transition",
            "response_state_changed",
            "response_owner_reclassification_unavailable",
            "response_publication_timeout",
            "response_completion_capacity",
            "response_snapshot_capacity",
            "node_maintenance",
            "node_removal_fenced",
            "learner_route_ineligible",
        ),
    ),
    SurfaceSourceRow(
        SurfaceSources.MEDIA_OWNER_LOST_410,
        // `any`: a 410 is the server's answer on a create as well as on a
        // segment, and it carries the position either way.
        "any",
        SurfaceClass.Recovering,
        thenWhenStopped = SurfaceClass.Stopped,
        carriesPositionMs = true,
    ),
    SurfaceSourceRow(SurfaceSources.CONTROL_HOLD, "attached", SurfaceClass.Hold),
    SurfaceSourceRow(SurfaceSources.MEDIA_WAITING, "attached", SurfaceClass.Buffering),
    SurfaceSourceRow(SurfaceSources.OWNER_RECOVERY_STEP, "any", SurfaceClass.Recovering),
    SurfaceSourceRow(SurfaceSources.READINESS_DEADLINE_RUNGS_LEFT, "any", SurfaceClass.Recovering),
    SurfaceSourceRow(
        SurfaceSources.DECODER_FAILED,
        "any",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(
        SurfaceSources.BLACK_FRAME_LADDER_SPENT,
        "start",
        SurfaceClass.Exhausted,
        requiresPlayerStopped = true,
        actions = listOf(SurfaceAction.Close, SurfaceAction.Retry),
    ),
    SurfaceSourceRow(
        SurfaceSources.REPEATED_EARLY_END,
        "attached",
        SurfaceClass.Stopped,
        requiresPlayerStopped = true,
    ),
    SurfaceSourceRow(SurfaceSources.DEGRADED_NOTICE, "any", SurfaceClass.Degraded),
    SurfaceSourceRow(SurfaceSources.LOG_ONLY, "any", null),
)

/**
 * Which source row a refusal CODE names, in [context], or null.
 *
 * The fixture's `codes` lists are what say "this status, with this code, is
 * that row": §3.3 row 6 for a create, row 8 for a playlist or segment. A code
 * in no row's list names no row — a 503 nobody explained is not a "still
 * building" answer and may not borrow one's class, which is the rule
 * `Controller.createIsStillBuilding` already applies to creates. A row is
 * consulted only in a context it declares, so a create-only code cannot claim
 * a segment refusal and the reverse.
 *
 * Read straight off [SURFACE_SOURCES], so a code the fixture adds or moves is
 * honoured by the adapter the moment the transcribed table moves with it.
 */
internal fun surfaceSourceForCode(code: String?, context: SurfaceContext): String? {
    val wanted = code?.trim()?.takeIf { it.isNotEmpty() } ?: return null
    return SURFACE_SOURCES.firstOrNull { row ->
        row.codes.contains(wanted) && row.matches(context)
    }?.id
}

/**
 * Does [sourceId]'s row admit a refusal carrying [code], in [context]?
 *
 * A row that lists `codes` is claiming "this status WITH one of these codes",
 * and a refusal carrying something else is not that row. A row that lists none
 * is claiming the status outright, and a client that demanded a code from it
 * would make the row unreachable — which is the defect this whole change
 * exists to remove, not one to add.
 *
 * Which of the two a row is doing belongs to the fixture, so it is READ here
 * rather than decided: the same adapter is correct before and after a `codes`
 * list is added to a row, and adding one narrows the row on every client at
 * once.
 */
internal fun surfaceRowAdmitsCode(
    sourceId: String,
    code: String?,
    context: SurfaceContext,
): Boolean {
    val row = SURFACE_SOURCES.firstOrNull { it.id == sourceId && it.matches(context) } ?: return false
    if (row.codes.isEmpty()) return true
    return row.codes.contains(code?.trim()?.takeIf { it.isNotEmpty() })
}

/**
 * Only the owner's OWN stop promotes a fault that declared a successor class
 * (`media_owner_lost_410` → `stopped`). Any other blocking source is its own
 * fault with its own sentence and its own actions — a decoder failure reported
 * as a 410 is exactly the misattribution the ledger exists to prevent, and an
 * `auth_401_403` folded into a 410 loses its Sign in.
 */
private val SURFACE_PROMOTING_SOURCES =
    setOf(SurfaceSources.OWNER_STOPPED, SurfaceSources.OWNER_EXHAUSTED)

/** The fixture's error ids. The reducer logs these and leaves the surface alone. */
internal object SurfaceErrors {
    const val BLOCKING_WITHOUT_STOP = "blocking_without_stop"
    const val SOURCE_CONTEXT_MISMATCH = "source_context_mismatch"
    const val UNKNOWN_SOURCE = "unknown_source"
}

/** The four client-log events of contract §3.6, plus the reducer's own error line. */
internal object SurfaceLogEvents {
    const val RAISED = "surface_raised"
    const val CLEARED = "surface_cleared"
    const val DISAGREEMENT = "surface_disagreement"
    const val LOG_ONLY = "surface_log_only"
    const val ERROR = "surface_error"
}

/**
 * A typed fault.
 *
 * [attached] is the media generation the fault is ABOUT (`mediaMutationEpoch`);
 * [intent] is the viewer request it belongs to, when it belongs to one
 * (`PlaybackIntent` sequence). A fault about a pending destination has an
 * [intent] and is retired only by that intent settling or being superseded; a
 * fault about the attached media has none and is retired by that media's
 * presentation evidence.
 *
 * [thenWhenStopped], [demoted] and [seq] are the reducer's bookkeeping over the
 * shape the implementation plan gives: the successor class the
 * `media_owner_lost_410` row declares, whether the agreement rule has already
 * demoted this fault, and a monotonic raise order so two faults of equal
 * severity break the tie by recency.
 */
internal data class PlaybackFault(
    val cls: SurfaceClass,
    val source: String,
    val attached: Long,
    val intent: Long?,
    val raisedAtMs: Long,
    val positionMs: Long? = null,
    val title: String? = null,
    val detail: String? = null,
    val actions: List<SurfaceAction> = emptyList(),
    val playerStopped: Boolean = false,
    val thenWhenStopped: SurfaceClass? = null,
    val demoted: Boolean = false,
    val seq: Long = 0,
)

/** What is drawn over the picture. Rendered FROM this and nothing else. */
internal sealed interface PlaybackSurface {
    val fault: PlaybackFault?

    /** Nothing is drawn over the picture. */
    data object None : PlaybackSurface {
        override val fault: PlaybackFault? get() = null
    }

    /** In-chrome progress indicator; the picture is presenting behind it. */
    data class Indicator(override val fault: PlaybackFault) : PlaybackSurface

    /** A notice strip with the fault's actions; the picture is untouched. */
    data class Banner(override val fault: PlaybackFault) : PlaybackSurface

    /**
     * The picture is covered. Only ever drawn over a player its recovery owner
     * has already stopped, or over a picture that is not presenting.
     */
    data class Blocking(override val fault: PlaybackFault) : PlaybackSurface
}

/**
 * The player input contract's `failed` state.
 *
 * A BLOCKING surface whose class is a prompt or a terminal — a fault with
 * actions the viewer must answer. A full-screen `preparing`/`buffering`/
 * `recovering` is blocking pixels, not routing: it keeps today's routing
 * (PLAYBACK-SURFACE-CONTRACT.md §4).
 */
internal val PlaybackSurface.entersFailedRouting: Boolean
    get() = this is PlaybackSurface.Blocking &&
        (
            this.fault.cls.severity == SurfaceSeverity.Prompt ||
                this.fault.cls.severity == SurfaceSeverity.Terminal
            )

/**
 * Whether two consecutive film-clock samples are presentation evidence
 * (contract §3.4).
 *
 * The clock MOVED, on the generation that is attached, in the foreground, with
 * playback requested. A state flag is not in it: `isPlaying`, `canplay` and
 * their twins say what the player thinks, not what the picture did, and a
 * surface that believed them would clear itself over a frozen frame.
 */
internal fun presentationEvidence(
    previousPositionMs: Long?,
    positionMs: Long,
    playbackRequested: Boolean,
    foreground: Boolean,
): Boolean = playbackRequested &&
    foreground &&
    previousPositionMs != null &&
    previousPositionMs != positionMs

/**
 * The terminal for a playback that never got a player.
 *
 * The decision or the detail request failed, so there is no `Controller`, no
 * presenter instance, no media generation for a fault to be about and no player
 * to stop — `player_stopped` is true because there is nothing running. The
 * class is still the contract's `stopped`, so the pre-play screen and the
 * player's own terminal read and route alike. It lives here so the fence can
 * forbid building a fault anywhere else.
 */
internal fun preplayerStoppedFault(detail: String): PlaybackFault = PlaybackFault(
    cls = SurfaceClass.Stopped,
    source = SurfaceSources.OWNER_STOPPED,
    attached = 0,
    intent = null,
    raisedAtMs = 0,
    detail = detail,
    actions = SurfaceClass.Stopped.defaultActions,
    playerStopped = true,
)

/** One ordered input to the reducer. Mirrors the fixture's event vocabulary. */
internal sealed interface SurfaceEvent {
    /** Generation [generation] becomes attached; an earlier one is retired. */
    data class Attach(val generation: Long) : SurfaceEvent

    /** [generation] retired: faults about it stop being about anything. */
    data class Retire(val generation: Long) : SurfaceEvent

    /**
     * A sample from the client's own presentation proof — on Android a
     * `realPosition()` advance or a rendered video frame on the current
     * `mediaMutationEpoch`, foreground, `playWhenReady` true. The ONLY event
     * that may retire a `presenting`-retired class or trigger a disagreement.
     */
    data class Presenting(val presenting: Boolean, val attached: Long?) : SurfaceEvent

    /**
     * An inert platform event. It prompts a look; it is not evidence, and it
     * never moves a surface — not even by letting a due timer run.
     */
    data class Inert(val name: String) : SurfaceEvent

    /** A fault from the adapter or the recovery owner. */
    data class Raise(
        val source: String,
        val context: SurfaceContext,
        val attached: Long,
        val intent: Long? = null,
        val playerStopped: Boolean = false,
        val actions: List<SurfaceAction>? = null,
        val positionMs: Long? = null,
        val title: String? = null,
        val detail: String? = null,
    ) : SurfaceEvent

    data class IntentSettled(val intent: Long) : SurfaceEvent

    data class IntentSuperseded(val intent: Long) : SurfaceEvent

    /** The owner reports that its recovery produced attached [generation]. */
    data class OwnerSuccess(val generation: Long) : SurfaceEvent

    /**
     * The viewer pressed an action. The reducer clears the fault the action
     * belonged to; the EFFECT of the action is the owner's, outside the reducer.
     */
    data class UserAction(val action: SurfaceAction) : SurfaceEvent

    /**
     * The viewer's transport intent changed.
     *
     * `false` retires every fault whose class names `playback_not_requested` —
     * `buffering`, and only `buffering`. `true` does nothing: the raise sites
     * decide what comes back, which is the rule every other retirement follows.
     *
     * The VIEWER's intent, never the owner's stop-before-raise. Media3 reports
     * both as a `USER_REQUEST`, and the filter that tells them apart is the
     * caller's (`Controller.onPlayWhenReadyChanged`), because the caller is the
     * only thing that knows which of the two wrote it.
     */
    data class PlaybackRequested(val requested: Boolean) : SurfaceEvent

    /**
     * The app is hidden. Android has no hidden page: this is
     * `presentationForeground` inverted. While hidden, `presenting` samples are
     * ignored and every timer freezes.
     */
    data class Hidden(val hidden: Boolean) : SurfaceEvent

    /** Timers advance to `nowMs`; timed notices expire. */
    data object Tick : SurfaceEvent
}

/** The reducer's whole state. Immutable; the caller carries it forward. */
internal data class SurfaceState(
    val faults: List<PlaybackFault> = emptyList(),
    val attached: Long? = null,
    val hidden: Boolean = false,
    val hiddenSinceMs: Long? = null,
    val presenting: Boolean = false,
    val presentingSinceMs: Long? = null,
    val nowMs: Long = 0,
    val seq: Long = 0,
)

/** One log line for the client log and the ledger ring. */
internal data class SurfaceLog(
    val event: String,
    /** The fault's own raise order. The ledger pairs a clear to its raise by it. */
    val seq: Long? = null,
    val cls: SurfaceClass? = null,
    val source: String? = null,
    val attached: Long? = null,
    val intent: Long? = null,
    val positionMs: Long? = null,
    val actions: List<SurfaceAction> = emptyList(),
    val playerStopped: Boolean = false,
    val by: String? = null,
    val action: SurfaceAction? = null,
    val error: String? = null,
    val context: String? = null,
    /**
     * What the owner said about this event.
     *
     * Every other field is an identity or a class. `log_only` (§3.3 row 18) has
     * neither — it maps to no class at all — so without this the one event that
     * IS the whole trace would reach the client log saying only that something
     * happened.
     */
    val detail: String? = null,
)

internal data class SurfaceStep(
    val state: SurfaceState,
    val surface: PlaybackSurface,
    val log: List<SurfaceLog>,
)

/**
 * Pure. `apply(state, event, nowMs) -> (state, surface, log)`.
 *
 * No Android imports, no player, no timers of its own: timers are the caller's
 * [SurfaceEvent.Tick] events and evidence is the caller's
 * [SurfaceEvent.Presenting] samples.
 */
internal class PlaybackSurfaceReducer {

    fun apply(state: SurfaceState, event: SurfaceEvent, nowMs: Long): SurfaceStep {
        val log = mutableListOf<SurfaceLog>()
        var next = state.copy(nowMs = nowMs)

        // Inert by contract: an inert event returns before the sweep, so it
        // cannot run a timer that has come due either.
        if (event is SurfaceEvent.Inert) {
            return SurfaceStep(next, surfaceFrom(next), log)
        }

        next = when (event) {
            is SurfaceEvent.Attach -> applyAttach(next, log, event.generation)
            is SurfaceEvent.Retire -> applyRetire(next, log, event.generation)
            is SurfaceEvent.Hidden -> applyHidden(next, event.hidden)
            is SurfaceEvent.Presenting -> applyPresenting(next, log, event)
            is SurfaceEvent.Raise -> applyRaise(next, log, event)
            is SurfaceEvent.IntentSettled -> dropFaults(next, log, "intent_settled") { fault ->
                fault.intent == event.intent &&
                    fault.cls.retiredBy.contains(SurfaceRetirement.IntentSettled)
            }
            is SurfaceEvent.IntentSuperseded -> dropFaults(next, log, "intent_superseded") { fault ->
                fault.intent == event.intent &&
                    fault.cls.retiredBy.contains(SurfaceRetirement.IntentSuperseded)
            }
            // One recovery owner per player: its success retires every
            // `recovering` fault, not only the ones about the generation it
            // replaced.
            is SurfaceEvent.OwnerSuccess -> dropFaults(next, log, "owner_success") { fault ->
                fault.cls.retiredBy.contains(SurfaceRetirement.OwnerSuccess)
            }
            is SurfaceEvent.PlaybackRequested -> if (event.requested) {
                next
            } else {
                dropFaults(next, log, SurfaceRetirement.PlaybackNotRequested.wire) { fault ->
                    fault.cls.retiredBy.contains(SurfaceRetirement.PlaybackNotRequested)
                }
            }
            is SurfaceEvent.UserAction -> applyUserAction(next, log, event.action)
            SurfaceEvent.Tick -> next
            is SurfaceEvent.Inert -> next // returned above; here for exhaustiveness
        }

        next = sweep(next, log)
        return SurfaceStep(next, surfaceFrom(next), log)
    }

    // ---------------------------------------------------------------- events

    private fun applyAttach(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        generation: Long,
    ): SurfaceState =
        dropFaults(state, log, "attached_retired") { it.attached != generation }
            .copy(attached = generation, presenting = false, presentingSinceMs = null)

    private fun applyRetire(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        generation: Long,
    ): SurfaceState {
        val dropped = dropFaults(state, log, "attached_retired") { it.attached == generation }
        return if (dropped.attached == generation) {
            dropped.copy(attached = null, presenting = false, presentingSinceMs = null)
        } else {
            dropped
        }
    }

    private fun applyHidden(state: SurfaceState, hidden: Boolean): SurfaceState = when {
        hidden && !state.hidden -> state.copy(hidden = true, hiddenSinceMs = state.nowMs)
        !hidden && state.hidden -> {
            // Carry the hidden interval forward rather than letting wall time
            // run under a frozen surface: a 30 s hold the viewer backgrounded
            // for a minute has not been on screen for 30 s. Nothing sampled the
            // picture while the app was away, so the continuous-presenting
            // clock restarts.
            val away = state.hiddenSinceMs?.let { state.nowMs - it } ?: 0L
            state.copy(
                faults = if (away > 0) {
                    state.faults.map { it.copy(raisedAtMs = it.raisedAtMs + away) }
                } else {
                    state.faults
                },
                hidden = false,
                hiddenSinceMs = null,
                presenting = false,
                presentingSinceMs = null,
            )
        }
        else -> state.copy(hidden = hidden)
    }

    private fun applyPresenting(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        event: SurfaceEvent.Presenting,
    ): SurfaceState {
        // A hidden app samples nothing, and evidence about a generation that is
        // no longer attached is about a picture nobody is watching.
        val generation = event.attached ?: return state
        if (state.hidden || generation != state.attached) return state
        if (!event.presenting) return state.copy(presenting = false, presentingSinceMs = null)
        val begun = if (state.presenting) {
            state
        } else {
            state.copy(presenting = true, presentingSinceMs = state.nowMs)
        }
        return resolveDisagreement(begun, log, generation)
    }

    private fun applyRaise(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        event: SurfaceEvent.Raise,
    ): SurfaceState {
        var sawId = false
        var row: SurfaceSourceRow? = null
        for (candidate in SURFACE_SOURCES) {
            if (candidate.id != event.source) continue
            sawId = true
            if (candidate.matches(event.context)) {
                row = candidate
                break
            }
        }
        if (row == null) {
            log += SurfaceLog(
                event = SurfaceLogEvents.ERROR,
                error = if (sawId) SurfaceErrors.SOURCE_CONTEXT_MISMATCH else SurfaceErrors.UNKNOWN_SOURCE,
                source = event.source,
                context = event.context.wire,
            )
            return state
        }
        val cls = row.cls
        if (cls == null) {
            log += SurfaceLog(
                event = SurfaceLogEvents.LOG_ONLY,
                source = row.id,
                attached = event.attached,
                detail = event.detail,
            )
            return state
        }
        if (cls.blocking == SurfaceBlocking.Always && !event.playerStopped) {
            // The whole point: the presenter never renders a blocking surface
            // over a player its recovery owner has not already stopped.
            log += SurfaceLog(
                event = SurfaceLogEvents.ERROR,
                error = SurfaceErrors.BLOCKING_WITHOUT_STOP,
                source = row.id,
                cls = cls,
                attached = event.attached,
            )
            return state
        }

        val rowActions = event.actions ?: row.actions ?: cls.defaultActions
        val promotedIndex = if (SURFACE_PROMOTING_SOURCES.contains(row.id)) {
            state.faults.indexOfFirst { fault ->
                fault.attached == event.attached && fault.thenWhenStopped == cls && !fault.demoted
            }.takeIf { it >= 0 }
        } else {
            null
        }

        var next: SurfaceState
        if (promotedIndex != null) {
            val existing = state.faults[promotedIndex]
            val promoted = existing.copy(
                cls = cls,
                playerStopped = event.playerStopped,
                raisedAtMs = state.nowMs,
                thenWhenStopped = null,
                title = event.title ?: existing.title,
                detail = event.detail ?: existing.detail,
                actions = when {
                    event.actions != null -> event.actions
                    existing.actions.isEmpty() -> rowActions
                    else -> existing.actions
                },
            )
            next = state.copy(
                faults = state.faults.toMutableList().also { it[promotedIndex] = promoted },
            )
            log += faultLog(SurfaceLogEvents.RAISED, promoted, by = SurfaceSources.OWNER_STOPPED)
        } else {
            val seq = state.seq + 1
            val fault = PlaybackFault(
                cls = cls,
                source = row.id,
                attached = event.attached,
                intent = event.intent,
                raisedAtMs = state.nowMs,
                positionMs = event.positionMs,
                title = event.title ?: cls.title,
                detail = event.detail,
                actions = rowActions,
                playerStopped = event.playerStopped,
                thenWhenStopped = row.thenWhenStopped,
                seq = seq,
            )
            next = state.copy(faults = state.faults + fault, seq = seq)
            log += faultLog(SurfaceLogEvents.RAISED, fault)
        }

        // A blocking fault raised over a picture that is presenting is a
        // disagreement the moment it is raised, not whenever the next evidence
        // sample happens to arrive.
        if (cls.blocking == SurfaceBlocking.Always &&
            next.presenting &&
            next.attached == event.attached
        ) {
            next = resolveDisagreement(next, log, event.attached)
        }
        return next
    }

    private fun applyUserAction(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        action: SurfaceAction,
    ): SurfaceState {
        val current = surfaceFrom(state).fault
        val targetSeq: Long? = if (current != null && current.actions.contains(action)) {
            current.seq
        } else {
            state.faults.firstOrNull { it.actions.contains(action) }?.seq
        }
        if (targetSeq == null) return state
        return dropFaults(state, log, "user", action) { it.seq == targetSeq }
    }

    // ------------------------------------------------------------ retirement

    private fun dropFaults(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        by: String,
        action: SurfaceAction? = null,
        predicate: (PlaybackFault) -> Boolean,
    ): SurfaceState {
        if (state.faults.none(predicate)) return state
        val kept = mutableListOf<PlaybackFault>()
        for (fault in state.faults) {
            if (predicate(fault)) {
                log += faultLog(SurfaceLogEvents.CLEARED, fault, by = by, action = action)
            } else {
                kept += fault
            }
        }
        return state.copy(faults = kept)
    }

    /**
     * `presenting_after_raise` only. The run of presentation has to have BEGUN
     * at or after the fault was raised: the picture that was already on screen
     * when the server said "held" is not proof the hold is over, and an owner
     * that reopens in place produces exactly this, which is why `recovering`
     * does not need a new generation to be retired.
     *
     * Plain `presenting` is NOT gated by it, and gating it was a bug: a picture
     * that is presenting is not buffering and is not preparing, whenever its
     * run began. A `media_waiting` raised over a picture that never stopped had
     * no evidence that could ever postdate it, so the spinner stayed up for the
     * rest of the film. The classes that need the stronger proof say so in
     * `retiredBy`.
     */
    private fun evidencePostdates(state: SurfaceState, fault: PlaybackFault): Boolean {
        val since = state.presentingSinceMs ?: return false
        return since >= fault.raisedAtMs
    }

    /** Continuous means continuous: a picture not presenting now has a run of nothing. */
    private fun continuousElapsed(state: SurfaceState, fault: PlaybackFault): Long? {
        if (!state.presenting) return null
        val since = state.presentingSinceMs ?: return null
        return state.nowMs - maxOf(since, fault.raisedAtMs)
    }

    private fun sweep(state: SurfaceState, log: MutableList<SurfaceLog>): SurfaceState {
        // A hidden app samples nothing and expires nothing. The clock it is
        // measured against is rewound when it comes back (see `applyHidden`).
        if (state.hidden) return state
        val reasons = HashMap<Long, String>()
        for (fault in state.faults) {
            val cls = fault.cls
            val timed = cls.timedMs
            if (timed != null &&
                cls.retiredBy.contains(SurfaceRetirement.Timer) &&
                !(cls.timerPausedWhileActions && fault.actions.isNotEmpty()) &&
                state.nowMs - fault.raisedAtMs >= timed
            ) {
                reasons[fault.seq] = "timer"
                continue
            }
            if (cls.retiredBy.contains(SurfaceRetirement.PresentingContinuousMs)) {
                val bound = cls.continuousMs
                val elapsed = continuousElapsed(state, fault)
                if (bound != null && elapsed != null && elapsed >= bound) {
                    reasons[fault.seq] = "presenting"
                    continue
                }
            }
            // Evidence never retires a fault about a pending destination.
            if (fault.intent != null) continue
            if (!state.presenting) continue
            if (cls.retiredBy.contains(SurfaceRetirement.Presenting)) {
                reasons[fault.seq] = "presenting"
                continue
            }
            if (cls.retiredBy.contains(SurfaceRetirement.PresentingAfterRaise) &&
                evidencePostdates(state, fault)
            ) {
                reasons[fault.seq] = "presenting"
            }
        }
        if (reasons.isEmpty()) return state
        val kept = mutableListOf<PlaybackFault>()
        for (fault in state.faults) {
            val by = reasons[fault.seq]
            if (by != null) {
                log += faultLog(SurfaceLogEvents.CLEARED, fault, by = by)
            } else {
                kept += fault
            }
        }
        return state.copy(faults = kept)
    }

    /**
     * The agreement rule (§3.2): a blocking surface over a moving picture is a
     * disagreement, and the picture wins. The fault keeps its actions and its
     * data — a `media_owner_lost` still carries its position and its Try again —
     * so when the buffer drains the viewer gets the specific recovery. Nothing
     * is re-paused and no timer is cancelled: the owner's detectors are running
     * because the owner never stopped them.
     */
    private fun resolveDisagreement(
        state: SurfaceState,
        log: MutableList<SurfaceLog>,
        generation: Long,
    ): SurfaceState {
        val affected = state.faults.any {
            it.cls.blocking == SurfaceBlocking.Always && it.attached == generation
        }
        if (!affected) return state
        val faults = state.faults.map { fault ->
            if (fault.cls.blocking != SurfaceBlocking.Always || fault.attached != generation) {
                fault
            } else {
                log += faultLog(SurfaceLogEvents.DISAGREEMENT, fault, by = "presenting")
                fault.copy(
                    cls = SurfaceClass.Degraded,
                    demoted = true,
                    title = "Playback recovered",
                    raisedAtMs = state.nowMs,
                )
            }
        }
        return state.copy(faults = faults)
    }

    // --------------------------------------------------------------- surface

    /** `minMs` debounces the SURFACE; the fault exists from the moment it is raised. */
    private fun drawable(state: SurfaceState, fault: PlaybackFault): Boolean {
        val minMs = fault.cls.minMs ?: return true
        return state.nowMs - fault.raisedAtMs >= minMs
    }

    private fun surfaceFrom(state: SurfaceState): PlaybackSurface {
        var chosen: PlaybackFault? = null
        for (fault in state.faults) {
            if (!drawable(state, fault)) continue
            val current = chosen
            // Highest severity owns the surface; equal severity breaks by recency.
            if (current == null ||
                fault.cls.severity.rank > current.cls.severity.rank ||
                (fault.cls.severity.rank == current.cls.severity.rank && fault.seq > current.seq)
            ) {
                chosen = fault
            }
        }
        val fault = chosen ?: return PlaybackSurface.None
        return when (fault.cls.blocking) {
            SurfaceBlocking.Always -> PlaybackSurface.Blocking(fault)
            SurfaceBlocking.WhileNotPresenting ->
                if (state.presenting) PlaybackSurface.Indicator(fault) else PlaybackSurface.Blocking(fault)
            SurfaceBlocking.Never -> PlaybackSurface.Banner(fault)
        }
    }

    private fun faultLog(
        event: String,
        fault: PlaybackFault,
        by: String? = null,
        action: SurfaceAction? = null,
    ): SurfaceLog = SurfaceLog(
        event = event,
        seq = fault.seq,
        cls = fault.cls,
        source = fault.source,
        attached = fault.attached,
        intent = fault.intent,
        positionMs = fault.positionMs,
        actions = fault.actions,
        playerStopped = fault.playerStopped,
        by = by,
        action = action,
    )
}
