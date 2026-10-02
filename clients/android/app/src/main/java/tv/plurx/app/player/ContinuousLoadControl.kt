@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.upstream.Allocator
import java.util.IdentityHashMap
import androidx.media3.exoplayer.source.MediaSource

/** Preserve allocator, startup and byte-budget policy while bounding new
 * controlled loads to twelve seconds of playout. Existing queues are retained. */
internal class ContinuousLoadControl(
    private val delegate: LoadControl,
    private val allocations: ContinuousAllocationOwnership? = null,
    private val controlled: (MediaSource.MediaPeriodId) -> Boolean,
) : LoadControl by delegate {
    private val allocators = IdentityHashMap<Allocator, ContinuousAllocator>()
    @Synchronized override fun getAllocator(playerId: PlayerId): Allocator {
        val original = delegate.getAllocator(playerId)
        val ownership = allocations ?: return original
        return allocators.getOrPut(original) {
            check(allocators.size < 8) { "Continuous allocator identity bound" }
            ContinuousAllocator(original, ownership)
        }
    }
    override fun shouldContinueLoading(parameters: LoadControl.Parameters): Boolean {
        val withinBudget = delegate.shouldContinueLoading(parameters)
        if (!withinBudget || !controlled(parameters.mediaPeriodId)) return withinBudget
        val speed = parameters.playbackSpeed.toDouble().takeIf { it.isFinite() && it > 0 }?.coerceAtMost(16.0) ?: 1.0
        return parameters.bufferedDurationUs < (12_000_000 * speed).toLong()
    }
}
