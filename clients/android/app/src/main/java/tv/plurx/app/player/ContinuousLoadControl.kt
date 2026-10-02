@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.source.MediaSource

/** Preserve allocator, startup and byte-budget policy while bounding new
 * controlled loads to eight seconds of playout. Existing queues are retained. */
internal class ContinuousLoadControl(
    private val delegate: LoadControl,
    private val controlled: (MediaSource.MediaPeriodId) -> Boolean,
) : LoadControl by delegate {
    override fun shouldContinueLoading(parameters: LoadControl.Parameters): Boolean {
        val withinBudget = delegate.shouldContinueLoading(parameters)
        if (!withinBudget || !controlled(parameters.mediaPeriodId)) return withinBudget
        val speed = parameters.playbackSpeed.toDouble().takeIf { it.isFinite() && it > 0 }?.coerceAtMost(16.0) ?: 1.0
        return parameters.bufferedDurationUs < (8_000_000 * speed).toLong()
    }
}
