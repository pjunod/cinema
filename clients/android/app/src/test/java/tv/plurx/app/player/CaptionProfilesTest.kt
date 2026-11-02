package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.CaptionProfile
import tv.plurx.app.data.CaptionSettings

class CaptionProfilesTest {
    private val baseTranslate = CaptionProfile("a".repeat(64), "base", "translate", "en", listOf("de"), default = true)
    private val baseTranscribe = CaptionProfile("b".repeat(64), "base", "transcribe", null, listOf("de"), default = true)
    private val smallTranslate = CaptionProfile("c".repeat(64), "small", "translate", "en", listOf("de"), default = false)
    private val smallTranscribe = CaptionProfile("d".repeat(64), "small", "transcribe", null, listOf("de"), default = false)
    private val profiles = listOf(baseTranslate, baseTranscribe, smallTranslate, smallTranscribe)
    @Test fun baseDefaultMapsToTheRequestedTaskWhileAnExplicitSmallTraversalWins() {
        assertEquals(baseTranslate, captionPreferredProfile(profiles, baseTranslate.profile_id, "translate"))
        assertEquals(baseTranscribe, captionPreferredProfile(profiles, baseTranslate.profile_id, "transcribe"))
        assertEquals(smallTranslate, captionPreferredProfile(profiles, baseTranslate.profile_id, "translate", smallTranslate.profile_id))
        assertEquals(smallTranscribe, captionPreferredProfile(profiles, baseTranslate.profile_id, "transcribe", smallTranslate.profile_id))
    }
    @Test fun savedSmallWithNoTaskProfileNeverFallsBackToFirstBaseRow() {
        val settings = CaptionSettings(1, "1", true, "small", null, null, 1_000)
        assertEquals(smallTranslate, captionSavedProfile(settings, profiles, smallTranslate.profile_id))
        assertEquals(smallTranslate, captionSavedProfile(settings, profiles, null))
        assertEquals(smallTranscribe, captionSavedProfile(settings.copy(profile_id = smallTranscribe.profile_id), profiles, smallTranscribe.profile_id))
    }
    @Test fun unavailableExplicitConfigurationRemainsUnavailableAndUnknownHintsAreRefused() {
        val invalid = profiles.map { it.copy(default = false) }
        assertNull(captionPreferredProfile(invalid, null, "translate"))
        assertNull(captionSavedProfile(CaptionSettings(1, "1", true, "missing", null, null, 1_000), invalid, null))
        assertNull(captionPreferredProfile(profiles, baseTranslate.profile_id, "translate", "e".repeat(64)))
        try {
            captionPreferredProfile(profiles, "e".repeat(64), "translate")
            fail("A hint must name an advertised registered profile")
        } catch (_: IllegalArgumentException) { }
    }
}
