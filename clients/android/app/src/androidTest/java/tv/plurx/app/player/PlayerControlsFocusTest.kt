package tv.plurx.app.player

import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.assertIsNotFocused
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.SemanticsProperties
import org.junit.Rule
import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.ui.theme.PlurxTheme

class PlayerControlsFocusTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun bothSkipDistancesRemainReachableBesidePlayPause() {
        val seeks = mutableListOf<Int>()
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 60_000,
                    durationMs = 120_000,
                    isPlaying = true,
                    requestInitialFocus = false,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = { seeks.add(-30) },
                    onSeekBack = { seeks.add(-10) },
                    onSeekForward = { seeks.add(10) },
                    onSeekForward30 = { seeks.add(30) },
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }
        for (label in listOf("Back 30 seconds", "Back 10 seconds", "Forward 10 seconds", "Forward 30 seconds")) {
            compose.onNodeWithContentDescription(label).performClick()
        }
        assertEquals(listOf(-30, -10, 10, 30), seeks)
        val back30 = compose.onNodeWithContentDescription("Back 30 seconds")
        back30.performSemanticsAction(SemanticsActions.RequestFocus)
        back30.performKeyInput { pressKey(Key.DirectionRight) }
        compose.onNodeWithContentDescription("Back 10 seconds").assertIsFocused()
        compose.onNodeWithContentDescription("Forward 10 seconds")
            .performSemanticsAction(SemanticsActions.RequestFocus)
        compose.onNodeWithContentDescription("Forward 10 seconds")
            .performKeyInput { pressKey(Key.DirectionRight) }
        compose.onNodeWithContentDescription("Forward 30 seconds").assertIsFocused()
        assertEquals(listOf(-30, -10, 10, 30), seeks)
    }

    @Test
    fun playPauseReceivesFocusWhenControlsOpen() {
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 30_000,
                    durationMs = 60_000,
                    isPlaying = true,
                    requestInitialFocus = true,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }

        val pause = compose.onNodeWithContentDescription("Pause")
        compose.waitUntil(timeoutMillis = 2_000) {
            pause.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        pause.assertIsFocused()
    }

    @Test
    fun verticalDpadLeavesTheProgressSliderWithoutSeeking() {
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 30_000,
                    durationMs = 60_000,
                    isPlaying = true,
                    requestInitialFocus = false,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }

        val slider = compose.onNodeWithContentDescription("Playback position")
        val pause = compose.onNodeWithContentDescription("Pause")

        slider.performSemanticsAction(SemanticsActions.RequestFocus)
        slider.assertIsFocused()
        slider.performKeyInput { pressKey(Key.DirectionUp) }
        slider.assertIsFocused()

        slider.performSemanticsAction(SemanticsActions.RequestFocus)
        slider.assertIsFocused()
        slider.performKeyInput { pressKey(Key.DirectionDown) }
        pause.assertIsFocused()

        pause.performKeyInput { pressKey(Key.DirectionUp) }
        slider.assertIsFocused()
    }

    @Test
    fun revealRestoresTheControlThatWasFocusedRatherThanPlayPause() {
        // `reveal` and `close_menu` re-enter Controls, which asks for its own
        // initial focus a frame later: a request made outside was overwritten
        // 80 ms after it landed, so every reveal came back on Play/Pause.
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 30_000,
                    durationMs = 60_000,
                    isPlaying = true,
                    requestInitialFocus = true,
                    initialFocus = PlayerControlId.Info,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }

        val info = compose.onNodeWithContentDescription("Playback info")
        compose.waitUntil(timeoutMillis = 2_000) {
            info.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        compose.waitForIdle()
        info.assertIsFocused()
        compose.onNodeWithContentDescription("Pause").assertIsNotFocused()
    }

    @Test
    fun anUnknownDurationLeavesTheTransportRowWithNoTimelineAbove() {
        // The timeline row is composed only when a duration is known, so
        // pointing `up` at its requester aimed focus at a node that was never
        // attached — an uninitialised-FocusRequester crash on the first Up.
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Live channel",
                    positionMs = 30_000,
                    durationMs = 0,
                    isPlaying = true,
                    requestInitialFocus = true,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }

        val pause = compose.onNodeWithContentDescription("Pause")
        compose.waitUntil(timeoutMillis = 2_000) {
            pause.fetchSemanticsNode().config.getOrElse(SemanticsProperties.Focused) { false }
        }
        pause.performKeyInput { pressKey(Key.DirectionUp) }
        compose.waitForIdle()
        pause.assertIsFocused()
    }

    @Test
    fun shippedRootAdapterPreviewsWithoutSeekingAndCommitsOnce() {
        var state = PlayerInputState.Timeline
        var previews = 0
        var commits = 0
        var reveals = 0
        var playPauses = 0
        compose.setContent {
            PlurxTheme {
                Box(
                    Modifier
                        .fillMaxSize()
                        .testTag("player-adapter-root")
                        .focusable()
                        .playerInputAdapter(state = { state }) { outcome, _, _ ->
                            when (outcome) {
                                PlayerInputOutcome.Preview -> {
                                    previews += 1
                                    state = PlayerInputState.Scrub
                                }
                                PlayerInputOutcome.Commit -> {
                                    commits += 1
                                    state = PlayerInputState.Timeline
                                }
                                PlayerInputOutcome.Reveal -> {
                                    reveals += 1
                                    state = PlayerInputState.Transport
                                }
                                PlayerInputOutcome.TogglePlay -> playPauses += 1
                                else -> Unit
                            }
                            true
                        },
                )
            }
        }
        compose.onNodeWithTag("player-adapter-root")
            .performSemanticsAction(SemanticsActions.RequestFocus)

        repeat(3) { compose.onRoot().performKeyInput { pressKey(Key.DirectionRight) } }
        compose.waitForIdle()
        assertEquals(3, previews)
        assertEquals(0, commits)

        compose.onRoot().performKeyInput { pressKey(Key.DirectionCenter) }
        compose.waitForIdle()
        assertEquals(1, commits)

        state = PlayerInputState.Hidden
        compose.onRoot().performKeyInput { pressKey(Key.DirectionRight) }
        compose.waitForIdle()
        assertEquals(1, reveals)
        assertEquals(1, commits)

        state = PlayerInputState.Menu
        compose.onRoot().performKeyInput { pressKey(Key.MediaPlayPause) }
        compose.waitForIdle()
        assertEquals(1, playPauses)
    }

    @Test
    fun rightFromSkipForwardStaysInTheTransportRow() {
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 30_000,
                    durationMs = 60_000,
                    isPlaying = true,
                    requestInitialFocus = false,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = null,
                )
            }
        }
        val forward = compose.onNodeWithContentDescription("Forward 10 seconds")
        val timeline = compose.onNodeWithContentDescription("Playback position")
        forward.performSemanticsAction(SemanticsActions.RequestFocus)
        forward.performKeyInput { pressKey(Key.DirectionRight) }
        timeline.assertIsNotFocused()
    }

    @Test
    fun transportOptionsFollowTheSharedContractOrder() {
        compose.setContent {
            PlurxTheme {
                Controls(
                    title = "Example episode",
                    positionMs = 30_000,
                    durationMs = 60_000,
                    isPlaying = true,
                    requestInitialFocus = false,
                    onClose = {},
                    onPlayPause = {},
                    onSeekBack30 = {},
                    onSeekBack = {},
                    onSeekForward = {},
                    onSeekForward30 = {},
                    onScrub = {},
                    onScrubEnd = {},
                    onTracks = {},
                    onSettings = {},
                    onInfo = {},
                    onPip = {},
                )
            }
        }

        val tracks = compose.onNodeWithContentDescription("Audio and subtitles")
        val settings = compose.onNodeWithContentDescription("Playback settings")
        val info = compose.onNodeWithContentDescription("Playback info")
        val pip = compose.onNodeWithContentDescription("Picture in picture")

        tracks.performSemanticsAction(SemanticsActions.RequestFocus)
        tracks.performKeyInput { pressKey(Key.DirectionRight) }
        settings.assertIsFocused()
        settings.performKeyInput { pressKey(Key.DirectionRight) }
        info.assertIsFocused()
        info.performKeyInput { pressKey(Key.DirectionRight) }
        pip.assertIsFocused()
    }
}
