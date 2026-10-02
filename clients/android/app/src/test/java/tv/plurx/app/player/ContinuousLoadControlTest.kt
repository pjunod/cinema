@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Timeline
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.upstream.DefaultAllocator
import org.junit.Assert.*
import org.junit.Test

class ContinuousLoadControlTest {
    @Test fun onlyOwnedPeriodsBoundFutureLoadingAndTheByteBudgetStillWins() {
        val owned = MediaSource.MediaPeriodId(Any())
        val ordinary = MediaSource.MediaPeriodId(Any())
        var byteBudget = true
        val allocator = DefaultAllocator(true, 64 * 1024)
        val delegate = object : LoadControl {
            override fun getAllocator(playerId: PlayerId) = allocator
            override fun shouldContinueLoading(parameters: LoadControl.Parameters) = byteBudget
        }
        val control = ContinuousLoadControl(delegate) { it == owned }
        fun request(id: MediaSource.MediaPeriodId, duration: Long, speed: Float = 1f) = LoadControl.Parameters(
            PlayerId.UNSET, Timeline.EMPTY, id, 0, duration, speed, true, false, C.TIME_UNSET, C.TIME_UNSET)
        assertTrue(control.shouldContinueLoading(request(owned, 11_999_999)))
        assertFalse(control.shouldContinueLoading(request(owned, 12_000_000)))
        assertTrue(control.shouldContinueLoading(request(ordinary, 50_000_000)))
        assertTrue(control.shouldContinueLoading(request(owned, 23_999_999, 2f)))
        assertFalse(control.shouldContinueLoading(request(owned, 24_000_000, 2f)))
        assertFalse(control.shouldContinueLoading(request(owned, 12_000_000, Float.NaN)))
        byteBudget = false
        assertFalse(control.shouldContinueLoading(request(owned, 0)))
        assertFalse(control.shouldContinueLoading(request(ordinary, 0)))
        assertSame(allocator, control.getAllocator(PlayerId.UNSET))
    }
}
