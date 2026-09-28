package tv.plurx.app

import androidx.compose.material3.Text
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.navigation.NavBackStackEntry
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.ui.DetailBackButton

class BackNavigationTest {
    @get:Rule
    val compose = createComposeRule()

    private lateinit var nav: NavHostController

    private fun showNavigation(vararg routes: String) {
        compose.setContent {
            nav = rememberNavController()
            NavHost(navController = nav, startDestination = "home") {
                composable("home") { Text("Home") }
                composable("library") { Text("Library") }
                composable("detail/{id}") { entry ->
                    DetailBackButton(onBack = { nav.popBackStackFrom(entry) })
                }
                composable("player") { Text("Player") }
            }
        }
        compose.runOnIdle { routes.forEach { nav.navigate(it) } }
        compose.waitForIdle()
    }

    private fun tapBackTwiceBeforeRecomposition() {
        val click = compose.onNodeWithContentDescription("Back")
            .fetchSemanticsNode().config[SemanticsActions.OnClick].action!!
        // Keep both events in one UI turn: the outgoing button can still
        // receive a second tap before its exit animation removes it.
        compose.runOnIdle {
            click()
            click()
        }
    }

    @Test
    fun rapidBackFromMovieKeepsHomeVisible() {
        showNavigation("detail/1")
        tapBackTwiceBeforeRecomposition()
        compose.runOnIdle { assertEquals("home", nav.currentDestination?.route) }
        compose.onNodeWithText("Home").assertIsDisplayed()
    }

    @Test
    fun secondBackDuringExitAnimationKeepsHomeVisible() {
        showNavigation("detail/1")
        compose.mainClock.autoAdvance = false
        compose.onNodeWithContentDescription("Back").performClick()
        compose.mainClock.advanceTimeBy(100)
        compose.onNodeWithContentDescription("Back")
            .assertIsDisplayed()
            .performClick()
        compose.runOnIdle { assertEquals("home", nav.currentDestination?.route) }
        compose.mainClock.autoAdvance = true
        compose.onNodeWithText("Home").assertIsDisplayed()
    }

    @Test
    fun rapidBackFromMovieKeepsItsLibraryVisible() {
        showNavigation("library", "detail/1")
        tapBackTwiceBeforeRecomposition()
        compose.runOnIdle { assertEquals("library", nav.currentDestination?.route) }
        compose.onNodeWithText("Library").assertIsDisplayed()
    }

    @Test
    fun rapidBackBetweenIdenticalDetailRoutesOnlyPopsOneEntry() {
        showNavigation("detail/1", "detail/1")
        lateinit var previous: NavBackStackEntry
        compose.runOnIdle { previous = nav.previousBackStackEntry!! }
        tapBackTwiceBeforeRecomposition()
        compose.runOnIdle { assertEquals(previous.id, nav.currentBackStackEntry?.id) }
        compose.onNodeWithContentDescription("Back").assertIsDisplayed()
    }

    @Test
    fun coveredDetailCannotPopPlayerButCanGoBackAfterPlayback() {
        showNavigation("detail/1")
        compose.runOnIdle {
            val detail = nav.currentBackStackEntry!!
            nav.navigate("player")
            assertFalse(nav.popBackStackFrom(detail))
            assertEquals("player", nav.currentDestination?.route)
            assertTrue(nav.popBackStackFrom(nav.currentBackStackEntry!!))
            assertTrue(nav.popBackStackFrom(detail))
            assertEquals("home", nav.currentDestination?.route)
        }
        compose.onNodeWithText("Home").assertIsDisplayed()
    }

    @Test
    fun backAtRootNeverEmptiesNavigation() {
        showNavigation()
        compose.runOnIdle {
            assertFalse(nav.popBackStackFrom(nav.currentBackStackEntry!!))
            assertEquals("home", nav.currentDestination?.route)
        }
        compose.onNodeWithText("Home").assertIsDisplayed()
    }
}
