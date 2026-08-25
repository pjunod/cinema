// Instrumentation, and it is a PR gate: a diff touching the Android client sets
// `android_device` in `validation/ci_scope.py`, which boots an emulator in
// `.github/workflows/ci.yml` and runs `make android-instrumentation-run`. That
// target invokes `am instrument` with no `-e class` filter, so every test in the
// package runs — this one included. Treat the assertions below as blocking.
//
// The panel body is a plain `verticalScroll` container, not a lazy list: every
// child stays composed whether or not it is on screen, but an off-screen child
// is clipped and `assertIsDisplayed` fails on it. What this test is checking is
// which facts the ledger routes to the grid and which to the notes strip, not
// where the viewport happens to end on a given emulator — so grid content, which
// sits at the top of the panel, is asserted as displayed, and everything in the
// notes strip is asserted with `assertExists`.
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

        // Header: outside the scrolling body, so always on screen.
        compose.onNodeWithText("Playback info").assertIsDisplayed()
        compose.onNodeWithText("Direct play · 0:31:04 / 2:03:04").assertIsDisplayed()
        // Standard has no Position row; the header subtitle carries it instead.
        compose.onAllNodesWithText("Position").assertCountEquals(0)
        // `videoHealth` is a note, so it is the last row of the strip: assert it
        // is there, not that the viewport reaches it.
        compose.onNodeWithText("Frames").assertExists()
        compose.onNodeWithText("12,340 rendered · 2 dropped (0.0%) · max streak 1").assertExists()
        // SOURCE has grid rows again, so it heads a card and not just a notes group.
        compose.onAllNodesWithTag(PlaybackSectionHeadTag, useUnmergedTree = true)
            .filterToOne(hasText("SOURCE"))
            .assertExists()
        // The filename is prose, so it is a SOURCE note and lives in the strip.
        compose.onNodeWithText("Example.2160p.mkv").assertExists()
        // The source video line is split: the codec/profile/resolution datum is
        // a grid row, the rest stays in the notes strip. Between them they still
        // spell out every fact `sourceVideo` carried.
        compose.onNodeWithText("HEVC · Main 10 · 3840×2160").assertIsDisplayed()
        compose.onNodeWithText("HDR10 · 10-bit · 48.2 Mbps").assertExists()
        // Three facts, so `splitLedgerValue` leaves the audio line whole in the grid.
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
        // NOW DECODING sends its whole video and audio lines to the strip.
        compose.onNodeWithText("HEVC · 3840×2160 · HDR10 / PQ").assertExists()
        compose.onNodeWithText("English · 7.1 · TrueHD").assertExists()
        // SERVER's only row is a grid row, at the top of the right column.
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
