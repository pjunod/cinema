package tv.plurx.app.player

import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.*

class SubtitleTextTranslationTest {
    private val intent = SubtitleTextTranslationIntent().also {
        it.jobId = "job"; it.sourceSize = "9007199254740993"; it.sourceMtime = "17"
    }
    private fun options() = SubtitleTextOptions(1, "41", "9007199254740993", "17", listOf(SubtitleTextSource(3, "en", text_eligible = true, downloaded_subtitle_id = "a".repeat(64))),
        SubtitleTextConfig(1, "2", false, "opus-mt-mul-en", "en"), "opus-mt-mul-en", emptyList(),
        SubtitleTextPermissions(true, true), 1000)
    private fun job() = SubtitleTextJob(1, "job", "succeeded", "complete", null, false,
        SubtitleTextProgress(100, 100), SubtitleTextResult("41", 3, "a".repeat(64), "en"))
    private fun refuses(block: () -> Unit) {
        try { block(); fail("Expected refused result") } catch (_: IllegalArgumentException) { }
    }
    @Test fun translatedTextRequiresExplicitUseAgainstTheSameSourceRevision() {
        assertEquals(3L, subtitleTextResultForUse(job(), 41, options(), intent))
        refuses { subtitleTextResultForUse(job(), 41, options().copy(source_size = "9007199254740992"), intent) }
        refuses { subtitleTextResultForUse(job(), 41, options().copy(source_mtime = "18"), intent) }
        refuses { subtitleTextResultForUse(job(), 42, options(), intent) }
    }
    @Test fun onlySuccessfulEnglishCaptionResultsCanBecomeOrdinaryTrackSelections() {
        refuses { subtitleTextResultForUse(job().copy(state = "running"), 41, options(), intent) }
        refuses { subtitleTextResultForUse(job().copy(job_id = "other"), 41, options(), intent) }
        refuses { subtitleTextResultForUse(job().copy(result = job().result!!.copy(language = "ja")), 41, options(), intent) }
        refuses { subtitleTextResultForUse(job().copy(result = job().result!!.copy(subtitle_index = -1)), 41, options(), intent) }
    }
    @Test fun stableCaptionIdentitySurvivesOrdinalChangesAndRejectsReplacementAtTheSameIndex() {
        val moved = options().copy(subtitles = listOf(options().subtitles.single().copy(index = 7)))
        assertEquals(7L, subtitleTextResultForUse(job(), 41, moved, intent))
        refuses { subtitleTextResultForUse(job(), 41, options().copy(subtitles = listOf(
            options().subtitles.single().copy(downloaded_subtitle_id = "b".repeat(64)))), intent) }
    }
    @Test fun textTranslationRequestHasIndependentModelAndNoSpeechTask() {
        val body = Json.encodeToString(SubtitleTextRequest(1, "request", "9007199254740993", "17", "ja", "en", "opus-mt-mul-en"))
        assertTrue(body.contains("opus-mt-mul-en"))
        assertTrue(body.contains("9007199254740993"))
        assertFalse(body.contains("whisper"))
        assertFalse(body.contains("audio_index"))
        assertFalse(body.contains("task"))
    }
}
