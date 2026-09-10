package tv.plurx.app.livetv

import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.pressKey
import androidx.compose.ui.unit.dp
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.ui.theme.PlurxTheme

/** Real key events through the guide modifier; reducer-only checks cannot prove focus wiring. */
class LiveTvGuideFocusTest {
    @get:Rule val compose = createComposeRule()

    private fun cell(start: Long, end: Long, title: String) = LiveTvGridCell(
        LiveTvProgramme(start, end, title),
        left = start.toFloat() / 18,
        width = (end - start).toFloat() / 18,
        airing = false,
        clipped = false,
    )

    @Test
    fun dpadKeepsTimeAnchorAndSelectActivatesExactlyOnce() {
        val channels = listOf(
            LiveTvChannel("one", "1.1", "One"),
            LiveTvChannel("two", "2.1", "Two"),
            LiveTvChannel("three", "3.1", "Three"),
        )
        val layout = LiveTvGridLayout(
            rows = listOf(
                LiveTvGridRow(channels[0], listOf(cell(0, 6_000, "Wide"))),
                LiveTvGridRow(channels[1], listOf(cell(0, 1_800, "Early"), cell(1_800, 2_700, "Short"), cell(2_700, 5_400, "Long"))),
                LiveTvGridRow(channels[2], listOf(cell(0, 1_200, "A"), cell(1_200, 2_400, "B"), cell(2_400, 3_600, "C"))),
            ),
            totalWidth = 480f,
            nowX = null,
        )
        var details = 0
        compose.setContent {
            PlurxTheme {
                LiveTvGuideGrid(
                    layout = layout,
                    slots = listOf(0, 1_800, 3_600),
                    playingChannelId = null,
                    onAiring = {},
                    onFuture = { _, _ -> details += 1 },
                    dpadNavigation = true,
                    modifier = Modifier.width(640.dp).height(260.dp),
                )
            }
        }

        // Seeding represents the viewer arriving at the guide. Every move and
        // activation after it is the same KeyEvent a physical D-pad produces.
        compose.onNodeWithText("Wide").performSemanticsAction(SemanticsActions.RequestFocus)
        compose.onRoot().performKeyInput { pressKey(Key.DirectionDown) }
        compose.waitForIdle()
        compose.onNodeWithText("Long").assertIsFocused()
        compose.onRoot().performKeyInput { pressKey(Key.DirectionDown) }
        compose.waitForIdle()
        compose.onNodeWithText("C").assertIsFocused()
        compose.onRoot().performKeyInput { pressKey(Key.DirectionCenter) }
        compose.waitForIdle()
        assertEquals(1, details)
    }
}
