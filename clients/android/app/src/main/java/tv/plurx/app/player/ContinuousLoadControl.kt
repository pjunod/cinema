@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.Timeline
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.upstream.Allocator
import java.util.IdentityHashMap
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.TrackGroupArray
import androidx.media3.exoplayer.trackselection.ExoTrackSelection

/** Preserve allocator, startup and byte-budget policy while bounding new
 * controlled loads to twelve seconds of playout. Existing queues are retained. */
internal class ContinuousLoadControl(
    private val delegate: LoadControl,
    private val allocations: ContinuousAllocationOwnership? = null,
    private val controlled: (MediaSource.MediaPeriodId, Timeline) -> Boolean,
) : LoadControl {
    // Kotlin delegation does not forward Java interface default methods. Media3's
    // defaults call obsolete overloads that throw, so forward its current API explicitly.
    override fun onPrepared(playerId: PlayerId) = delegate.onPrepared(playerId)
    override fun onStopped(playerId: PlayerId) = delegate.onStopped(playerId)
    override fun onReleased(playerId: PlayerId) = delegate.onReleased(playerId)
    override fun onTracksSelected(
        parameters: LoadControl.Parameters,
        trackGroups: TrackGroupArray,
        trackSelections: Array<out ExoTrackSelection?>,
    ) = delegate.onTracksSelected(parameters, trackGroups, trackSelections)
    override fun getBackBufferDurationUs(playerId: PlayerId): Long = delegate.getBackBufferDurationUs(playerId)
    override fun retainBackBufferFromKeyframe(playerId: PlayerId): Boolean = delegate.retainBackBufferFromKeyframe(playerId)
    override fun shouldStartPlayback(parameters: LoadControl.Parameters): Boolean = delegate.shouldStartPlayback(parameters)
    override fun shouldContinuePreloading(
        playerId: PlayerId,
        timeline: Timeline,
        mediaPeriodId: MediaSource.MediaPeriodId,
        bufferedDurationUs: Long,
    ): Boolean = delegate.shouldContinuePreloading(playerId, timeline, mediaPeriodId, bufferedDurationUs)

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
        if (!withinBudget || !controlled(parameters.mediaPeriodId, parameters.timeline)) return withinBudget
        val speed = parameters.playbackSpeed.toDouble().takeIf { it.isFinite() && it > 0 }?.coerceAtMost(16.0) ?: 1.0
        return parameters.bufferedDurationUs < (12_000_000 * speed).toLong()
    }
}
