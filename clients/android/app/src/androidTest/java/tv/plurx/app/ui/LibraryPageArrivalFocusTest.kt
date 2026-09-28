package tv.plurx.app.ui

import android.content.res.Configuration
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.isFocused
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performKeyInput
import androidx.compose.ui.test.performSemanticsAction
import androidx.compose.ui.test.pressKey
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page
import tv.plurx.app.data.PosterSize
import tv.plurx.app.data.ViewerPreferences
import tv.plurx.app.ui.theme.PlurxTheme
import java.util.concurrent.atomic.AtomicBoolean

/** Actual production LibraryScreen, LibraryPager and PosterCard focus targets.
 * Only transport/preferences are supplied locally; no view-model/account,
 * poster URL, server, or physical-device acceptance is involved. Run on TV. */
class LibraryPageArrivalFocusTest {
    @get:Rule val compose = createComposeRule()

    @Test fun arrivingPagePreservesTheDpadSelectedCardAndItsOpenAction() {
        val configuration = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration
        check(configuration.uiMode and Configuration.UI_MODE_TYPE_MASK == Configuration.UI_MODE_TYPE_TELEVISION) {
            "This D-pad regression requires an owned Android TV test image; a skipped phone run is not evidence"
        }
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val page = CompletableDeferred<Page>()
        val nextRequested = AtomicBoolean(false)
        val all = (1L..400L).map { id ->
            Item(id = id, kind = "movie", title = "Fixture %04d".format(id), sort_title = "fixture %04d".format(id))
        }
        val pager = LibraryPager(listOf(7), "title", scope) { _, offset, order ->
            check(order == "title")
            when (offset) {
                0 -> Page(all.take(200), total = 400)
                200 -> { nextRequested.set(true); page.await() }
                else -> error("Unexpected page offset $offset")
            }
        }
        val factory: (String) -> LibraryPager = { order -> check(order == "title"); pager }
        var opened: Long? = null
        try {
            compose.setContent {
                PlurxTheme {
                    LibraryScreen(
                        libraryIds = listOf(7), title = "Controlled library",
                        preferences = ViewerPreferences(posterSize = PosterSize.Small), kind = "movie",
                        pagerFactory = factory, onOpenItem = { opened = it }, onBack = {},
                    )
                }
            }
            compose.waitUntil(5_000) { pager.state.value.loadedCount == 200 }
            compose.waitUntil(5_000) {
                compose.onAllNodesWithText("200 of 400 loaded · 200 match").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("200 of 400 loaded · 200 match").assertExists()
            compose.onNodeWithText("Fixture 0001").performSemanticsAction(SemanticsActions.RequestFocus)
            compose.waitForIdle()
            compose.onRoot().performKeyInput { pressKey(Key.DirectionRight) }
            compose.waitForIdle()
            val selected = compose.onNodeWithText("Fixture 0002")
            selected.assertIsFocused()
            compose.runOnIdle { scope.launch { pager.ensure(201) } }
            compose.waitUntil(5_000) { nextRequested.get() }
            selected.assertIsFocused()
            page.complete(Page(all.drop(200), total = 400))
            compose.waitUntil(5_000) { pager.state.value.complete }
            compose.waitUntil(5_000) {
                compose.onAllNodesWithText("400 of 400 loaded · 400 match").fetchSemanticsNodes().isNotEmpty()
            }
            compose.onNodeWithText("400 of 400 loaded · 400 match").assertExists()
            selected.assertIsFocused()
            assertEquals(1, compose.onAllNodes(isFocused()).fetchSemanticsNodes().size)
            compose.onRoot().performKeyInput { pressKey(Key.DirectionCenter) }
            compose.runOnIdle { assertEquals(2L, opened) }
        } finally {
            scope.cancel()
        }
    }
}
