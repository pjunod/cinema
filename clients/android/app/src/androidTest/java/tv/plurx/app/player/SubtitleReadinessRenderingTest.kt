@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.net.Uri
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.common.PlaybackException
import androidx.media3.common.text.CueGroup
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.datasource.TransferListener
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.hls.HlsMediaSource
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicReference
import java.util.concurrent.atomic.AtomicInteger

/** An explicitly supplied synthetic HLS fixture; never uses a user's server or library. */
class SubtitleReadinessRenderingTest {
    @Test
    fun firstReadyAfterBufferedEmptySegmentsRestoresRealCues() {
        val base = InstrumentationRegistry.getArguments().getString("subtitleFixture")
        assumeTrue("requires the local synthetic subtitle fixture", base != null)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val published = AtomicBoolean(false)
        val emptyRead = CountDownLatch(1)
        val restoredCue = CountDownLatch(1)
        val failure = AtomicReference<PlaybackException?>()
        val readyReads = AtomicInteger()
        val emptyReads = AtomicInteger()
        val factory = DataSource.Factory {
            object : DataSource {
                private var delegate: DataSource? = null
                override fun addTransferListener(listener: TransferListener) = Unit
                override fun getUri(): Uri? = delegate?.uri
                override fun open(spec: DataSpec): Long {
                    delegate = if (spec.uri.path.orEmpty().endsWith(".vtt")) {
                        val text = if (published.get()) {
                            readyReads.incrementAndGet()
                            "WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00:00.000,MPEGTS:0\n\n00:00:00.000 --> 00:00:30.000\nrestored-caption\n\n"
                        } else { emptyReads.incrementAndGet(); emptyRead.countDown(); "WEBVTT\n\n" }
                        ByteArrayDataSource(text.toByteArray())
                    } else DefaultHttpDataSource.Factory().createDataSource()
                    return delegate!!.open(spec)
                }
                override fun read(buffer: ByteArray, offset: Int, length: Int): Int = delegate!!.read(buffer, offset, length)
                override fun close() { delegate?.close() }
            }
        }
        lateinit var player: ExoPlayer
        instrumentation.runOnMainSync {
            player = ExoPlayer.Builder(instrumentation.targetContext).build()
            player.volume = 0f
            player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                .setPreferredTextLanguage("en").setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false).build()
            player.addListener(object : Player.Listener {
                override fun onCues(cueGroup: CueGroup) {
                    if (cueGroup.cues.any { it.text.toString().contains("restored-caption") }) restoredCue.countDown()
                }
                override fun onPlayerError(error: PlaybackException) { failure.set(error) }
            })
            player.setMediaSource(HlsMediaSource.Factory(factory).createMediaSource(MediaItem.fromUri("$base/master.m3u8")))
            player.prepare()
            player.play()
        }
        try {
            assertTrue("real Media3 must consume an empty subtitle segment", emptyRead.await(15, TimeUnit.SECONDS))
            Thread.sleep(1000)
            published.set(true)
            val readiness = SubtitleReadinessRetryState()
            assertTrue("first ready must not require a prior warming exchange", readiness.record("ready"))
            runBlocking {
                withContext(Dispatchers.Main) {
                    retryNativeTextRendition(player, isCurrent = { true }) {
                        player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                            .setPreferredTextLanguage("en").setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false).build()
                    }
                }
            }
            val restored = restoredCue.await(15, TimeUnit.SECONDS)
            var state = ""
            instrumentation.runOnMainSync {
                state = "position=${player.currentPosition} state=${player.playbackState} text=${player.currentTracks.isTypeSelected(C.TRACK_TYPE_TEXT)}"
            }
            assertTrue("Media3 must refetch and render; empty=${emptyReads.get()} ready=${readyReads.get()} $state error=${failure.get()}", restored)
            instrumentation.runOnMainSync {
                assertTrue("the video media item remains attached", player.mediaItemCount == 1)
                assertTrue("subtitle recovery must not fail video", failure.get() == null)
            }
        } finally {
            instrumentation.runOnMainSync { player.release() }
        }
    }
}
