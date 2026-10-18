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
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.navigation.NavHostController
import androidx.navigation.compose.rememberNavController
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.MainNav
import tv.plurx.app.data.*
import tv.plurx.app.ui.theme.PlurxTheme
import java.io.File

/** Renders the actual MainNav and real screens, using only synthetic account-local data.
 * The optional screenshot is an emulator capture, never a design/mockup render. */
class MeridianProductionHostTest {
    @get:Rule val compose = createComposeRule()
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
