@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.graphics.Rect
import android.os.Build
import androidx.compose.foundation.layout.size
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.media3.common.MediaItem
import androidx.media3.common.Player
import androidx.media3.datasource.ByteArrayDataSource
import androidx.media3.datasource.DataSource
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.ProgressiveMediaSource
import androidx.media3.ui.PlayerView
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Rule
import org.junit.Test

/** The generated green AVC fixture exercises the platform's real buffer mapping,
 * including the BLAST child created by Surface(SurfaceControl). Arithmetic-only
 * assertions cannot tell which coordinate space that platform surface uses. */
class PreparedVideoSurfaceRenderingTest {
    @get:Rule val compose = createComposeRule()

    @Test fun decoderFrameFillsThePreparedSurfaceWidth() {
        assumeTrue(Build.VERSION.SDK_INT >= 35)
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val bytes = instrumentation.context.assets.open("prepared-surface-green.mp4").use { it.readBytes() }
        lateinit var player: ExoPlayer
        lateinit var view: PlayerView
        lateinit var surfaces: PreparedVideoSurfaces
        compose.setContent {
            AndroidView(modifier = Modifier.size(300.dp, 220.dp), factory = { context ->
                player = ExoPlayer.Builder(context).build()
                surfaces = checkNotNull(PreparedVideoSurfaces.create(true))
                view = PlayerView(context).apply {
                    useController = false
                    keepScreenOn = true
                    this.player = surfaces.presenter(player)
                }
                player.setMediaSource(ProgressiveMediaSource.Factory(
                    DataSource.Factory { ByteArrayDataSource(bytes) },
                ).createMediaSource(MediaItem.fromUri("memory://prepared-surface-green.mp4")))
                player.repeatMode = Player.REPEAT_MODE_ALL
                player.prepare()
                player.play()
                view
            })
        }
        try {
            val bounds = Rect()
            compose.waitUntil(timeoutMillis = 15_000) {
                var decoded = false
                compose.runOnIdle {
                    decoded = player.videoSize.width == 1920 &&
                        view.videoSurfaceView?.getGlobalVisibleRect(bounds) == true
                }
                decoded && greenAt(bounds, 0.1f)
            }
            assertTrue("decoded picture reaches the right side without a second scale", greenAt(bounds, 0.9f))
        } finally {
            compose.runOnIdle {
                view.player = null
                surfaces.release()
                player.release()
            }
        }
    }

    private fun greenAt(bounds: Rect, horizontal: Float): Boolean {
        val screenshot = InstrumentationRegistry.getInstrumentation().uiAutomation.takeScreenshot() ?: return false
        try {
            val x = bounds.left + (bounds.width() * horizontal).toInt()
            val y = bounds.centerY()
            val color = screenshot.getPixel(x, y)
            return android.graphics.Color.green(color) > 150 &&
                android.graphics.Color.red(color) < 80 && android.graphics.Color.blue(color) < 80
        } finally {
            screenshot.recycle()
        }
    }
}
