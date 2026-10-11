package tv.plurx.app.ui

import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.width
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.getUnclippedBoundsInRoot
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.unit.dp
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.ThemeId

class HomeTopBarTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun liveTvAndSettingsFitNarrowPhonesAndLiveActionIsReachable() {
        var opened = false
        var settingsOpened = false
        compose.setContent {
            Box(Modifier.width(320.dp)) {
                HomeTopBar(theme = ThemeId.Classic, username = "viewer", formFactor = FormFactor.Compact,
                    side = 20.dp, onRefresh = {}, onSearch = {}, onOpenSettings = { settingsOpened = true },
                    safeInsets = WindowInsets(0, 0, 0, 0), onOpenLiveTv = { opened = true })
            }
        }
        val live = compose.onNodeWithContentDescription("Live TV").assertIsDisplayed().assertHasClickAction()
        val settings = compose.onNodeWithContentDescription("Settings").assertIsDisplayed().assertHasClickAction()
        for (label in listOf("Refresh", "Search", "Live TV", "Recordings", "Library channels", "Downloads", "Settings")) {
            val action = compose.onNodeWithContentDescription(label).assertIsDisplayed().assertHasClickAction()
            val bounds = action.getUnclippedBoundsInRoot()
            assertTrue("$label starts inside the narrow viewport", bounds.left >= 0.dp)
            assertTrue("$label ends inside the narrow viewport", bounds.right <= 320.dp)
        }
        assertTrue(live.getUnclippedBoundsInRoot().right <= 320.dp)
        assertTrue(settings.getUnclippedBoundsInRoot().right <= 320.dp)
        live.performClick()
        compose.runOnIdle { assertTrue(opened) }
        settings.performClick()
        compose.runOnIdle { assertTrue(settingsOpened) }
    }

    @Test
    fun headerControlsClearTopAndSideSystemInsets() {
        val topInset = 32.dp
        val sideInset = 18.dp

        compose.setContent {
            HomeTopBar(
                theme = ThemeId.Classic,
                username = "viewer",
                formFactor = FormFactor.Compact,
                side = 20.dp,
                onRefresh = {},
                onSearch = {},
                onOpenSettings = {},
                safeInsets = WindowInsets(left = sideInset, top = topInset),
            )
        }

        val brand = compose.onNodeWithText("cinema").assertIsDisplayed()
        val search = compose.onNodeWithContentDescription("Search")
            .assertIsDisplayed()
            .assertHasClickAction()
        val settings = compose.onNodeWithContentDescription("Settings")
            .assertIsDisplayed()
            .assertHasClickAction()

        assertTrue("Home title must clear the status bar", brand.getUnclippedBoundsInRoot().top >= topInset)
        assertTrue("Home title must clear a side display cutout", brand.getUnclippedBoundsInRoot().left >= sideInset)
        assertTrue("Search must clear the status bar", search.getUnclippedBoundsInRoot().top >= topInset)
        assertTrue("Settings must clear the status bar", settings.getUnclippedBoundsInRoot().top >= topInset)
    }
}
