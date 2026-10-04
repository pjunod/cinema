@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Timeline
import androidx.media3.exoplayer.LoadControl
import androidx.media3.exoplayer.analytics.PlayerId
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.TrackGroupArray
import androidx.media3.exoplayer.trackselection.ExoTrackSelection
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
    @Test fun modernPlayerLifecycleAndStartupKeepTheDelegatePolicy() {
        val playerId = PlayerId.UNSET
        val period = MediaSource.MediaPeriodId(Any())
        val parameters = LoadControl.Parameters(playerId, Timeline.EMPTY, period,
            0, 1234, 1f, true, false, C.TIME_UNSET, C.TIME_UNSET)
        val events = mutableListOf<String>()
        var ready = false
        val allocator = DefaultAllocator(true, 64 * 1024)
        val delegate = object : LoadControl {
            override fun getAllocator(playerId: PlayerId) = allocator
            override fun onPrepared(playerId: PlayerId) { events += "prepared" }
            override fun onTracksSelected(parameters: LoadControl.Parameters,
                trackGroups: TrackGroupArray, trackSelections: Array<out ExoTrackSelection?>) {
                assertSame(Timeline.EMPTY, parameters.timeline)
                assertSame(period, parameters.mediaPeriodId)
                assertSame(TrackGroupArray.EMPTY, trackGroups)
                assertEquals(0, trackSelections.size)
                events += "tracks"
            }
            override fun getBackBufferDurationUs(playerId: PlayerId) = 7_000_000L
            override fun retainBackBufferFromKeyframe(playerId: PlayerId) = true
            override fun shouldStartPlayback(parameters: LoadControl.Parameters): Boolean {
                assertSame(period, parameters.mediaPeriodId)
                return ready
            }
            override fun shouldContinuePreloading(playerId: PlayerId, timeline: Timeline,
                mediaPeriodId: MediaSource.MediaPeriodId, bufferedDurationUs: Long): Boolean {
                assertSame(period, mediaPeriodId)
                assertEquals(1234L, bufferedDurationUs)
                return ready
            }
            override fun onStopped(playerId: PlayerId) { events += "stopped" }
            override fun onReleased(playerId: PlayerId) { events += "released" }
        }
        val control = ContinuousLoadControl(delegate) { true }
        assertEquals(7_000_000L, control.getBackBufferDurationUs(playerId))
        assertTrue(control.retainBackBufferFromKeyframe(playerId))
        control.onPrepared(playerId)
        control.onTracksSelected(parameters, TrackGroupArray.EMPTY, emptyArray())
        assertFalse(control.shouldStartPlayback(parameters))
        assertFalse(control.shouldContinuePreloading(playerId, Timeline.EMPTY, period, 1234))
        ready = true
        assertTrue(control.shouldStartPlayback(parameters))
        assertTrue(control.shouldContinuePreloading(playerId, Timeline.EMPTY, period, 1234))
        control.onStopped(playerId)
        control.onReleased(playerId)
        assertEquals(listOf("prepared", "tracks", "stopped", "released"), events)
    }

}
