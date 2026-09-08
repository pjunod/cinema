package tv.plurx.app.player

/** The client-side lifetime used when the server gives no exact expiry timestamp. */
internal const val PREPARED_OFFER_TTL_MS = 330_000L

/** A validated `prepare` action, kept intact so its origin can be echoed verbatim. */
data class PreparedReplacementOffer(
    val actionId: String,
    val sessionId: String,
    val playlistUrl: String,
    val mediaOriginMs: Long,
    val effectiveSelection: EffectiveSelection,
) {
    companion object {
        internal fun from(action: ControlAction): PreparedReplacementOffer? {
            if (!action.preparedPayloadIsValid) return null
            return PreparedReplacementOffer(
                actionId = action.actionId ?: return null,
                sessionId = action.sessionId ?: return null,
                playlistUrl = action.playlistUrl ?: return null,
                mediaOriginMs = action.mediaOriginMs ?: return null,
                effectiveSelection = action.effectiveSelection ?: return null,
            )
        }
    }
}

enum class PreparedReplacementReleaseReason {
    COMMITTED,
    FAILED,
    ABORTED,
    WITHDRAWN,
    EXPIRED,
    SUPERSEDED,
    COMMIT_DISCARDED,
    ACKNOWLEDGEMENT_REJECTED,
    OWNER_CHANGED,
    SESSION_GONE,
    SESSION_ENDED,
    OWNER_LOST,
    PROTOCOL_REFUSED,
}

/**
 * The marked seam for the later presentation PR.
 *
 * This release never builds a second player. A future presentation owner may
 * build on [Offered] and must settle its preparation exactly once on
 * [Released].
 */
sealed interface PreparedReplacementEvent {
    data class Offered(val offer: PreparedReplacementOffer) : PreparedReplacementEvent

    data class Released(
        val offer: PreparedReplacementOffer,
        val reason: PreparedReplacementReleaseReason,
    ) : PreparedReplacementEvent
}

private enum class PreparedPhase {
    STAGED,
    METADATA_READY,
    BUFFER_READY,
    COMMITTING,
}

internal data class PreparedCommitResult(
    val queued: Boolean,
    val released: PreparedReplacementEvent.Released? = null,
)

/**
 * One offer and one ordered acknowledgement ladder.
 *
 * There is intentionally no ExoPlayer here. The coordinator owns only protocol
 * state: repeated-offer identity, exact origin echoing, selection fencing,
 * silent-discard detection, expiry, and exactly-once teardown notifications.
 */
internal class PreparedReplacementCoordinator {
    private var offer: PreparedReplacementOffer? = null
    private var phase: PreparedPhase? = null
    private var offeredAtMs = 0L
    private var stagedSelection: ClientSelection? = null
    private var releasedActionId: String? = null
    private val pending = ArrayDeque<ActionAcknowledgement>()

    @get:Synchronized
    val nextAcknowledgement: ActionAcknowledgement?
        get() = pending.firstOrNull()

    @get:Synchronized
    val activeOffer: PreparedReplacementOffer?
        get() = offer

    @Synchronized
    fun receive(
        action: ControlAction,
        sent: ActionAcknowledgement?,
        requestSelection: ClientSelection,
        nowMs: Long,
    ): List<PreparedReplacementEvent> {
        val events = expireLocked(nowMs).toMutableList()
        val inbound = if (action.type == PlaybackControl.PREPARE_ACTION_TYPE) {
            PreparedReplacementOffer.from(action)
        } else {
            null
        }
        val queued = pending.firstOrNull()
        val sentMatches = sent != null &&
            queued?.actionId == sent.actionId &&
            queued.state == sent.state
        val current = offer

        if (current != null && sentMatches && sent.state == AcknowledgementState.COMMITTED) {
            if (inbound?.actionId == current.actionId) {
                // A successful commit response cannot retain the offer. Its
                // persistence is the server's silent discard signal, most
                // importantly for an origin mismatch. Release once and ask
                // the server to abort whatever it still has staged.
                pending.clear()
                pending += ActionAcknowledgement(
                    actionId = current.actionId,
                    state = AcknowledgementState.ABORTED,
                )
                events += releaseActive(
                    PreparedReplacementReleaseReason.COMMIT_DISCARDED,
                    keepPending = true,
                )
                return events
            }
            pending.removeFirst()
            events += releaseActive(PreparedReplacementReleaseReason.COMMITTED)
        } else if (sentMatches) {
            pending.removeFirst()
        }

        if (action.type != PlaybackControl.PREPARE_ACTION_TYPE) {
            offer?.let {
                events += releaseActive(PreparedReplacementReleaseReason.WITHDRAWN)
            }
            if (pending.isEmpty()) releasedActionId = null
            return events
        }

        val prepared = inbound ?: return events
        val active = offer
        if (active == null) {
            // A terminal acknowledgement is delivered after its local
            // preparation has already been released. If an old owner repeats
            // that same action in the response, it is not a new offer.
            if (prepared.actionId == releasedActionId) return events
            start(prepared, requestSelection, nowMs)
            events += PreparedReplacementEvent.Offered(prepared)
            return events
        }
        if (active.actionId != prepared.actionId) {
            events += releaseActive(PreparedReplacementReleaseReason.SUPERSEDED)
            start(prepared, requestSelection, nowMs)
            events += PreparedReplacementEvent.Offered(prepared)
        }
        return events
    }

    @Synchronized
    fun failure(
        failure: Throwable?,
        sent: ActionAcknowledgement?,
        requestGeneration: String,
    ): List<PreparedReplacementEvent> {
        val transport = failure as? ControlTransportException ?: run {
            return releaseForTerminalFailure(PreparedReplacementReleaseReason.PROTOCOL_REFUSED)
        }
        val retryable = (transport.status == 425 && transport.code == "owner_transition") ||
            (transport.status == 429 && transport.code == "control_rate_limited") ||
            (transport.status == 503 && transport.code == "control_unavailable") ||
            transport.status == 408 ||
            transport.status == null
        if (retryable) return emptyList()

        val reason = when {
            transport.isPreparationAcknowledgementRejected && sent != null ->
                PreparedReplacementReleaseReason.ACKNOWLEDGEMENT_REJECTED
            transport.status == 409 && transport.code == "owner_changed" ->
                PreparedReplacementReleaseReason.OWNER_CHANGED
            transport.status == 409 &&
                transport.code == "stale_control" &&
                transport.generation != null &&
                transport.generation != requestGeneration ->
                PreparedReplacementReleaseReason.OWNER_CHANGED
            transport.status == 404 && transport.code == "session_gone" ->
                PreparedReplacementReleaseReason.SESSION_GONE
            transport.status == 410 && transport.code == "session_ended" ->
                PreparedReplacementReleaseReason.SESSION_ENDED
            transport.status == 410 && transport.code == "owner_lost" ->
                PreparedReplacementReleaseReason.OWNER_LOST
            else -> PreparedReplacementReleaseReason.PROTOCOL_REFUSED
        }
        return releaseForTerminalFailure(reason)
    }

    @Synchronized
    fun metadataReady(actionId: String): Boolean {
        if (offer?.actionId != actionId || phase != PreparedPhase.STAGED) return false
        phase = PreparedPhase.METADATA_READY
        pending += ActionAcknowledgement(actionId, AcknowledgementState.METADATA_READY)
        return true
    }

    @Synchronized
    fun bufferReady(actionId: String, bufferedThroughMs: Long): Boolean {
        if (offer?.actionId != actionId || phase != PreparedPhase.METADATA_READY) return false
        val acknowledgement = ActionAcknowledgement(
            actionId,
            AcknowledgementState.BUFFER_READY,
            bufferedThroughMs = bufferedThroughMs,
        )
        if (!acknowledgement.isValid) return false
        phase = PreparedPhase.BUFFER_READY
        pending += acknowledgement
        return true
    }

    @Synchronized
    fun committed(
        actionId: String,
        firstFrameUnixMs: Long,
        currentSelection: ClientSelection?,
        demand: PlaybackDemand?,
    ): PreparedCommitResult {
        val current = offer ?: return PreparedCommitResult(false)
        if (current.actionId != actionId || phase != PreparedPhase.BUFFER_READY) {
            return PreparedCommitResult(false)
        }
        if (currentSelection == null ||
            currentSelection != stagedSelection ||
            demand == PlaybackDemand.END
        ) {
            return PreparedCommitResult(
                queued = false,
                released = releaseWithTerminalAcknowledgement(
                    if (demand == PlaybackDemand.END) {
                        PreparedReplacementReleaseReason.SESSION_ENDED
                    } else {
                        PreparedReplacementReleaseReason.ABORTED
                    },
                ),
            )
        }
        val acknowledgement = ActionAcknowledgement(
            actionId,
            AcknowledgementState.COMMITTED,
            committedMediaOriginMs = current.mediaOriginMs,
            firstFrameUnixMs = firstFrameUnixMs,
        )
        if (!acknowledgement.isValid) return PreparedCommitResult(false)
        phase = PreparedPhase.COMMITTING
        pending += acknowledgement
        return PreparedCommitResult(true)
    }

    @Synchronized
    fun reconcile(
        currentSelection: ClientSelection,
        demand: PlaybackDemand,
    ): PreparedReplacementEvent.Released? {
        if (offer == null) return null
        val reason = when {
            demand == PlaybackDemand.END -> PreparedReplacementReleaseReason.SESSION_ENDED
            currentSelection != stagedSelection -> PreparedReplacementReleaseReason.ABORTED
            else -> return null
        }
        return releaseWithTerminalAcknowledgement(reason)
    }

    @Synchronized
    fun failed(actionId: String): PreparedReplacementEvent.Released? =
        terminal(actionId, AcknowledgementState.FAILED, PreparedReplacementReleaseReason.FAILED)

    @Synchronized
    fun aborted(actionId: String): PreparedReplacementEvent.Released? =
        terminal(actionId, AcknowledgementState.ABORTED, PreparedReplacementReleaseReason.ABORTED)

    @Synchronized
    fun expire(nowMs: Long): List<PreparedReplacementEvent> = expireLocked(nowMs)

    @Synchronized
    fun reset(reason: PreparedReplacementReleaseReason): List<PreparedReplacementEvent> {
        val event = offer?.let { releaseActive(reason) }
        clearAll()
        return listOfNotNull(event)
    }

    private fun terminal(
        actionId: String,
        state: AcknowledgementState,
        reason: PreparedReplacementReleaseReason,
    ): PreparedReplacementEvent.Released? {
        val current = offer ?: return null
        if (current.actionId != actionId || phase == PreparedPhase.COMMITTING) return null
        return releaseWithTerminalAcknowledgement(reason, state)
    }

    private fun releaseWithTerminalAcknowledgement(
        reason: PreparedReplacementReleaseReason,
        state: AcknowledgementState = AcknowledgementState.ABORTED,
    ): PreparedReplacementEvent.Released {
        val current = checkNotNull(offer)
        pending.clear()
        pending += ActionAcknowledgement(current.actionId, state)
        return releaseActive(reason, keepPending = true)
    }

    private fun expireLocked(nowMs: Long): List<PreparedReplacementEvent> {
        val current = offer ?: return emptyList()
        if (nowMs - offeredAtMs < PREPARED_OFFER_TTL_MS) return emptyList()
        return listOf(releaseActive(PreparedReplacementReleaseReason.EXPIRED))
    }

    private fun releaseForTerminalFailure(
        reason: PreparedReplacementReleaseReason,
    ): List<PreparedReplacementEvent> {
        val event = offer?.let { releaseActive(reason) }
        clearAll()
        return listOfNotNull(event)
    }

    private fun releaseActive(
        reason: PreparedReplacementReleaseReason,
        keepPending: Boolean = false,
    ): PreparedReplacementEvent.Released {
        val current = checkNotNull(offer)
        offer = null
        phase = null
        offeredAtMs = 0
        stagedSelection = null
        releasedActionId = current.actionId
        if (!keepPending) pending.clear()
        return PreparedReplacementEvent.Released(current, reason)
    }

    private fun start(
        inbound: PreparedReplacementOffer,
        requestSelection: ClientSelection,
        nowMs: Long,
    ) {
        offer = inbound
        phase = PreparedPhase.STAGED
        offeredAtMs = nowMs
        stagedSelection = requestSelection
        releasedActionId = null
        pending.clear()
    }

    private fun clearAll() {
        offer = null
        phase = null
        offeredAtMs = 0
        stagedSelection = null
        releasedActionId = null
        pending.clear()
    }
}
