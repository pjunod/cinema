@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.util.Log
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import java.util.concurrent.atomic.AtomicReference

/** Opt-in transport observation against synthetic local media, never the viewer's library. */
class SubtitleFailureRenderingTest {
    @Test
    fun terminalFailedSubtitleResponseKeepsVideoObservable() {
        val base = InstrumentationRegistry.getArguments().getString("subtitleFailureFixture")
        assumeTrue("requires explicit synthetic failure fixture", base != null)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        for (mode in listOf("empty", "failed")) {
            val failure = AtomicReference<PlaybackException?>()
            lateinit var player: ExoPlayer
            instrumentation.runOnMainSync {
                player = ExoPlayer.Builder(instrumentation.targetContext).build()
                player.volume = 0f
                player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                    .setPreferredTextLanguage("en").setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false).build()
                player.addListener(object : Player.Listener {
                    override fun onPlayerError(error: PlaybackException) { failure.set(error) }
                })
                player.setMediaSource(HlsMediaSource.Factory(DefaultHttpDataSource.Factory())
                    .createMediaSource(MediaItem.fromUri("$base/$mode/master.m3u8")))
                player.prepare()
                player.play()
            }
            try {
                Thread.sleep(15000)
                instrumentation.runOnMainSync {
                    val detail = "mode=$mode position=${player.currentPosition} state=${player.playbackState} " +
                        "error=${failure.get()?.errorCodeName} cause=${failure.get()?.cause}"
                    Log.i("SubtitleFailureObservation", detail)
                    println("SUBTITLE_FAILURE_OBSERVATION $detail")
                    assertTrue("video must advance through the caption response: $detail", player.currentPosition > 8000)
                    assertTrue("caption transport must not fail the video: $detail", failure.get() == null)
                }
            } finally {
                instrumentation.runOnMainSync { player.release() }
            }
        }
    }
}
