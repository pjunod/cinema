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
    var pendingSeek: PendingSeek? = null
        private set
    var controlSequenceFloor: Long? = null
        private set
    private var nextSequence = 0L
    private var presentedFrames = 0L
    private var lastAudioPositionMs: Long? = null

    /** Adopt the quality carried by the exact decision that a successor opens. */
    @Synchronized
    fun adoptQuality(quality: PlaybackQuality) {
        desiredQuality = quality
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
    fun markExecuted(sequence: Long): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence) return false
        lastAudioPositionMs = null
        pendingSeek = pending.copy(frameFloor = presentedFrames)
        return true
    }

    @Synchronized
    fun isCurrent(sequence: Long): Boolean = pendingSeek?.sequence == sequence

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
    fun presentedAudio(positionMs: Long, sequence: Long? = pendingSeek?.sequence): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence || pending.frameFloor == null) {
            return false
        }
        val previous = lastAudioPositionMs
        if (previous == null) {
            if (abs(positionMs - pending.targetMs) > LANDING_TOLERANCE_MS) return false
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
    }
}
