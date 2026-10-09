package tv.plurx.app.player

import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.SemanticsNodeInteraction
import android.content.res.Configuration
import androidx.test.platform.app.InstrumentationRegistry
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
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
                        clientLoadedSeconds = 12.3,
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
                            production_ahead_seconds = 12,
                            delivered_bytes = 86_000_000,
                            delivered_bps = 12_300_000,
                        ),
                    ),
                    reasons = listOf("Source exceeds the selected output rung"),
                    mode = PlaybackStatsMode.Details,
                    onMode = {},
                    onDismiss = {},
                )
            }
        }

        compose.onNodeWithText("Playback info").assertIsDisplayed()
        val close = compose.onNodeWithText("Close")
        compose.waitUntil(timeoutMillis = 2_000) {
            close.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        close.assertHasClickAction().assertIsFocused()

        // Details owns the labeled observations; Standard is an overview.
        val television = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration.uiMode and Configuration.UI_MODE_TYPE_MASK == Configuration.UI_MODE_TYPE_TELEVISION
        fun reveal(node: SemanticsNodeInteraction): SemanticsNodeInteraction {
            if (television) node.performSemanticsAction(SemanticsActions.RequestFocus) { it() }
            else node.performScrollTo()
            return node.assertIsDisplayed()
        }
        fun detail(label: String) { reveal(compose.onNodeWithText(label)) }
        detail("Source frame")
        detail("Stream frame")
        detail("Player display size")
        detail("Buffering interruptions")
        reveal(compose.onNodeWithText("Buffer & delivery  +")).performClick()
        detail("Buffered on device")
        detail("Server response rate")
        detail("Server responses completed")
        reveal(compose.onNodeWithText("Server work  +")).performClick()
        detail("Encode speed")
        detail("Production actual")
        detail("Control")
        reveal(compose.onNodeWithText("Session & history  +")).performClick()
        detail("Method")
        detail("Position")
        close.assertHasClickAction()
    }
}
