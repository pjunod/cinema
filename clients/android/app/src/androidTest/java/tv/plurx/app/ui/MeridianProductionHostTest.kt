@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.ui

import android.app.Application
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Color
import android.graphics.LinearGradient
import android.graphics.Shader
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.runtime.getValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createAndroidComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavHostController
import androidx.navigation.compose.rememberNavController
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertSame
import org.junit.Assert.assertNotNull
import tv.plurx.app.remote.*
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.MainNav
import tv.plurx.app.data.*
import tv.plurx.app.ui.theme.PlurxTheme
import java.io.File

/** Renders the actual MainNav and real screens, using only synthetic account-local data.
 * The optional screenshot is an emulator capture, never a design/mockup render. */
class MeridianProductionHostTest {
    @get:Rule val compose = createAndroidComposeRule<MeridianTestActivity>()
    private lateinit var nav: NavHostController
    private lateinit var vm: AppViewModel

    @Suppress("UNCHECKED_CAST")
    private fun <T> seed(name: String, value: T) {
        val field = AppViewModel::class.java.getDeclaredField(name).apply { isAccessible = true }
        (field.get(vm) as MutableStateFlow<T>).value = value
    }

    private fun start() {
        val app = InstrumentationRegistry.getInstrumentation().targetContext.applicationContext as Application
        vm = AppViewModel(app)
        runBlocking { vm.phase.first { it != Phase.Loading } }
        val titles = listOf("The Glass Horizon", "After the Rain", "Northern Passage", "City of Lanterns", "The Last Observatory", "Paper Moons")
        val items = titles.mapIndexed { index, title ->
            val bitmap = Bitmap.createBitmap(600, 900, Bitmap.Config.ARGB_8888)
            val canvas = Canvas(bitmap)
            val paint = Paint(Paint.ANTI_ALIAS_FLAG)
            val shades = listOf(0xFF284E69.toInt(), 0xFF725438.toInt(), 0xFF3A6270.toInt(), 0xFF694963.toInt(), 0xFF3C4E60.toInt(), 0xFF696445.toInt())
            paint.shader = LinearGradient(0f, 0f, 600f, 900f, shades[index], Color.rgb(16, 20, 31), Shader.TileMode.CLAMP)
            canvas.drawRect(0f, 0f, 600f, 900f, paint)
            paint.shader = null; paint.color = Color.argb(110, 240, 209, 145)
            canvas.drawCircle(390f, 235f, 145f, paint)
            paint.color = Color.argb(240, 16, 24, 36)
            for (building in 0..8) canvas.drawRect(building * 80f, 420f + (building % 3) * 50f, building * 80f + 65f, 900f, paint)
            paint.color = Color.WHITE; paint.textSize = 24f
            canvas.drawText("CINEMA · SYNTHETIC FIXTURE", 32f, 830f, paint)
            val artwork = File(app.filesDir, "meridian-fixture-$index.png")
            artwork.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
            Item(id = index + 1L, library_id = 7, kind = "movie", title = title, year = 2026, runtime_ms = 5_400_000, poster = artwork.toURI().toString(), backdrop = artwork.toURI().toString())
        }
        seed("_home", HomeState(hubs = Hubs(continue_watching = items.take(3), recently_added = items), libraries = listOf(Library(7, "Fixture films", "movie"), Library(8, "Fixture series", "show")), libraryItems = mapOf(7L to items), loading = false))
        seed("_preferences", ViewerPreferences(theme = ThemeId.Meridian, appearance = Appearance.Dark, layout = ViewerLayout.Meridian))
        compose.setContent {
            val preferences by vm.preferences.collectAsStateWithLifecycle()
            nav = rememberNavController()
            PlurxTheme(preferences.theme, preferences.appearance) {
                Surface(color = MaterialTheme.colorScheme.background) {
                    MainNav(vm, MutableStateFlow(null), {}, MutableStateFlow(null), {}, controller = nav)
                }
            }
        }
        compose.onNodeWithText("FEATURED IN YOUR LIBRARY").assertIsDisplayed()
    }

    private fun remoteCoordinator(): RemoteNavigationCoordinator {
        val model = RemoteRuntime.get(InstrumentationRegistry.getInstrumentation().targetContext)
        return model.javaClass.getDeclaredField("navigation").apply { isAccessible = true }.get(model) as RemoteNavigationCoordinator
    }

    private fun connectFixture(fixture: MeridianLoopbackFixture) {
        compose.runOnIdle {
            Session.origin = fixture.origin; Session.token = "synthetic-meridian"
            AppViewModel::class.java.getDeclaredField("origin").apply { isAccessible = true }.set(vm, fixture.origin)
            AppViewModel::class.java.getDeclaredField("api").apply { isAccessible = true }.set(vm, Net.api(fixture.origin))
        }
        compose.waitForIdle()
    }

    private fun openMenu() {
        val configuration = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration
        if (configuration.screenWidthDp < 600) compose.onNodeWithText("More").performClick()
        else compose.onNodeWithTag("meridian-menu").performClick()
        compose.waitForIdle()
    }

    private fun screenshot(name: String) {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val bitmap = instrumentation.uiAutomation.takeScreenshot()
        val path = File(instrumentation.targetContext.getExternalFilesDir(null), "meridian-$name.png")
        path.outputStream().use { bitmap.compress(Bitmap.CompressFormat.PNG, 100, it) }
    }

    private fun primary(label: String) {
        val configuration = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration
        val tv = configuration.uiMode and android.content.res.Configuration.UI_MODE_TYPE_MASK == android.content.res.Configuration.UI_MODE_TYPE_TELEVISION
        if (tv) {
            compose.onNodeWithTag("meridian-menu").performClick()
        }
        compose.onAllNodes(hasText(label) and hasClickAction()).onLast().performClick()
        compose.waitForIdle()
    }

    @Test fun actualHostRestoresSearchBranchReplacesSourceAndRetiresProfile() {
        start()
        primary("Search")
        compose.onNode(hasSetTextAction()).performTextInput("glass")
        compose.waitForIdle()
        val searchEntry = nav.currentBackStackEntry!!.id
        primary("Home")
        primary("Search")
        compose.onNode(hasSetTextAction()).assertTextContains("glass")
        assertEquals(searchEntry, nav.currentBackStackEntry!!.id)
        primary("Libraries")
        compose.onAllNodesWithText("Fixture films").onFirst().performClick()
        compose.waitForIdle()
        assertEquals("7", nav.currentBackStackEntry!!.arguments!!.getString("ids"))
        val sourceEntry = nav.currentBackStackEntry!!.id
        primary("Search")
        primary("Libraries")
        assertEquals(sourceEntry, nav.currentBackStackEntry!!.id)
        compose.runOnIdle { nav.popBackStack() }
        compose.waitForIdle()
        assertEquals("libraries", nav.currentDestination!!.route)
        compose.onAllNodesWithText("Fixture films").onFirst().performClick()
        compose.waitForIdle()
        assertEquals("7", nav.currentBackStackEntry!!.arguments!!.getString("ids"))
        val configuration = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration
        if (configuration.screenWidthDp < 600) compose.onNodeWithText("More").performClick()
        else compose.onNodeWithTag("meridian-menu").performClick()
        compose.onAllNodesWithText("Fixture series").onLast().performScrollTo().performClick()
        compose.waitForIdle()
        assertEquals("8", nav.currentBackStackEntry!!.arguments!!.getString("ids"))
        assertEquals("libraries", nav.previousBackStackEntry!!.destination.route)
        compose.runOnIdle { seed("_home", vm.home.value.copy(libraries = listOf(Library(7, "Fixture films", "movie")))) }
        compose.waitForIdle()
        assertEquals("libraries", nav.currentDestination!!.route)
        compose.onNodeWithText("The selected library is no longer available. Choose another library.").assertIsDisplayed()
        compose.runOnIdle {
            AppViewModel::class.java.getDeclaredField("currentUserId").apply { isAccessible = true }.set(vm, 2L)
            vm.setAppearance(Appearance.Light)
        }
        compose.waitForIdle()
        assertEquals("home", nav.currentDestination!!.route)
        primary("Search")
        assertNotEquals(searchEntry, nav.currentBackStackEntry!!.id)
        compose.onNode(hasSetTextAction()).assert(SemanticsMatcher.expectValue(androidx.compose.ui.semantics.SemanticsProperties.EditableText, androidx.compose.ui.text.AnnotatedString("")))
        compose.runOnIdle {
            val coordinator = remoteCoordinator()
            assertNotNull(coordinator.onHome)
            assertEquals(RemoteOutcome.Applied, coordinator.dispatch(RemoteAction("home", buildJsonObject {}), coordinator.context))
        }
        compose.waitForIdle()
        assertEquals("home", nav.currentDestination!!.route)
    }

    @Test fun populatedSharedQueriesAndNestedCompanionMenuKeepTheirActualOwners() {
        val previous = Session.playbackAuthorization()
        MeridianLoopbackFixture().use { fixture ->
            try {
                start(); connectFixture(fixture)
                openMenu()
                compose.onAllNodes(hasText("Shared libraries") and hasClickAction()).onLast().performScrollTo().performClick()
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Shared films · movie · Fixture Source").fetchSemanticsNodes().isNotEmpty() }
                compose.onNodeWithText("Shared films · movie · Fixture Source").performScrollTo().performClick()
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Original Shared title · movie · Fixture Source").fetchSemanticsNodes().isNotEmpty() }
                compose.onNode(hasSetTextAction()).performTextInput("native")
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Filtered native · movie · Fixture Source").fetchSemanticsNodes().isNotEmpty() }
                compose.runOnIdle {
                    val coordinator = remoteCoordinator(); val snapshot = coordinator.snapshot()
                    assertEquals(RemoteOutcome.Applied, coordinator.dispatch(RemoteAction("text_replace", buildJsonObject { put("text_nonce", snapshot.textNonce); put("text", "companion") }), coordinator.context))
                }
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Filtered companion · movie · Fixture Source").fetchSemanticsNodes().isNotEmpty() }
                assertTrue(fixture.queries.any { it.first == "native" && it.second == null })
                assertTrue(fixture.queries.any { it.first == "companion" && it.second == null })
                compose.onNodeWithText("Filtered companion · movie · Fixture Source").performScrollTo().performClick()
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Nested Shared detail").fetchSemanticsNodes().isNotEmpty() }
                val entry = nav.currentBackStackEntry!!.id
                compose.runOnIdle { vm.setLayout(ViewerLayout.Standard) }; compose.waitForIdle()
                compose.runOnIdle { vm.setLayout(ViewerLayout.Meridian) }; compose.waitForIdle()
                compose.onNodeWithText("Nested Shared detail").assertIsDisplayed()
                assertEquals(entry, nav.currentBackStackEntry!!.id)
                primary("Home")
                assertEquals("home", nav.currentDestination!!.route)
                openMenu(); compose.onAllNodes(hasText("Shared libraries") and hasClickAction()).onLast().performScrollTo().performClick()
                compose.waitUntil(10_000) { compose.onAllNodesWithText("Nested Shared detail").fetchSemanticsNodes().isNotEmpty() }
                assertEquals(entry, nav.currentBackStackEntry!!.id)
                openMenu()
                InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_DPAD_DOWN)
                compose.onAllNodes(hasText("Home") and hasClickAction()).onLast().performSemanticsAction(androidx.compose.ui.semantics.SemanticsActions.RequestFocus)
                compose.waitUntil(5_000) { remoteCoordinator().focusedControl == "meridian:home" }
                screenshot("shared-menu")
                compose.runOnIdle {
                    val coordinator = remoteCoordinator()
                    assertTrue(coordinator.scope.startsWith("shared:"))
                    assertEquals(RemoteOutcome.Applied, coordinator.dispatch(RemoteAction("select", buildJsonObject {}), coordinator.context))
                }
                compose.waitForIdle(); assertEquals("home", nav.currentDestination!!.route)
            } finally { Session.origin = previous.origin; Session.token = previous.token }
        }
    }

    private fun nativePlayer(): androidx.media3.common.Player? {
        var result: androidx.media3.common.Player? = null
        InstrumentationRegistry.getInstrumentation().runOnMainSync {
            fun visit(view: android.view.View) {
                if (view is androidx.media3.ui.PlayerView) result = view.player
                if (view is android.view.ViewGroup) for (index in 0 until view.childCount) visit(view.getChildAt(index))
            }
            androidx.test.runner.lifecycle.ActivityLifecycleMonitorRegistry.getInstance()
                .getActivitiesInStage(androidx.test.runner.lifecycle.Stage.RESUMED).forEach { visit(it.window.decorView) }
        }
        return result
    }

    @Test fun activeNativePlayerOwnerSurvivesLayoutPaletteAndAppearanceChanges() {
        val previous = Session.playbackAuthorization()
        MeridianLoopbackFixture().use { fixture ->
            try {
                start(); connectFixture(fixture)
                compose.runOnIdle { nav.navigate("player/1/11/0") }
                compose.waitUntil(15_000) { nativePlayer() != null }
                val player = requireNotNull(nativePlayer())
                compose.runOnIdle {
                    player.volume = 0f; player.play()
                }
                compose.waitUntil(15_000) {
                    var playing = false
                    InstrumentationRegistry.getInstrumentation().runOnMainSync { playing = player.isPlaying }
                    playing
                }
                val entry = nav.currentBackStackEntry!!.id
                val decisions = fixture.decisionCalls.get()
                compose.runOnIdle { vm.setLayout(ViewerLayout.Standard); vm.setTheme(ThemeId.Terminal); vm.setAppearance(Appearance.Light) }
                compose.waitForIdle()
                assertSame(player, nativePlayer()); assertEquals(entry, nav.currentBackStackEntry!!.id)
                compose.runOnIdle { vm.setLayout(ViewerLayout.Meridian); vm.setTheme(ThemeId.Meridian); vm.setAppearance(Appearance.Dark) }
                compose.waitForIdle()
                assertSame(player, nativePlayer()); assertEquals(entry, nav.currentBackStackEntry!!.id)
                assertEquals(decisions, fixture.decisionCalls.get())
                compose.runOnIdle { assertTrue(player.isPlaying) }
                screenshot("active-player-owner")
                compose.runOnIdle { nav.popBackStack() }; compose.waitForIdle()
            } finally { Session.origin = previous.origin; Session.token = previous.token }
        }
    }

    @Test fun cancelledEpisodeStartCanLeaveRestoreAndRetryTheActualSeries() {
        val previous = Session.playbackAuthorization()
        MeridianLoopbackFixture().use { fixture ->
            try {
                start(); connectFixture(fixture)
                compose.runOnIdle { nav.navigate("detail/20") }
                compose.waitUntil(10_000) { fixture.episodeCalls.get() >= 1 && compose.onAllNodesWithText("1. Delayed episode").fetchSemanticsNodes().isNotEmpty() }
                compose.runOnIdle { fixture.holdEpisode.set(true) }
                compose.onNodeWithText("1. Delayed episode").performScrollTo().performClick()
                compose.waitUntil(10_000) { fixture.episodeCalls.get() >= 2 }
                primary("Search")
                fixture.holdEpisode.set(false); fixture.releaseEpisode.countDown()
                primary("Home")
                compose.waitUntil(10_000) { compose.onAllNodesWithContentDescription("Play episode").fetchSemanticsNodes().isNotEmpty() }
                val before = fixture.episodeCalls.get()
                compose.onNodeWithText("1. Delayed episode").performScrollTo().performClick()
                compose.waitUntil(10_000) { fixture.episodeCalls.get() > before }
                compose.waitUntil(10_000) { nav.currentDestination?.route?.startsWith("player/") == true }
                compose.runOnIdle { nav.popBackStack("home", false) }; compose.waitForIdle()
            } finally { Session.origin = previous.origin; Session.token = previous.token }
        }
    }

    @Test fun actualHostCapturesHomeAndPreservesNestedSettingsEntryAcrossLayoutAndPalette() {
        start()
        screenshot("home")
        compose.runOnIdle { nav.navigate("settings"); nav.navigate("remote-devices") }
        compose.waitForIdle()
        val entry = nav.currentBackStackEntry!!.id
        compose.runOnIdle { vm.setLayout(ViewerLayout.Standard); vm.setTheme(ThemeId.Terminal) }
        compose.waitForIdle()
        assertEquals(entry, nav.currentBackStackEntry!!.id)
        compose.runOnIdle { vm.setLayout(ViewerLayout.Meridian); vm.setTheme(ThemeId.Meridian); vm.setAppearance(Appearance.Light) }
        compose.waitForIdle()
        assertEquals(entry, nav.currentBackStackEntry!!.id)
        screenshot("nested-settings-light")
        compose.runOnIdle { nav.popBackStack() }
        compose.waitForIdle()
        assertEquals("settings", nav.currentDestination!!.route)
        assertNotEquals(entry, nav.currentBackStackEntry!!.id)
    }

    @Test fun actualHostTvMenuTransfersNativeFocusAndBackKeepsEntry() {
        start()
        val configuration = InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration
        if (configuration.screenWidthDp < 600) {
            compose.onNodeWithText("More").performClick()
        } else {
            compose.onNodeWithTag("meridian-menu").performClick()
        }
        compose.onNodeWithText("Close navigation").assertExists()
        compose.waitForIdle()
        if (configuration.uiMode and android.content.res.Configuration.UI_MODE_TYPE_MASK == android.content.res.Configuration.UI_MODE_TYPE_TELEVISION) {
            compose.waitUntil(5_000) { compose.onAllNodes(isFocused()).fetchSemanticsNodes().isNotEmpty() }
        }
        screenshot("navigation")
        val entry = nav.currentBackStackEntry!!.id
        InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(android.view.KeyEvent.KEYCODE_BACK)
        compose.waitForIdle()
        assertEquals(entry, nav.currentBackStackEntry!!.id)
        compose.onNodeWithText("FEATURED IN YOUR LIBRARY").assertIsDisplayed()
    }
}
