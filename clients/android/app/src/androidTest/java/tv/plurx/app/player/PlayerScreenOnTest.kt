@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.os.SystemClock
import androidx.media3.common.AudioAttributes
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.ui.PlayerView
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/** Actual PlayerView and ExoPlayer callbacks through the production M3 binder.
 * Synthetic silent PCM is generated in memory; no server, account or asset is
 * read. The video-plan flag is supplied explicitly, as in PlayerScreen. */
class PlayerScreenOnTest {
    @Test fun actualPlayerCallbacksHoldVideoScreenDuringBufferingAndPlayButNotPause() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val releaseLoad = CountDownLatch(1)
        val loading = CountDownLatch(1)
        lateinit var player: ExoPlayer
        lateinit var view: PlayerView
        lateinit var binder: PlayerScreenOn
        var madePlayer = false
        var madeView = false
        var madeBinder = false
        val bytes = silentWave()
        try {
            instrumentation.runOnMainSync {
                val context = instrumentation.targetContext
                player = ExoPlayer.Builder(context).build().apply {
                    volume = 0f
                    setAudioAttributes(AudioAttributes.DEFAULT, false)
                }
                madePlayer = true
                view = PlayerView(context).apply { this.player = player; useController = false }
                madeView = true
                binder = PlayerScreenOn(view = { view }, player = { player }, isVideo = true)
                madeBinder = true
                player.addListener(binder)
                binder.sync()
                val source = ProgressiveMediaSource.Factory(DataSource.Factory {
                    val data = ByteArrayDataSource(bytes)
                    object : DataSource by data {
                        override fun open(dataSpec: DataSpec): Long {
                            loading.countDown()
                            if (!releaseLoad.await(10, TimeUnit.SECONDS)) throw IOException("Synthetic fixture gate timed out")
                            return data.open(dataSpec)
                        }
                    }
                }).createMediaSource(MediaItem.fromUri("memory://plurx-screen-test/fixture.wav"))
                player.setMediaSource(source)
                player.prepare()
                player.play()
            }
            assertTrue("Synthetic local load reached buffering", loading.await(5, TimeUnit.SECONDS))
            awaitMainCondition("Requested buffering holds the video screen") {
                player.playbackState == Player.STATE_BUFFERING && view.keepScreenOn
            }
            instrumentation.runOnMainSync { player.pause() }
            awaitMainCondition("Viewer pause releases the screen during buffering") { !view.keepScreenOn }
            instrumentation.runOnMainSync { player.play() }
            awaitMainCondition("Resume during buffering holds the screen") { view.keepScreenOn }
            releaseLoad.countDown()
            awaitMainCondition("Actual prepared playback holds the screen") { player.isPlaying && view.keepScreenOn }
            instrumentation.runOnMainSync { player.pause() }
            awaitMainCondition("Actual pause releases the screen") { !player.isPlaying && !view.keepScreenOn }
            instrumentation.runOnMainSync { player.play() }
            awaitMainCondition("Actual resume holds the screen") { player.isPlaying && view.keepScreenOn }
        } finally {
            releaseLoad.countDown()
            instrumentation.runOnMainSync {
                if (madePlayer) {
                    if (madeBinder) player.removeListener(binder)
                    if (madeView) view.player = null
                    player.release()
                }
            }
        }
    }

    private fun awaitMainCondition(message: String, condition: () -> Boolean) {
        val deadline = SystemClock.elapsedRealtime() + 5_000L
        var observed = false
        do {
            InstrumentationRegistry.getInstrumentation().runOnMainSync { observed = condition() }
            if (observed) break
            SystemClock.sleep(20)
        } while (SystemClock.elapsedRealtime() < deadline)
        assertTrue(message, observed)
    }

    private fun silentWave(): ByteArray {
        val dataBytes = 8_000 * 2 * 30
        return ByteBuffer.allocate(44 + dataBytes).order(ByteOrder.LITTLE_ENDIAN)
            .put("RIFF".toByteArray(Charsets.US_ASCII)).putInt(36 + dataBytes)
            .put("WAVEfmt ".toByteArray(Charsets.US_ASCII)).putInt(16)
            .putShort(1).putShort(1).putInt(8_000).putInt(16_000).putShort(2).putShort(16)
            .put("data".toByteArray(Charsets.US_ASCII)).putInt(dataBytes).array()
    }
}
