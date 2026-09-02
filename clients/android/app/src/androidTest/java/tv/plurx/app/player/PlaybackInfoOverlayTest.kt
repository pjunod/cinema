package tv.plurx.app.player

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Appearance
import tv.plurx.app.data.PlaybackSessionStatus
import tv.plurx.app.data.ThemeId
import tv.plurx.app.ui.theme.PlurxTheme

class PlaybackInfoOverlayTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun lightThemeStillShowsStructuredPlaybackDetails() {
        compose.setContent {
            PlurxTheme(ThemeId.Classic, Appearance.Light) {
                PlaybackInfoOverlay(
                    details = PlaybackInfoDetails(
                        title = "Example feature film",
                        fileId = 42,
                        delivery = "Transcode · nvenc · 1080p",
                        position = "0:31:04 / 2:03:04",
                        buffer = "12.3 s",
                        frames = "2 / 12,342 frames",
                        sourceFile = "Example.2160p.mkv",
                        sourceVideo = "HEVC · Main 10 · 10-bit · HDR10",
                        sourceResolution = "3840×2160",
                        sourceBitrate = "48 Mb/s",
                        container = "MKV",
                        sourceAudio = "TRUEHD · 7.1 · English",
                        decodeResolution = "1920×1080",
                        playingAudio = "AAC · Stereo · English",
                        dynamicRange = "HDR10",
                        subtitles = "English · native",
                        stalls = "2 (1 supply · 1 decode)",
                        encoder = "nvenc",
                        control = "node-a · active",
                        sessionStatus = PlaybackSessionStatus(
                            id = "session-42",
                            encoder = "nvenc",
                            recent_speed = 1.25,
                            ahead_seconds = 12,
                            delivered_bytes = 86_000_000,
                            delivered_bps = 12_300_000,
                        ),
                    ),
                    reasons = listOf("Source exceeds the selected output rung"),
                    mode = PlaybackStatsMode.Standard,
                    onMode = {},
                    onDismiss = {},
                )
            }
        }

        compose.onNodeWithText("Playback info").assertIsDisplayed()
        compose.onNodeWithText("Method").assertIsDisplayed()
        compose.onNodeWithText("Position").assertIsDisplayed()
        compose.onAllNodesWithText("Resolution").assertCountEquals(2)
        compose.onNodeWithText("Buffer").assertExists()
        compose.onNodeWithText("Stalls").assertExists()
        compose.onNodeWithText("Delivery rate").assertExists()
        compose.onNodeWithText("Delivered").assertExists()
        compose.onNodeWithText("Encode speed").assertExists()
        compose.onNodeWithText("Server ahead").assertExists()
        compose.onNodeWithText("Control").assertExists()

        val close = compose.onNodeWithContentDescription("Close playback info")
        compose.waitUntil(timeoutMillis = 2_000) {
            close.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        close.assertHasClickAction().assertIsFocused()
    }
}
