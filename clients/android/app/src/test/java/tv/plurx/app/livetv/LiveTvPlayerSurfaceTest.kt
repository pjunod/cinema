package tv.plurx.app.livetv

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The fullscreen picture on tablets stayed at the inline box's size because
 * one PlayerView — and the SurfaceView inside it — was moved between host
 * boxes with `movableContentOf`. These pin the replacement: every host builds
 * its own PlayerView, and a view leaving composition unbinds the player.
 */
class LiveTvPlayerSurfaceTest {
    private fun screenSource(): String = listOf(
        File("app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt"),
        File("src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt"),
        File("clients/android/app/src/main/java/tv/plurx/app/livetv/LiveTvScreen.kt"),
    ).firstOrNull(File::isFile)?.readText() ?: error("LiveTvScreen.kt source not found")

    @Test
    fun everyHostBoxBuildsItsOwnPlayerView() {
        val screen = screenSource()
        assertFalse(
            "the picture must not be moved between host boxes as movable content",
            Regex("""movableContentOf\s*[({]""").containsMatchIn(screen),
        )
        assertFalse(
            "the forced-relayout and surface-rebind workaround must not return",
            screen.contains("relayoutSubtree") || screen.contains("rebindVideoSurface"),
        )
        val surface = screen.substringAfter("private fun LiveTvPlayerSurface(")
            .substringBefore("internal data class LiveTvPictureInfo(")
        assertTrue("each host creates its PlayerView", surface.contains("PlayerView(context)"))
        assertEquals(
            "the screen builds its PlayerView in exactly one place, the per-host composable",
            1, Regex("""PlayerView\(context\)""").findAll(screen).count(),
        )
        assertEquals(
            "the screen hosts exactly one AndroidView, the per-host PlayerView",
            1, Regex("""\bAndroidView\(""").findAll(screen).count(),
        )
        assertTrue(
            "every recomposition rebinds the current player, so a retune's new ExoPlayer reaches the view",
            Regex("""update\s*=\s*\{\s*view\s*->\s*view\.player\s*=\s*controller\.player""").containsMatchIn(surface),
        )
        assertTrue(
            "the in-place resize path keeps the API 34 SurfaceView sync workaround on",
            surface.contains("setEnableComposeSurfaceSyncWorkaround(true)"),
        )
        assertTrue(
            "a host leaving composition must unbind the shared player",
            Regex("""onRelease\s*=\s*\{\s*view\s*->\s*view\.player\s*=\s*null\s*}""").containsMatchIn(surface),
        )
    }
}
