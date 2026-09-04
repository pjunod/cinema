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
        return PendingSeek(
            ++nextSequence,
            targetMs.coerceAtLeast(0),
            originMs.coerceAtLeast(0),
            null,
        )
            .also { pendingSeek = it }
    }

    /** The media mutation for this generation has landed; departing frames do not count. */
    @Synchronized
    fun markExecuted(sequence: Long): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence) return false
        pendingSeek = pending.copy(frameFloor = presentedFrames)
        return true
    }

    @Synchronized
    fun isCurrent(sequence: Long): Boolean = pendingSeek?.sequence == sequence

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
        return true
    }

    /** Audio-only equivalent: a moving active audio clock is presentation. */
    @Synchronized
    fun presentedAudio(positionMs: Long, sequence: Long? = pendingSeek?.sequence): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence || abs(positionMs - pending.targetMs) > LANDING_TOLERANCE_MS) {
            return false
        }
        pendingSeek = null
        controlSequenceFloor = null
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
    }

    companion object {
        const val LANDING_TOLERANCE_MS = 250L
    }
}
