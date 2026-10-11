@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.app.ActivityManager
import android.content.Context
import android.content.pm.ApplicationInfo
import android.os.Build
import android.os.Bundle
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.SinglePeriodTimeline
import androidx.media3.exoplayer.upstream.Allocation
import androidx.test.platform.app.InstrumentationRegistry
import coil.imageLoader
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PlaybackLoadControlTest {
    @Test
    fun actualProcessHeapAndCacheBoundHaveExplicitProbeProvenance() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val context = instrumentation.targetContext
        val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager
        val grant = Runtime.getRuntime().maxMemory()
        val cache = context.imageLoader.memoryCache?.maxSize ?: 0
        val info = context.packageManager.getPackageInfo(context.packageName, 0)
        val roles = BufferRole.entries.joinToString(",") { role ->
            "$role:${playbackBufferTargetBytes(manager.memoryClass, grant, role)}"
        }
        assertTrue(grant > 0)
        for (role in BufferRole.entries) {
            assertTrue(playbackBufferTargetBytes(manager.memoryClass, grant, role) <=
                playbackBufferTargetBytes(manager.memoryClass))
        }
        val receipt = "package=${context.packageName} version_code=${info.longVersionCode} " +
            "sdk=${Build.VERSION.SDK_INT} abis=${Build.SUPPORTED_ABIS.joinToString(",")} " +
            "debuggable=${context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0} " +
            "large_heap_requested=${context.applicationInfo.flags and ApplicationInfo.FLAG_LARGE_HEAP != 0} " +
            "memory_class_mb=${manager.memoryClass} large_memory_class_mb=${manager.largeMemoryClass} " +
            "granted_heap_bytes=$grant image_cache_max_bytes=$cache role_targets=$roles " +
            "scope=instrumented_process_not_production_playback"
        instrumentation.sendStatus(0, Bundle().apply { putString("stream", "\n$receipt\n") })
    }

    @Test
    fun byteBudgetStopsLoadingBeforeTimeTargetForNetworkAndLocalMedia() {
        val context = InstrumentationRegistry.getInstrumentation().targetContext
        val heapMb = (context.getSystemService(Context.ACTIVITY_SERVICE) as ActivityManager).memoryClass
        for (uri in listOf("https://example.invalid/movie.mp4", "file:///movie.mp4")) {
            for (role in BufferRole.entries) {
                val live = role == BufferRole.Live
                val target = playbackBufferTargetBytes(heapMb, Runtime.getRuntime().maxMemory(), role)
                val control = playbackLoadControl(context, live, role)
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
