@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.Player
import androidx.media3.common.TrackGroup
import androidx.media3.common.TrackSelectionParameters
import androidx.media3.common.Tracks
import androidx.media3.common.MimeTypes
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.lang.reflect.Proxy

@OptIn(ExperimentalCoroutinesApi::class)
class NativeSubtitleRetryTest {
    private class Engine {
        val listeners = mutableListOf<Player.Listener>()
        var parameters = TrackSelectionParameters.DEFAULT_WITHOUT_CONTEXT
        var tracks = Tracks(listOf(Tracks.Group(
            TrackGroup(Format.Builder().setSampleMimeType(MimeTypes.TEXT_VTT).build()),
            false, intArrayOf(C.FORMAT_HANDLED), booleanArrayOf(true),
        )))
        val player = Proxy.newProxyInstance(Player::class.java.classLoader, arrayOf(Player::class.java)) { _, method, args ->
            when (method.name) {
                "addListener" -> { listeners.add(args!![0] as Player.Listener); null }
                "removeListener" -> { listeners.remove(args!![0]); null }
                "getCurrentTracks" -> tracks
                "getTrackSelectionParameters" -> parameters
                "setTrackSelectionParameters" -> { parameters = args!![0] as TrackSelectionParameters; null }
                else -> error("Unexpected player mutation: ${method.name}")
            }
        } as Player
        fun acknowledgeDisabled() {
            tracks = Tracks.EMPTY
            listeners.toList().forEach { it.onTracksChanged(tracks) }
        }
    }

    @Test fun reselectWaitsForPlaybackThreadAcknowledgement() = runTest {
        val engine = Engine()
        var restored = 0
        val job = launch { retryNativeTextRendition(engine.player, { true }) { restored++ } }
        runCurrent()
        assertTrue(engine.parameters.disabledTrackTypes.contains(C.TRACK_TYPE_TEXT))
        assertEquals(0, restored)
        engine.acknowledgeDisabled()
        job.join()
        assertEquals(1, restored)
        assertTrue(engine.listeners.isEmpty())
    }

    @Test fun newerOffIntentCannotBeReenabledByLateAcknowledgement() = runTest {
        val engine = Engine()
        var current = true
        var restored = 0
        val job = launch { retryNativeTextRendition(engine.player, { current }) { restored++ } }
        runCurrent()
        current = false
        engine.acknowledgeDisabled()
        job.join()
        assertEquals(0, restored)
        assertTrue(engine.listeners.isEmpty())
    }

    @Test fun cancellationAndTimeoutDoNotStrandTheCurrentSelectionDisabled() = runTest {
        for (cancel in listOf(true, false)) {
            val engine = Engine()
            var restored = 0
            val job = launch { retryNativeTextRendition(engine.player, { true }) { restored++ } }
            runCurrent()
            if (cancel) job.cancel()
            job.join()
            assertEquals(1, restored)
            assertTrue(engine.listeners.isEmpty())
        }
    }
}
