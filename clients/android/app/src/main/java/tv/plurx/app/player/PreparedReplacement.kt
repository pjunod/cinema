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
 * build on [Offered] and must release everything it built on [Released].
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
    FAILING,
    ABORTING,
}

/**
 * One offer and one ordered acknowledgement ladder.
 *
 * There is intentionally no ExoPlayer here. The coordinator owns only protocol
 * state: repeated-offer identity, exact origin echoing, monotonic progress,
 * silent-discard detection, expiry, and teardown notifications.
 */
internal class PreparedReplacementCoordinator {
    private var offer: PreparedReplacementOffer? = null
    private var phase: PreparedPhase? = null
    private var offeredAtMs = 0L
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
        nowMs: Long,
    ): List<PreparedReplacementEvent> {
        val events = expireLocked(nowMs).toMutableList()
        if (action.type != PlaybackControl.PREPARE_ACTION_TYPE) {
            settleWithoutOffer(sent, events)
            return events
        }

        val inbound = PreparedReplacementOffer.from(action) ?: return events
        val current = offer
        if (current == null) {
            start(inbound, nowMs)
            events += PreparedReplacementEvent.Offered(inbound)
            return events
        }
        if (current.actionId != inbound.actionId) {
            events += releaseLocked(PreparedReplacementReleaseReason.SUPERSEDED)
            start(inbound, nowMs)
            events += PreparedReplacementEvent.Offered(inbound)
            return events
        }

        val queued = pending.firstOrNull()
        if (sent != null &&
            queued?.actionId == sent.actionId &&
            queued.state == sent.state
        ) {
            when (sent.state) {
                AcknowledgementState.METADATA_READY,
                AcknowledgementState.BUFFER_READY,
                -> pending.removeFirst()

                AcknowledgementState.COMMITTED -> {
                    // A commit is the only acknowledgement whose successful
                    // response cannot still carry the offer. Persistence is a
                    // silent discard (most importantly, an origin mismatch),
                    // so release locally and explicitly abort the staging.
                    pending.clear()
                    phase = PreparedPhase.ABORTING
                    pending += ActionAcknowledgement(
                        actionId = current.actionId,
                        state = AcknowledgementState.ABORTED,
                    )
                    events += PreparedReplacementEvent.Released(
                        current,
                        PreparedReplacementReleaseReason.COMMIT_DISCARDED,
                    )
                }

                AcknowledgementState.FAILED,
                AcknowledgementState.ABORTED,
                -> pending.removeFirst()
            }
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
            return releaseIfPresent(PreparedReplacementReleaseReason.PROTOCOL_REFUSED)
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
        return releaseIfPresent(reason)
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
    fun committed(actionId: String, firstFrameUnixMs: Long): Boolean {
        val current = offer ?: return false
        if (current.actionId != actionId || phase != PreparedPhase.BUFFER_READY) return false
        val acknowledgement = ActionAcknowledgement(
            actionId,
            AcknowledgementState.COMMITTED,
            committedMediaOriginMs = current.mediaOriginMs,
            firstFrameUnixMs = firstFrameUnixMs,
        )
        if (!acknowledgement.isValid) return false
        phase = PreparedPhase.COMMITTING
        pending += acknowledgement
        return true
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
    fun reset(reason: PreparedReplacementReleaseReason): List<PreparedReplacementEvent> =
        releaseIfPresent(reason)

    private fun settleWithoutOffer(
        sent: ActionAcknowledgement?,
        events: MutableList<PreparedReplacementEvent>,
    ) {
        val current = offer ?: return
        if (sent != null && pending.firstOrNull() == sent) pending.removeFirst()
        val reason = if (sent?.state == AcknowledgementState.COMMITTED) {
            PreparedReplacementReleaseReason.COMMITTED
        } else {
            PreparedReplacementReleaseReason.WITHDRAWN
        }
        events += PreparedReplacementEvent.Released(current, reason)
        clear()
    }

    private fun terminal(
        actionId: String,
        state: AcknowledgementState,
        reason: PreparedReplacementReleaseReason,
    ): PreparedReplacementEvent.Released? {
        val current = offer ?: return null
        if (current.actionId != actionId || phase == PreparedPhase.COMMITTING) return null
        pending.clear()
        phase = if (state == AcknowledgementState.FAILED) {
            PreparedPhase.FAILING
        } else {
            PreparedPhase.ABORTING
        }
        pending += ActionAcknowledgement(actionId, state)
        return PreparedReplacementEvent.Released(current, reason)
    }

    private fun expireLocked(nowMs: Long): List<PreparedReplacementEvent> {
        val current = offer ?: return emptyList()
        if (nowMs - offeredAtMs < PREPARED_OFFER_TTL_MS) return emptyList()
        clear()
        return listOf(
            PreparedReplacementEvent.Released(
                current,
                PreparedReplacementReleaseReason.EXPIRED,
            ),
        )
    }

    private fun releaseIfPresent(
        reason: PreparedReplacementReleaseReason,
    ): List<PreparedReplacementEvent> {
        if (offer == null) return emptyList()
        val event = releaseLocked(reason)
        return listOf(event)
    }

    private fun releaseLocked(
        reason: PreparedReplacementReleaseReason,
    ): PreparedReplacementEvent.Released {
        val current = checkNotNull(offer)
        clear()
        return PreparedReplacementEvent.Released(current, reason)
    }

    private fun start(inbound: PreparedReplacementOffer, nowMs: Long) {
        offer = inbound
        phase = PreparedPhase.STAGED
        offeredAtMs = nowMs
        pending.clear()
    }

    private fun clear() {
        offer = null
        phase = null
        offeredAtMs = 0
        pending.clear()
    }
}
