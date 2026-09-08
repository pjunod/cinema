package tv.plurx.app.player

/**
 * Turning what Media3 is doing into what the protocol says.
 *
 * The rules live here rather than in the player screen for one reason: the web
 * client already decided them (`crates/plurxd/src/web/index.html`,
 * `playbackControlSnapshot`), and three platforms answering the same question
 * differently is worse than any of the three answers. Keeping the mapping as a
 * pure function over an explicit observation means the whole of it is testable
 * and the untestable part shrinks to filling in fields.
 *
 * `clients/apple/Sources/PlaybackControlSnapshotMapper.swift` is the same
 * function in Swift, boundary for boundary.
 */
object PlaybackControlMapping {
    /**
     * A stream whose bytes end within this much of the title's duration has
     * actually finished. Ending earlier is a truncated stream the reopen path
     * recovers, so it stays active failed demand.
     */
    const val ENDED_SLACK_MS = 15_000L

    /**
     * A wait this long is no longer buffering. Past it the server should be
     * told the player is starved rather than merely waiting.
     */
    const val PERSISTENT_STALL_MS = 8_000L

    const val MIN_ACTIVE_RATE = 0.25
    const val MAX_RATE = 4.0

    /**
     * The whole mapping. Order matters and follows the web client's: a hard
     * error outranks an end, an end outranks a caller override, and an
     * override outranks everything the player itself reports.
     */
    fun snapshot(observation: PlayerControlObservation): PlaybackControlSnapshot {
        val position = clampedPosition(observation)
        val ended = terminallyEnded(observation, position)
        val render = renderState(observation, ended)
        val demand = demand(observation, ended)
        val range = bufferedRange(observation, position)
        return PlaybackControlSnapshot(
            demand = demand,
            positionMs = position,
            bufferedFromMs = range.first,
            bufferedThroughMs = range.second,
            playbackRate = playbackRate(observation, demand),
            renderState = render,
            // Only a seek reports a target, and the target *is* the position:
            // the playhead the viewer asked for, not the one Media3 is still
            // rendering.
            seekTargetMs = if (render == RenderState.SEEKING) position else null,
            observedDownloadBps = observation.observedDownloadBps?.takeIf { it > 0 },
            selection = observation.selection,
            capabilities = observation.capabilities,
            observation = clientObservation(observation, render),
            // Carried through untouched. The acknowledgement is a fact about a
            // transaction, not a reading of the player, so nothing here is
            // entitled to derive or adjust it — the one rule that does apply to
            // it (no `committed` on an ending exchange) belongs to the request
            // the reporter builds, where the demand is final.
            acknowledgement = observation.acknowledgement,
        )
    }

    fun clampedPosition(observation: PlayerControlObservation): Long {
        val position = maxOf(0L, observation.positionMs)
        if (observation.durationMs <= 0) return position
        return minOf(position, observation.durationMs)
    }

    /**
     * The bytes ending is not the title ending. A stream that stops well short
     * of the duration is truncated, and the client reopens it — so it reports
     * failed rather than ended, and keeps demanding.
     */
    fun terminallyEnded(observation: PlayerControlObservation, position: Long): Boolean {
        if (!observation.isEnded) return false
        if (observation.durationMs <= 0) return true
        return position >= maxOf(0L, observation.durationMs - ENDED_SLACK_MS)
    }

    fun demand(observation: PlayerControlObservation, terminallyEnded: Boolean): PlaybackDemand =
        when {
            terminallyEnded -> PlaybackDemand.END
            // A non-terminal end is a truncation being recovered from, so the
            // client still wants bytes.
            observation.isEnded -> PlaybackDemand.ACTIVE
            observation.isPaused -> PlaybackDemand.HOLD
            else -> PlaybackDemand.ACTIVE
        }

    fun renderState(
        observation: PlayerControlObservation,
        terminallyEnded: Boolean,
    ): RenderState = when {
        observation.errorCode != null -> RenderState.FAILED
        observation.isEnded -> if (terminallyEnded) RenderState.ENDED else RenderState.FAILED
        observation.renderOverride != null -> observation.renderOverride
        observation.isSeeking -> RenderState.SEEKING
        !observation.hasStarted -> RenderState.STARTING
        observation.waitingForMs != null ->
            if (observation.waitingForMs >= PERSISTENT_STALL_MS) RenderState.STALLED
            else RenderState.WAITING
        !observation.isPaused && !observation.isLikelyToKeepUp -> RenderState.WAITING
        else -> RenderState.RENDERING
    }

    /**
     * A player that means to play reports at least a quarter rate even while
     * its rate is momentarily zero — the protocol reads rate as intent, and
     * zero from an active player would read as a hold nobody asked for. A held
     * player reports what it actually has.
     */
    fun playbackRate(observation: PlayerControlObservation, demand: PlaybackDemand): Double {
        val rate = if (observation.rate.isFinite()) observation.rate else 0.0
        return if (demand == PlaybackDemand.ACTIVE) {
            minOf(MAX_RATE, maxOf(MIN_ACTIVE_RATE, if (rate == 0.0) 1.0 else rate))
        } else {
            minOf(MAX_RATE, maxOf(0.0, rate))
        }
    }

    /**
     * Runway is what can be played without another fetch, so only the range
     * containing the playhead counts. `through` never precedes the position
     * even when the loaded range ends just behind it.
     */
    fun bufferedRange(
        observation: PlayerControlObservation,
        position: Long,
    ): Pair<Long?, Long> {
        val through = observation.bufferedThroughMs ?: return null to position
        val from = observation.bufferedFromMs?.let { maxOf(0L, minOf(it, position)) }
        return from to maxOf(position, through)
    }

    fun clientObservation(
        observation: PlayerControlObservation,
        render: RenderState,
    ): ClientObservation? {
        var decoderState = when {
            observation.errorCode != null -> DecoderState.FAILED
            render == RenderState.STALLED -> DecoderState.STARVED
            observation.hasStarted -> DecoderState.READY
            else -> DecoderState.UNKNOWN
        }
        var droppedFrames = observation.droppedFrames?.takeIf { it >= 0 }
        var errorCode = observation.errorCode
        var errorDetail = if (errorCode == null) null else observation.errorDetail
        // A recovery path's evidence is more specific than anything derived
        // from the player's own state, so it wins field by field.
        observation.observationOverride?.let { override ->
            override.decoderState?.let { decoderState = it }
            override.droppedFrames?.let { droppedFrames = it }
            override.errorCode?.let {
                errorCode = it
                errorDetail = override.errorDetail
            }
        }
        return ClientObservation(droppedFrames, decoderState, errorCode, errorDetail).bounded()
    }
}

/**
 * What the player is doing, gathered at one instant.
 *
 * Every field is something the player screen already knows; nothing here is
 * derived. Media3's `Player` does not appear, so a test can state a player
 * state directly instead of building one.
 */
data class PlayerControlObservation(
    /** Film position, already rebased onto the title's timeline. */
    val positionMs: Long,
    /**
     * The title's duration where it is known, `0` where it is not (a growing
     * stream). Position is clamped to it, as the web client clamps.
     */
    val durationMs: Long,
    /**
     * The contiguous buffered range *containing the playhead*, in title time.
     * Null when no loaded range contains it — the protocol's runway is what
     * can be played without a fetch, so a disjoint range ahead is not runway.
     */
    val bufferedFromMs: Long? = null,
    val bufferedThroughMs: Long? = null,
    val rate: Double,
    val isPaused: Boolean,
    /** The bytes ran out. Not necessarily the title: see [ENDED_SLACK_MS]. */
    val isEnded: Boolean,
    val isSeeking: Boolean,
    /**
     * True once real playback has begun. Before it, the player is starting
     * however busy it looks.
     */
    val hasStarted: Boolean,
    /**
     * How long the player has been waiting for data, or null if it is not
     * waiting. [PERSISTENT_STALL_MS] separates waiting from stalled.
     */
    val waitingForMs: Long? = null,
    /**
     * The item is playable right now — Media3's `STATE_READY` with play-when-
     * ready honoured. A player that is not paused and not ready is waiting
     * even when it has not reported a wait.
     */
    val isLikelyToKeepUp: Boolean,
    val errorCode: ClientErrorCode? = null,
    val errorDetail: String? = null,
    val droppedFrames: Long? = null,
    val observedDownloadBps: Long? = null,
    /**
     * A recovery path knows things Media3 cannot report — whether a wait ran
     * out of bytes or stalled with bytes in hand, and which class of failure
     * it was. It overrides the derived observation, exactly as the web client
     * lets its recovery callbacks override.
     */
    val observationOverride: ClientObservation? = null,
    /**
     * A render state the caller knows better than this mapping does, for the
     * same reason.
     */
    val renderOverride: RenderState? = null,
    /**
     * How the prepared replacement this client was offered is going, when it
     * has something to say. Owned by [Controller]'s ledger, not derived here.
     */
    val acknowledgement: ActionAcknowledgement? = null,
    val selection: ClientSelection,
    val capabilities: DynamicCapabilities,
) {
    /**
     * The runway ahead of the playhead, in milliseconds. Zero when nothing
     * contiguous is loaded.
     */
    val runwayMs: Long
        get() = bufferedThroughMs?.let { maxOf(0L, it - positionMs) } ?: 0L
}

/**
 * The control protocol's view of this device, derived from the very map that
 * goes to `/decision`.
 *
 * Deriving rather than re-probing is the point: two independent probes would
 * eventually disagree, and then the server would be told one thing when it
 * decided and another while it played.
 */
fun controlCapabilities(
    query: Map<String, String>,
    preparedReplacementEnabled: Boolean = false,
): DynamicCapabilities {
    val codecs = query["vcodec"].orEmpty().split(",")
        .mapNotNull { name ->
            when (name.trim().lowercase()) {
                "h264" -> CodecPolicy.H264
                "hevc" -> CodecPolicy.HEVC
                "av1" -> CodecPolicy.AV1
                else -> null
            }
        }
        .distinct()
        // AVC decoding is mandatory on Android, and the protocol refuses an
        // empty codec list. An unreadable registry reports the baseline rather
        // than a claim the server cannot act on.
        .ifEmpty { listOf(CodecPolicy.H264) }

    val hdr = query["hdr"] == "1"
    val ranges = buildList {
        add(DynamicRangePolicy.SDR)
        if (hdr) {
            add(DynamicRangePolicy.HDR10)
            add(DynamicRangePolicy.HLG)
        }
        // `dv` is the same guarded claim the decision query makes: generic HDR
        // eligibility is not a Dolby Vision claim, and overclaiming makes the
        // server preserve DV metadata this device then refuses.
        if (hdr && query["dv"] == "1") add(DynamicRangePolicy.DOLBY_VISION)
    }

    return DynamicCapabilities(
        platform = "android",
        // The protocol's own ceiling. The capability query reports per-codec
        // decoder limits rather than one device height, and collapsing them
        // into a single number here would be a claim rather than a capability.
        maxHeight = PlaybackControl.MAX_HEIGHT,
        codecs = codecs,
        dynamicRanges = ranges,
        // Off unless the viewer turned it on in Settings → Developer, and off
        // is what every device that touches nothing reports.
        //
        // This is a hardware claim — "this platform can hold two live decode
        // pipelines" — and M5.5 measured it per device class, not per platform.
        // Both phones passed same-codec dual preparation 20 of 20; the tunneled
        // Google TV failed that same case 0 of 3, and same-codec is the *only*
        // kind of change the server ever prepares. So a platform-wide `true`
        // would authorise the server to prime a second pipeline on the one
        // device with a measured hard failure, and a platform-wide `false`
        // throws away two phones that passed. Protocol v1 has no way to say
        // "yes on phones, no on tunneled televisions" — that is a v2 decision
        // (`docs/M6-IMPLEMENTATION-HANDOFF.md` §3).
        //
        // Which leaves the narrowest honest answer available today: the person
        // holding the device decides, with the measurements in front of them.
        // The Developer screen lists what M5.5 found and whether this device
        // meets it, advisory and never blocking, so an operator on a phone can
        // have the feature and an operator on a television can see exactly what
        // they are taking on. The default is unchanged, which is what the
        // frozen literal was protecting.
        dualPlayerPreparation = preparedReplacementEnabled,
    )
}
