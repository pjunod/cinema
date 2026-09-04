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
    data class PendingSeek(val sequence: Long, val targetMs: Long, val originMs: Long)

    var quality: PlaybackQuality = initialQuality
        private set
    var pendingSeek: PendingSeek? = null
        private set
    private var nextSequence = 0L

    /** Adopt the quality carried by the exact decision that this controller opens. */
    fun adoptQuality(quality: PlaybackQuality) {
        this.quality = quality
    }

    fun beginSeek(targetMs: Long, originMs: Long, quality: PlaybackQuality = this.quality): PendingSeek {
        this.quality = quality
        return PendingSeek(++nextSequence, targetMs.coerceAtLeast(0), originMs.coerceAtLeast(0))
            .also { pendingSeek = it }
    }

    /** A later seek supersedes the old one; only the current target may clear. */
    fun presented(positionMs: Long, sequence: Long? = pendingSeek?.sequence): Boolean {
        val pending = pendingSeek ?: return false
        if (sequence != pending.sequence || abs(positionMs - pending.targetMs) > LANDING_TOLERANCE_MS) {
            return false
        }
        pendingSeek = null
        return true
    }

    fun clear() { pendingSeek = null }

    companion object {
        const val LANDING_TOLERANCE_MS = 2_500L
    }
}
