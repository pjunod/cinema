package tv.plurx.app.livetv

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.ui.PREPARED_REPLACEMENT_GRADUATION
import tv.plurx.app.ui.RELEASE_PROFILE_GRADUATION

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

    /** The body of one private card composable, up to the next declaration. */
    private fun cardBody(source: String, name: String): String {
        val start = source.indexOf("private fun $name(")
        assertTrue("$name is not declared", start >= 0)
        val rest = source.substring(start + 1)
        val end = rest.indexOf("\n@Composable").takeIf { it >= 0 } ?: rest.length
        return rest.substring(0, end)
    }

    /** The composable that draws each card on its screen. */
    private val drawnBy = mapOf(
        LiveTvSettingsCard.HdHomeRun to "HdHomeRunCard",
        LiveTvSettingsCard.ProgrammeGuide to "ProgrammeGuideCard",
        LiveTvSettingsCard.Recording to "RecordingCard",
        LiveTvSettingsCard.LibraryChannels to "LibraryChannelsCard",
        LiveTvSettingsCard.DisplayCadence to "DisplayCadenceCard",
        LiveTvSettingsCard.EnableLiveTv to "EnableLiveTvCard",
    )

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
        // What the screen draws, not just what the file mentions: each card is
        // dispatched to its composable, and that composable makes the card's
        // edits and draws its title.
        movedCalls.forEach { (card, calls) ->
            val composable = drawnBy.getValue(card)
            assertTrue(
                "Settings → Live TV must dispatch ${card.title} to $composable",
                screen.contains("LiveTvSettingsCard.${card.name} -> $composable(state)"),
            )
            val body = cardBody(screen, composable)
            assertTrue(body.contains("Text(LiveTvSettingsCard.${card.name}.title"))
            calls.forEach { call ->
                assertTrue("${card.title} must draw $call", body.contains(call))
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
        LiveTvSettingsPlacement.developer.forEach { card ->
            assertTrue(developer.contains("LiveTvSettingsCard.${card.name} -> ${drawnBy.getValue(card)}(state)"))
        }
        // The guide is not drawn here, so it is not read here either; the
        // enable card's prerequisites are drawn only here, so only here read.
        assertTrue(developer.contains("rememberLiveTvAdminState(origin, LiveTvAdminSurface.Developer)"))
        assertFalse(LiveTvAdminSurface.Developer.readsGuide)
        assertTrue(LiveTvAdminSurface.Developer.readsEnablePrerequisites)
        assertTrue(LiveTvAdminSurface.LiveTvSettings.readsGuide)
        assertFalse(LiveTvAdminSurface.LiveTvSettings.readsEnablePrerequisites)
    }

    /**
     * Paul's rule for an enable section: say what is needed to turn it on
     * safely and whether each part is met right now — advisory, never a gate.
     * The web's Enable Live TV card draws `/live-tv/readiness` beside its
     * toggle; so does the native Developer card, and nothing it draws reaches
     * the button.
     */
    @Test
    fun theDeveloperEnableCardDrawsEveryPrerequisiteRowAndGatesNothing() {
        val developer = source("livetv/LiveTvDeveloperScreen.kt")
        val card = cardBody(developer, "EnableLiveTvCard")
        assertTrue(card.contains("state.prerequisites?.let { prerequisites ->"))
        assertTrue(
            "every row the server sends, never a hand-written subset",
            card.contains("prerequisites.checks.forEach { Text(\"\${if (it.ready) \"Met\" else \"Not met\"}: \${it.message}\") }"),
        )
        assertFalse(card.contains("it.id =="))
        assertTrue(card.contains("Readiness unavailable: \$it. You can still enable Live TV."))
        assertTrue(card.contains("onClick = state::refreshPrerequisites"))
        // The enable gates on an in-flight request only.
        assertTrue(card.contains("Button(enabled = !state.busy, onClick = { state.write(LiveTvSettingsChange.Enabled(!settings.live_tv_enabled)) })"))
        for (gate in listOf("prerequisites.ready", "it.ready &&", "prerequisitesError == null")) {
            assertFalse("$gate would let an advisory check block the enable", card.contains(gate))
        }
        val state = source("livetv/LiveTvAdminState.kt")
        assertTrue(state.contains("if (surface.readsEnablePrerequisites) readPrerequisites(client)"))
        assertTrue(state.contains("prerequisites = client.currentReadiness()"))
        assertTrue(source("livetv/LiveTvApi.kt").contains("request(url(\"live-tv\", \"readiness\"), authenticated = true)"))
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
        // Every other entry drawn in Settings' Developer section has its line too.
        assertTrue(developer.contains("Text(\"Release startup profile\""))
        assertTrue(developer.contains("Text(RELEASE_PROFILE_GRADUATION,"))
        assertTrue(RELEASE_PROFILE_GRADUATION.startsWith("Leaves Developer when: "))
        assertTrue(RELEASE_PROFILE_GRADUATION.contains(" Then: "))
        val activity = source("MainActivity.kt")
        assertTrue(activity.contains("onOpenLiveTvSettings = { nav.navigate(\"live-tv-settings\") }"))
        assertTrue(activity.contains("composable(\"live-tv-settings\")"))
        assertTrue(activity.contains("LiveTvSettingsScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })"))
    }
}
