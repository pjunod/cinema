package tv.plurx.app.ui

import android.graphics.Bitmap
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.hasAnyAncestor
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.isDialog
import kotlinx.coroutines.CompletableDeferred
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
            capture("library-rows-movies")
            compose.onNodeWithText("Z").performClick()
            compose.onNodeWithText("Zulu 401").assertIsDisplayed()
            compose.onNodeWithContentDescription("View all Z · 21").performClick()
            compose.onNode(hasText("Zulu 401") and hasAnyAncestor(isDialog())).performClick()
            compose.runOnIdle { assertEquals(401L, opened) }
            // Opening an item closes its owned expanded window. Reopen the
            // group to exercise the explicit return-to-rows action too.
            compose.onNodeWithText("Zulu 401").assertIsDisplayed()
            compose.onNodeWithContentDescription("View all Z · 21").performClick()
            compose.onNodeWithText("All rows").performClick()
            compose.onNodeWithText("Zulu 401").assertIsDisplayed()
            compose.onNodeWithText("Rows").performClick()
            compose.onNodeWithText("Grid").performClick()
            compose.onNodeWithText("Alpha 1").assertIsDisplayed()
        } finally { scope.cancel() }
    }
    @Test fun expandedGroupShowsFailureAndRetryWithoutClosing() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val failPage = CompletableDeferred<Unit>()
        val all = (1L..421).map { Item(id = it, kind = "movie", title = "Alpha $it", sort_title = "alpha") }
        var fail = true
        val pager = LibraryPager(listOf(1), "title", scope) { _, offset, _ ->
            if (offset == 200 && fail) { failPage.await(); fail = false; error("controlled failure") }
            Page(all.drop(offset).take(200), total = all.size)
        }
        try {
            compose.setContent {
                PlurxTheme {
                    LibraryScreen(listOf(1), "Movie fixture", ViewerPreferences(posterSize = PosterSize.Small), "movie",
                        { pager }, {}, {})
                }
            }
            compose.waitUntil(5_000) { compose.onAllNodesWithText("View all").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("View all").performClick()
            failPage.complete(Unit)
            compose.waitUntil(5_000) { pager.state.value.error != null }
            compose.onNode(hasText("Incomplete library: controlled failure") and hasAnyAncestor(isDialog())).assertIsDisplayed()
            compose.onNode(hasText("Retry") and hasAnyAncestor(isDialog())).performClick()
            compose.waitUntil(5_000) { pager.state.value.complete }
            compose.onNode(hasText("A · 421") and hasAnyAncestor(isDialog())).assertIsDisplayed()
        } finally { scope.cancel() }
    }

    @Test fun homeVideoRowsUseRecordingYearsAndLandscapeArtwork() {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val all = (1L..12).map { id ->
            Item(id = id, kind = "video", title = "Home video $id", sort_title = "home video $id",
                recorded_at = if (id <= 6) "2026-08-01" else "2025-07-01", runtime_ms = 720_000)
        }
        val pager = LibraryPager(listOf(1), "recorded", scope) { _, _, order ->
            assertEquals("recorded", order); Page(all, total = all.size)
        }
        try {
            compose.setContent {
                PlurxTheme {
                    LibraryScreen(listOf(1), "Home videos", ViewerPreferences(posterSize = PosterSize.Small), "home",
                        { pager }, {}, {})
                }
            }
            compose.waitUntil(5_000) { compose.onAllNodesWithText("2026 · 6").fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText("Date recorded").assertIsDisplayed()
            val bounds = compose.onAllNodesWithTag("poster-artwork", useUnmergedTree = true)[0].getUnclippedBoundsInRoot()
            assertEquals(16f / 9f, (bounds.right - bounds.left).value / (bounds.bottom - bounds.top).value, .02f)
            capture("library-rows-home-videos")
        } finally { scope.cancel() }
    }

    private fun capture(name: String) {
        compose.waitForIdle()
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val bitmap = instrumentation.uiAutomation.takeScreenshot() ?: return
        File(instrumentation.targetContext.cacheDir, "$name.png").outputStream().use {
            bitmap.compress(Bitmap.CompressFormat.PNG, 100, it)
        }
        bitmap.recycle()
    }

}
