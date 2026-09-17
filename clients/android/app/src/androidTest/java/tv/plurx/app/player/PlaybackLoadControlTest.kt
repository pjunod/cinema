@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.app.ActivityManager
import android.content.Context
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.SinglePeriodTimeline
import androidx.media3.exoplayer.upstream.Allocation
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PlaybackLoadControlTest {
    @Test
    fun byteBudgetStopsLoadingBeforeTimeTargetForNetworkAndLocalMedia() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val heapMb = (context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager).memoryClass
        val target = playbackBufferTargetBytes(heapMb)
        for (uri in listOf("https://example.invalid/movie.mp4", "file:///movie.mp4")) {
            for (live in listOf(false, true)) {
                val control = playbackLoadControl(context, live)
                val id = PlayerId("memory-budget-test")
                val timeline = SinglePeriodTimeline(
                    120_000_000L, true, false, false, null, MediaItem.fromUri(uri),
                )
                val params = LoadControl.Parameters(
                    id, timeline, MediaSource.MediaPeriodId(timeline.getUidOfPeriod(0)),
                    0L, 750_000L, 1f, true, false, C.TIME_UNSET, C.TIME_UNSET,
                )
                control.onPrepared(id)
                val allocator = control.getAllocator(id)
                val allocations = mutableListOf<Allocation>()
                try {
                    assertTrue(control.shouldContinueLoading(params))
                    assertFalse(control.shouldStartPlayback(params))
                    while (allocator.totalBytesAllocated < target) allocations += allocator.allocate()
                    assertFalse("Byte budget must win over time: $uri live=$live", control.shouldContinueLoading(params))
                    assertTrue("A full buffer must not wait forever for its time target", control.shouldStartPlayback(params))
                } finally {
                    allocations.forEach(allocator::release)
                    control.onReleased(id)
                }
            }
        }
    }
}
