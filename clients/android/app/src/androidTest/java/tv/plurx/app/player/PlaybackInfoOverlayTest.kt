// Instrumentation: this suite needs a device or emulator, so CI never runs it
// and it can rot unnoticed. It had drifted six assertions behind the overlay by
// the time the ledger rewrite landed; those were repaired on this branch along
// with the overlay changes, which is why it looks larger than the feature work
// would suggest. Run it by hand (`connectedDebugAndroidTest`) before trusting it.
package tv.plurx.app.player

import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.filterToOne
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.semantics.SemanticsProperties
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Appearance
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
                        delivery = "Direct play",
                        position = "0:31:04 / 2:03:04",
                        buffer = "0:00 ahead · 73%",
                        videoHealth = "12,340 rendered · 2 dropped (0.0%) · max streak 1",
                        sourceFile = "Example.2160p.mkv",
                        sourceVideo = "HEVC · Main 10 · 3840×2160 · HDR10 · 10-bit · 48.2 Mbps",
                        sourceAudio = "TRUEHD · 7.1 · English",
                        playingVideo = "HEVC · 3840×2160 · HDR10 / PQ",
                        playingAudio = "English · 7.1 · TrueHD",
                        subtitles = "Off",
                    ),
                    reasons = listOf("Device supports the source video and audio codecs"),
                    mode = PlaybackStatsMode.Standard,
                    onMode = {},
                    onDismiss = {},
                )
            }
        }

        compose.onNodeWithText("Playback info").assertIsDisplayed()
        compose.onNodeWithText("Direct play · 0:31:04 / 2:03:04").assertIsDisplayed()
        compose.onAllNodesWithText("Position").assertCountEquals(0)
        compose.onNodeWithText("Frames").assertIsDisplayed()
        compose.onNodeWithText("12,340 rendered · 2 dropped (0.0%) · max streak 1").assertIsDisplayed()
        // SOURCE has grid rows again, so it heads a card and not just a notes group.
        compose.onAllNodesWithTag(PlaybackSectionHeadTag, useUnmergedTree = true)
            .filterToOne(hasText("SOURCE"))
            .assertExists()
        compose.onNodeWithText("Example.2160p.mkv").assertIsDisplayed()
        // The source video line is split: the codec/profile/resolution datum is
        // a grid row, the rest stays in the notes strip. Between them they still
        // spell out every fact `sourceVideo` carried.
        compose.onNodeWithText("HEVC · Main 10 · 3840×2160").assertIsDisplayed()
        compose.onNodeWithText("HDR10 · 10-bit · 48.2 Mbps").assertExists()
        compose.onNodeWithText("TRUEHD · 7.1 · English").assertIsDisplayed()
        // Two roles, not two headings: the card head and the notes-group hint.
        // Targeted by role rather than by a count, so a fixture where only one
        // section contributes notes — and the group labels are suppressed —
        // fails on the role that is missing instead of on arithmetic.
        compose.onAllNodesWithTag(PlaybackSectionHeadTag, useUnmergedTree = true)
            .filterToOne(hasText("NOW DECODING"))
            .assertExists()
        compose.onAllNodesWithTag(PlaybackNotesGroupTag, useUnmergedTree = true)
            .filterToOne(hasText("NOW DECODING"))
            .assertExists()
        compose.onNodeWithText("HEVC · 3840×2160 · HDR10 / PQ").assertIsDisplayed()
        compose.onNodeWithText("English · 7.1 · TrueHD").assertIsDisplayed()
        compose.onNodeWithText("No server-side session").assertIsDisplayed()
        val close = compose.onNodeWithContentDescription("Close playback info")
        compose.waitUntil(timeoutMillis = 2_000) {
            close.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        close
            .assertIsDisplayed()
            .assertHasClickAction()
            .assertIsFocused()
    }
}
