package tv.plurx.app.livetv

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.ui.PREPARED_REPLACEMENT_GRADUATION

/**
 * Where Android draws the server's Live TV settings. The web moved the tuner,
 * guide, recording and Library channel cards from Developer to Settings → Live
 * TV; Paul's Developer rule is that a finished setting does not wait in
 * Developer. These tests pin both halves: the Live TV screen draws the four
 * cards and their saves, and the Developer screen draws none of them and says
 * what each card it keeps is waiting on.
 */
class LiveTvSettingsPlacementTest {
    private fun source(path: String): String = listOf(
        File("app/src/main/java/tv/plurx/app/$path"),
        File("src/main/java/tv/plurx/app/$path"),
        File("clients/android/app/src/main/java/tv/plurx/app/$path"),
    ).firstOrNull(File::isFile)?.readText() ?: error("$path source not found")

    /** Every edit a moved card makes, and the read only the guide card makes. */
    private val movedCalls = mapOf(
        LiveTvSettingsCard.HdHomeRun to listOf("LiveTvSettingsChange.Configure(", "checkSavedConfiguration", "readiness?.checks"),
        LiveTvSettingsCard.ProgrammeGuide to listOf("refreshGuideReadiness", "guideReadiness?.let"),
        LiveTvSettingsCard.Recording to listOf("LiveTvSettingsChange.DvrEnabled(", "\"Enable recording\""),
        LiveTvSettingsCard.LibraryChannels to listOf("LiveTvSettingsChange.LibraryChannelsEnabled(", "\"Enable Library channels\""),
    )

    @Test
    fun liveTvSettingsOwnTheFourCardsTheWebMovedInTheWebsOrder() {
        assertEquals(
            listOf("HDHomeRun Live TV", "Programme guide", "Recording", "Library channels"),
            LiveTvSettingsPlacement.liveTvSettings.map { it.title },
        )
        assertTrue(
            "a card cannot be drawn on both screens",
            LiveTvSettingsPlacement.liveTvSettings.none { it in LiveTvSettingsPlacement.developer },
        )
        val screen = source("livetv/LiveTvSettingsScreen.kt")
        assertTrue(screen.contains("LiveTvSettingsPlacement.liveTvSettings.forEach"))
        movedCalls.forEach { (card, calls) ->
            calls.forEach { call ->
                assertTrue("Settings → Live TV must draw ${card.title} ($call)", screen.contains(call))
            }
        }
    }

    @Test
    fun developerNoLongerListsTheMovedCardsAndKeepsOnlyWhatIsWaiting() {
        assertEquals(
            listOf(LiveTvSettingsCard.DisplayCadence, LiveTvSettingsCard.EnableLiveTv),
            LiveTvSettingsPlacement.developer,
        )
        val developer = source("livetv/LiveTvDeveloperScreen.kt")
        movedCalls.forEach { (card, calls) ->
            calls.forEach { call ->
                assertFalse("Developer must not draw ${card.title} ($call)", developer.contains(call))
            }
        }
        // The two cards that stay are the ones whose evidence is still owed,
        // and each says what it waits on — the web's `devGraduation` line.
        assertTrue(developer.contains("LiveTvSettingsChange.Enabled("))
        assertTrue(developer.contains("LiveTvSettingsChange.DisplayModeMatch("))
        assertTrue(developer.contains("LiveTvSettingsPlacement.ENABLE_LIVE_TV_GRADUATION"))
        assertTrue(developer.contains("LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION"))
        assertTrue(LiveTvSettingsPlacement.ENABLE_LIVE_TV_GRADUATION.startsWith("Leaves Developer when: "))
        assertTrue(LiveTvSettingsPlacement.ENABLE_LIVE_TV_GRADUATION.contains(" Then: the switch moves to Settings → Live TV"))
        assertTrue(LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION.startsWith("Leaves Developer when: "))
        assertTrue(LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION.contains(" Then: "))
        // The guide is not drawn here, so it is not read here either.
        assertTrue(developer.contains("rememberLiveTvAdminState(origin, readsGuide = false)"))
    }

    @Test
    fun settingsReachesLiveTvTheWayItReachesDeveloper() {
        val settings = source("ui/SettingsScreen.kt")
        val liveTv = settings.substringAfter("SettingsSection(\n                \"Live TV\",")
            .substringBefore("SettingsSection(\"Account\"")
        assertTrue("Settings must have a Live TV section", liveTv.length < settings.length)
        assertTrue(liveTv.contains("PreferenceAction(\"Tuner, guide, recording and Library channels\", onClick = onOpenLiveTvSettings)"))
        val developer = settings.substringAfter("SettingsSection(\"Developer\"").substringBefore("SettingsSection(\"About\"")
        assertFalse("the Developer entry no longer promises HDHomeRun setup", developer.contains("HDHomeRun"))
        // The one Developer entry drawn on Settings itself says what it waits on too.
        assertTrue(developer.contains("PreparedReplacementEnable("))
        assertTrue(settings.contains("            PREPARED_REPLACEMENT_GRADUATION,\n"))
        assertTrue(PREPARED_REPLACEMENT_GRADUATION.startsWith("Leaves Developer when: "))
        val activity = source("MainActivity.kt")
        assertTrue(activity.contains("onOpenLiveTvSettings = { nav.navigate(\"live-tv-settings\") }"))
        assertTrue(activity.contains("composable(\"live-tv-settings\")"))
        assertTrue(activity.contains("LiveTvSettingsScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })"))
    }
}
