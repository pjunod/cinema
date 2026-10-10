package tv.plurx.app.livetv

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * Where Android draws the server's Live TV settings. The web moved the Live TV
 * enable switch and the tuner, guide, recording and Library channel cards from
 * Developer to Settings → Live TV; Paul's Developer rule is that a finished
 * setting does not wait in Developer. These tests pin both halves: the Live TV
 * screen draws the five cards and their saves, enable first, and the Developer
 * screen draws none of them and says what the one card it keeps is waiting on.
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
        LiveTvSettingsCard.EnableLiveTv to listOf("LiveTvSettingsChange.Enabled(", "refreshPrerequisites", "state.prerequisites?.let"),
        LiveTvSettingsCard.HdHomeRun to listOf("LiveTvSettingsChange.Configure(", "checkSavedConfiguration", "readiness?.checks"),
        LiveTvSettingsCard.ProgrammeGuide to listOf("refreshGuideReadiness", "guideReadiness?.let"),
        LiveTvSettingsCard.Recording to listOf("LiveTvSettingsChange.DvrEnabled(", "\"Enable recording\""),
        LiveTvSettingsCard.LibraryChannels to listOf("LiveTvSettingsChange.LibraryChannelsEnabled(", "\"Enable Library channels\""),
    )

    @Test
    fun liveTvSettingsOwnTheFiveCardsTheWebMovedInTheWebsOrder() {
        assertEquals(
            listOf("Enable Live TV", "HDHomeRun Live TV", "Programme guide", "Recording", "Library channels"),
            LiveTvSettingsPlacement.liveTvSettings.map { it.title },
        )
        // The switch is a finished setting now: its title no longer calls it
        // advisory enablement, and it is the first thing the screen draws.
        assertEquals(LiveTvSettingsCard.EnableLiveTv, LiveTvSettingsPlacement.liveTvSettings.first())
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
            listOf(LiveTvSettingsCard.DisplayCadence),
            LiveTvSettingsPlacement.developer,
        )
        val developer = source("livetv/LiveTvDeveloperScreen.kt")
        movedCalls.forEach { (card, calls) ->
            calls.forEach { call ->
                assertFalse("Developer must not draw ${card.title} ($call)", developer.contains(call))
            }
        }
        // The card that stays is the one whose evidence is still owed, and it
        // says what it waits on — the web's `devGraduation` line. The enable
        // switch graduated, so it is neither drawn here nor waiting here.
        assertFalse(developer.contains("EnableLiveTvCard"))
        assertTrue(developer.contains("LiveTvSettingsChange.DisplayModeMatch("))
        assertTrue(developer.contains("LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION"))
        assertTrue(LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION.startsWith("Leaves Developer when: "))
        assertTrue(LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION.contains(" Then: "))
        LiveTvSettingsPlacement.developer.forEach { card ->
            assertTrue(developer.contains("LiveTvSettingsCard.${card.name} -> ${drawnBy.getValue(card)}(state)"))
        }
        // Neither the guide nor the enable card is drawn here, so neither is
        // read here; Settings → Live TV draws both, so it reads both.
        assertTrue(developer.contains("rememberLiveTvAdminState(origin, LiveTvAdminSurface.Developer)"))
        assertFalse(LiveTvAdminSurface.Developer.readsGuide)
        assertFalse(LiveTvAdminSurface.Developer.readsEnablePrerequisites)
        assertTrue(LiveTvAdminSurface.LiveTvSettings.readsGuide)
        assertTrue(LiveTvAdminSurface.LiveTvSettings.readsEnablePrerequisites)
        // The Live TV screen's own copy sends nobody to Developer for the switch.
        assertFalse(source("livetv/LiveTvSettingsScreen.kt").contains("Settings → Developer"))
        assertFalse(LiveTvAdminSurface.LiveTvSettings.readyMessage.contains("Developer"))
        assertFalse(LiveTvAdminSurface.LiveTvSettings.needsAttentionMessage.contains("Developer"))
        assertEquals(
            "Live TV is disabled. An administrator can enable it in Settings → Live TV.",
            liveTvKnownMessage("live_tv_disabled"),
        )
    }

    /**
     * Paul's rule for an enable section: say what is needed to turn it on
     * safely and whether each part is met right now — advisory, never a gate.
     * The web's Enable Live TV card draws `/live-tv/readiness` beside its
     * toggle; so does the native Settings → Live TV card, and nothing it draws
     * reaches the button.
     */
    @Test
    fun theLiveTvEnableCardDrawsEveryPrerequisiteRowAndGatesNothing() {
        val screen = source("livetv/LiveTvSettingsScreen.kt")
        val card = cardBody(screen, "EnableLiveTvCard")
        // A graduated card carries no "Leaves Developer when" line.
        assertFalse(card.contains("GRADUATION"))
        assertFalse(card.contains("Leaves Developer"))
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
        assertTrue("Settings must have a Developer section", developer.length < settings.length)
        assertFalse("the Developer entry no longer promises HDHomeRun setup", developer.contains("HDHomeRun"))
        assertFalse("Enable Live TV graduated to Settings → Live TV", developer.contains("Enable Live TV"))
        // Prepared replacement graduated to Settings → Playback with its switch
        // and advisory rows, and no longer says what it waits on.
        assertFalse(developer.contains("PreparedReplacementEnable("))
        val playback = settings.substringAfter("SettingsSection(\n                \"Playback\",")
            .substringBefore("val appearance: @Composable () -> Unit")
        assertTrue("Settings must have a Playback section", playback.length < settings.length)
        assertTrue(playback.contains("PreparedReplacementEnable("))
        assertTrue(playback.contains("onEnabled = vm::setPreparedReplacement"))
        // Nothing left on Settings itself waits in Developer, so no entry here
        // carries a graduation line.
        assertFalse(settings.contains("_GRADUATION"))
        assertFalse(cardBody(settings, "PreparedReplacementEnable").contains("Leaves Developer"))
        // The Release startup profile status has no switch, so it is a build
        // fact in About rather than a Developer entry.
        assertFalse(developer.contains("Release startup profile"))
        val about = settings.substringAfter("SettingsSection(\"About\", null)")
        assertTrue("Settings must have an About section", about.length < settings.length)
        assertTrue(about.contains("Release startup profile"))
        assertTrue(about.contains("Text(profileStatus"))
        val activity = source("MainActivity.kt")
        assertTrue(activity.contains("onOpenLiveTvSettings = { nav.navigate(\"live-tv-settings\") }"))
        assertTrue(activity.contains("composable(\"live-tv-settings\")"))
        assertTrue(activity.contains("LiveTvSettingsScreen(origin = vm.origin, onBack = { nav.popBackStackFrom(entry) })"))
    }
}
