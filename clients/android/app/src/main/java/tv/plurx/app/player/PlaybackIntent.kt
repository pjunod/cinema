package tv.plurx.app.player

import tv.plurx.app.data.PlaybackQuality
import java.util.UUID
import kotlin.math.abs

/**
 * Viewer intent whose lifetime is the presentation, not one ExoPlayer or
 * server session.
 *
 * Quality changes rebuild the plan and controller. Keeping identity and the
 * pending target here prevents that replacement from looking like a new
 * viewer and from erasing the seek that caused it.
 */
class PlaybackIntent(
    val playbackId: String = UUID.randomUUID().toString(),
    initialQuality: PlaybackQuality,
) {
    data class PendingSeek(
        val sequence: Long,
        val targetMs: Long,
        val originMs: Long,
        val frameFloor: Long?,
    )

    /** The newest quality the viewer asked for, even before its plan exists. */
    var desiredQuality: PlaybackQuality = initialQuality
        private set
    /** Viewer transport intent survives plan/controller replacement too. */
    var playbackRequested: Boolean = true
        private set
    var pendingSeek: PendingSeek? = null
        private set
    var controlSequenceFloor: Long? = null
        private set
    private var nextSequence = 0L
    private var presentedFrames = 0L
    private var lastAudioPositionMs: Long? = null
    private var audioObservedAtMs = 0L
    private var audioActiveElapsedMs = 0L
    private var audioMediaElapsedMs = 0.0
    private var audioWasActive = false
    private var audioRate = 1.0

    /** Adopt the quality carried by the exact decision that a successor opens. */
    @Synchronized
    fun adoptQuality(quality: PlaybackQuality) {
        desiredQuality = quality
    }

    @Synchronized
    fun setPlaybackRequested(requested: Boolean) {
        playbackRequested = requested
    }

    @Synchronized
    fun beginSeek(
        targetMs: Long,
        originMs: Long,
        quality: PlaybackQuality = desiredQuality,
    ): PendingSeek {
        desiredQuality = quality
        lastAudioPositionMs = null
        return PendingSeek(
            ++nextSequence,
            targetMs.coerceAtLeast(0),
            originMs.coerceAtLeast(0),
            null,
        )
            .also { pendingSeek = it }
    }

    /**
     * Coalesce transport nudges against the newest optimistic destination,
     * not the player clock that remains at the old frame during the 100 ms
     * publication window.
     */
    @Synchronized
    fun beginRelativeSeek(
        deltaMs: Long,
        observedMs: Long,
        durationMs: Long,
        quality: PlaybackQuality = desiredQuality,
    ): PendingSeek {
        val ceiling = durationMs.takeIf { it > 0 } ?: Long.MAX_VALUE
        val base = pendingSeek?.targetMs ?: observedMs
        val target = if (deltaMs > 0 && base > Long.MAX_VALUE - deltaMs) {
            Long.MAX_VALUE
        } else if (deltaMs < 0 && base < Long.MIN_VALUE - deltaMs) {
            Long.MIN_VALUE
        } else {
            base + deltaMs
        }.coerceIn(0, ceiling)
        return beginSeek(target, observedMs, quality)
    }

    /** The media mutation for this generation has landed; departing frames do not count. */
    @Synchronized
    fun markExecuted(
        sequence: Long,
        observedAtMs: Long = monotonicNowMs(),
        playbackActive: Boolean = true,
        playbackRate: Double = 1.0,
    ): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence) return false
        lastAudioPositionMs = null
        audioObservedAtMs = observedAtMs
        audioActiveElapsedMs = 0
        audioMediaElapsedMs = 0.0
        audioWasActive = playbackActive
        audioRate = playbackRate.takeIf { it.isFinite() && it > 0 } ?: 0.0
        pendingSeek = pending.copy(frameFloor = presentedFrames)
        return true
    }

    @Synchronized
    fun isCurrent(sequence: Long): Boolean = pendingSeek?.sequence == sequence

    @Synchronized
    fun generation(): Long = nextSequence

    /** Stream changes inherit an optimistic seek until that target presents. */
    @Synchronized
    fun positionForPlaybackIntent(observedMs: Long): Long = pendingSeek?.targetMs ?: observedMs

    /** An in-place track change cannot acknowledge a seek it inherited. */
    @Synchronized
    fun needsSeekForInPlaceSelection(): Boolean = pendingSeek != null

    /** The current destination only becomes presentation-owned after mutation. */
    @Synchronized
    fun executedSequence(): Long? = pendingSeek
        ?.takeIf { it.frameFloor != null }
        ?.sequence

    /**
     * A later seek supersedes the old one. Only a new frame from the executed
     * generation, at the requested film position, may clear the destination.
     */
    @Synchronized
    fun presentedVideoFrame(positionMs: Long, sequence: Long? = pendingSeek?.sequence): Boolean {
        presentedFrames += 1
        val pending = pendingSeek ?: return false
        val frameFloor = pending.frameFloor ?: return false
        if (sequence != pending.sequence ||
            presentedFrames <= frameFloor ||
            abs(positionMs - pending.targetMs) > LANDING_TOLERANCE_MS
        ) {
            return false
        }
        pendingSeek = null
        controlSequenceFloor = null
        lastAudioPositionMs = null
        return true
    }

    /**
     * Audio-only equivalent: land at the target, then prove that the active
     * post-execution clock advances. Media3's `isPlaying` is only a state
     * predicate; a READY player can still freeze at the target.
     *
     * The second sample need not remain inside the 250 ms landing window. The
     * controller's shared monitor samples once a second, so requiring that
     * would make healthy audio-only playback impossible to settle.
     */
    @Synchronized
    fun presentedAudio(
        positionMs: Long,
        sequence: Long? = pendingSeek?.sequence,
        observedAtMs: Long = monotonicNowMs(),
        playbackActive: Boolean = true,
        playbackRate: Double = 1.0,
    ): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence || pending.frameFloor == null) {
            return false
        }
        val elapsed = (observedAtMs - audioObservedAtMs).coerceAtLeast(0)
        audioObservedAtMs = maxOf(audioObservedAtMs, observedAtMs)
        if (audioWasActive) {
            val active = elapsed.coerceAtMost((AUDIO_PRESENTATION_DEADLINE_MS - audioActiveElapsedMs).coerceAtLeast(0))
            audioActiveElapsedMs += active
            audioMediaElapsedMs += active * audioRate
        }
        audioWasActive = playbackActive
        audioRate = playbackRate.takeIf { it.isFinite() && it > 0 } ?: 0.0
        if (!playbackActive || audioRate == 0.0) {
            lastAudioPositionMs = null
            return false
        }
        // The monitor can first observe healthy audio a second after it
        // starts. Admit only the target-relative distance that the active
        // monotonic clock could have traversed, capped at the presentation
        // deadline. Paused/buffering time never widens this interval.
        val delta = positionMs.toDouble() - pending.targetMs.toDouble()
        if (delta < -LANDING_TOLERANCE_MS ||
            delta > audioMediaElapsedMs + LANDING_TOLERANCE_MS
        ) return false
        val previous = lastAudioPositionMs
        if (previous == null) {
            lastAudioPositionMs = positionMs
            return false
        }
        if (positionMs <= previous) return false
        pendingSeek = null
        controlSequenceFloor = null
        lastAudioPositionMs = null
        return true
    }

    /**
     * An in-place track mutation leaves the already-presenting video output
     * attached. Once the exact mutation has executed, no replacement frame
     * generation exists to wait for.
     */
    @Synchronized
    fun presentedInPlace(sequence: Long): Boolean {
        val pending = pendingSeek ?: return false
        if (pending.sequence != sequence || pending.frameFloor == null) return false
        pendingSeek = null
        controlSequenceFloor = null
        lastAudioPositionMs = null
        return true
    }

    @Synchronized
    fun retainControlSequence(sequence: Long?) {
        if (sequence != null && sequence > 0L) {
            controlSequenceFloor = maxOf(controlSequenceFloor ?: 0L, sequence)
        }
    }

    /** Complete a replacement publication only if this exact command still owns it. */
    @Synchronized
    fun retainReplacementSequence(
        pending: PendingSeek,
        quality: PlaybackQuality,
        controlSequence: Long?,
    ): Boolean {
        if (pendingSeek?.sequence != pending.sequence || desiredQuality != quality) return false
        if (controlSequence != null && controlSequence > 0L) {
            controlSequenceFloor = maxOf(controlSequenceFloor ?: 0L, controlSequence)
        }
        return true
    }

    @Synchronized
    fun orderedControlSequence(current: Long?): Long? =
        listOfNotNull(current, controlSequenceFloor).maxOrNull()

    @Synchronized
    fun clear() {
        pendingSeek = null
        controlSequenceFloor = null
        lastAudioPositionMs = null
    }

    companion object {
        const val LANDING_TOLERANCE_MS = 250L
        const val AUDIO_PRESENTATION_DEADLINE_MS = 8_000L
    }
}

/**
 * Publishes one controller-replacement intent, then proves that the command
 * still owns the destination and immutable quality after the suspension.
 */
internal suspend fun publishReplacementIntent(
    intent: PlaybackIntent,
    pending: PlaybackIntent.PendingSeek,
    quality: PlaybackQuality,
    isActive: () -> Boolean = { true },
    publish: suspend () -> Long?,
): Boolean {
    if (!isActive() || !intent.retainReplacementSequence(pending, quality, null)) return false
    val sequence = publish()
    return isActive() && intent.retainReplacementSequence(pending, quality, sequence)
}

/** Fences delayed control work to its session and the owning controller's lifetime. */
internal class PlaybackControlBootstrapFence {
    class Claim internal constructor(val sessionId: String, val generation: Long)

    private var generation = 0L
    private var released = false

    @Synchronized
    fun claim(sessionId: String): Claim = Claim(sessionId, ++generation)

    @Synchronized
    fun snapshot(sessionId: String): Claim = Claim(sessionId, generation)

    @Synchronized
    fun invalidate() {
        generation += 1
    }

    @Synchronized
    fun release() {
        released = true
        generation += 1
    }

    @Synchronized
    fun isActive(): Boolean = !released

    @Synchronized
    fun isCurrent(claim: Claim, currentSessionId: String?): Boolean =
        !released && claim.generation == generation && claim.sessionId == currentSessionId
}

/** A later transport/track command carries an outstanding plan change with it. */
internal class PlaybackPlanReplacement(private val activeQuality: PlaybackQuality) {
    private var reload: ((Long, PlaybackQuality) -> Unit)? = null
    private var publishedGeneration: Long? = null

    fun retain(callback: (Long, PlaybackQuality) -> Unit) {
        reload = callback
    }

    fun route(intent: PlaybackIntent, force: Boolean = false, retry: Boolean = false): Boolean {
        val pending = intent.pendingSeek ?: return false
        val desired = intent.desiredQuality
        if (!force && desired == activeQuality) return false
        val callback = reload ?: return false
        if (retry || publishedGeneration != pending.sequence) {
            publishedGeneration = pending.sequence
            callback(pending.targetMs, desired)
        }
        return true
    }

    fun release() {
        reload = null
    }
}

/**
 * Deadline for a requested destination, independent of the departing clock.
 * One bounded recovery is allowed for each viewer command. Pause/background
 * time is excluded, and stale callbacks cannot reset the successor's clock.
 */
internal class PlaybackTargetDeadline(private val timeoutMs: Long = 8_000) {
    data class Event(val sequence: Long, val targetMs: Long, val terminal: Boolean)

    private var generation: Long? = null
    private var observedAtMs = 0L
    private var elapsedActiveMs = 0L
    private var wasActive = false
    private var recovered = false
    private var fired = false

    fun sample(
        pending: PlaybackIntent.PendingSeek?,
        playbackRequested: Boolean,
        foreground: Boolean,
        nowMs: Long,
    ): Event? {
        if (pending == null) {
            reset()
            return null
        }
        val active = playbackRequested && foreground
        if (generation != pending.sequence) {
            generation = pending.sequence
            observedAtMs = nowMs
            elapsedActiveMs = 0
            wasActive = active
            recovered = false
            fired = false
            return null
        }
        val elapsed = (nowMs - observedAtMs).coerceAtLeast(0)
        observedAtMs = maxOf(observedAtMs, nowMs)
        if (wasActive && !fired) {
            elapsedActiveMs += elapsed.coerceAtMost((timeoutMs - elapsedActiveMs).coerceAtLeast(0))
        }
        wasActive = active
        if (!active || fired || elapsedActiveMs < timeoutMs) return null
        fired = true
        return Event(pending.sequence, pending.targetMs, terminal = recovered)
    }

    fun recover(event: Event, nowMs: Long): Boolean {
        if (generation != event.sequence || !fired || recovered || event.terminal) return false
        recovered = true
        fired = false
        elapsedActiveMs = 0
        observedAtMs = nowMs
        return true
    }

    fun nextSampleDelayMs(nowMs: Long): Long {
        if (generation == null || !wasActive || fired) return 1_000
        val elapsedSinceSample = (nowMs - observedAtMs).coerceAtLeast(0)
        return (timeoutMs - elapsedActiveMs - elapsedSinceSample).coerceIn(1, 1_000)
    }

    fun reset() {
        generation = null
        wasActive = false
        recovered = false
        fired = false
        elapsedActiveMs = 0
    }
}
