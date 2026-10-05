package tv.plurx.app.ui

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Item
import tv.plurx.app.data.Page
import tv.plurx.app.data.PosterSize
import tv.plurx.app.data.ViewerPreferences
import tv.plurx.app.ui.theme.PlurxTheme

/** Real library screen with only the page transport supplied by the test. */
class LibraryRowsNavigationTest {
    @get:Rule val compose = createComposeRule()

    @Test fun jumpAndViewAllReachTheUnfetchedTailAndReturnToRows() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val all = (1L..421).map { id ->
            val key = if (id <= 400) "Alpha" else "Zulu"
            Item(id = id, kind = "movie", title = "$key $id", sort_title = "$key %04d".format(id).lowercase())
        }
        val pager = LibraryPager(listOf(1), "title", scope) { _, offset, _ -> Page(all.drop(offset).take(200), total = all.size) }
        var opened: Long? = null
        try {
            compose.setContent {
                PlurxTheme {
                    LibraryScreen(listOf(1), "Movie fixture", ViewerPreferences(posterSize = PosterSize.Small), "movie",
                        { pager }, { opened = it }, {})
                }
            }
            compose.waitUntil(5_000) { pager.state.value.complete }
            compose.waitUntil(5_000) { compose.onAllNodesWithText("421 of 421 loaded · 421 match").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Choose group").performClick()
            compose.onNodeWithText("Z").performClick()
            compose.onNodeWithText("Zulu 401").assertIsDisplayed()
            compose.onNodeWithText("View all").performClick()
            compose.onNodeWithText("Zulu 401").performClick()
            compose.runOnIdle { assertEquals(401L, opened) }
            compose.onNodeWithText("All rows").performClick()
            compose.onNodeWithText("Zulu 401").assertIsDisplayed()
            compose.onNodeWithText("Rows").performClick()
            compose.onNodeWithText("Grid").performClick()
            compose.onNodeWithText("Alpha 1").assertIsDisplayed()
        } finally { scope.cancel() }
    }
}
