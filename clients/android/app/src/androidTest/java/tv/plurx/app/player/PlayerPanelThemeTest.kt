package tv.plurx.app.player

import androidx.compose.ui.graphics.luminance
import androidx.compose.ui.graphics.toPixelMap
import androidx.compose.ui.test.captureToImage
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Appearance
import tv.plurx.app.data.ThemeId
import tv.plurx.app.ui.theme.PlurxTheme

class PlayerPanelThemeTest {
    @get:Rule val compose = createComposeRule()

    @Test fun lightPlaybackPanelHasVisibleUnselectedRowsAndSwitchLabels() {
        compose.setContent {
            PlurxTheme(ThemeId.Classic, Appearance.Light) {
                PlayerPanelSurface("Playback settings", {}) {
                    PanelRow("1080p", false) {}
                    PanelSwitch("Autoplay next episode", true) {}
                }
            }
        }
        for (label in listOf("    1080p", "Autoplay next episode")) {
            val pixels = compose.onNodeWithText(label, useUnmergedTree = true)
                .captureToImage().toPixelMap()
            var darkPixels = 0
            for (y in 0 until pixels.height) for (x in 0 until pixels.width) {
                if (pixels[x, y].luminance() < 0.2f) darkPixels++
            }
            assertTrue("$label must have contrasting ink on its light panel", darkPixels > 20)
        }
    }
}
