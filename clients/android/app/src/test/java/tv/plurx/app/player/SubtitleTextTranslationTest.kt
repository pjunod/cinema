package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.joinAll
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.encodeToString
import okhttp3.Headers.Companion.headersOf
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody
import okhttp3.ResponseBody.Companion.toResponseBody
import retrofit2.Response
import java.lang.reflect.Proxy
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
    private fun heldUse(newChoice: ((ProgressiveCaptions) -> Unit)?, expectedAdoptions: Int) = runBlocking {
        val released = CompletableDeferred<Unit>()
        val started = CompletableDeferred<Unit>()
        var optionReads = 0
        val unused = Proxy.newProxyInstance(PlurxApi::class.java.classLoader, arrayOf(PlurxApi::class.java)) { _, method, _ ->
            error("Unexpected caption API ${method.name}")
        } as PlurxApi
        fun response(value: String): Response<ResponseBody> = Response.success(value.toResponseBody("application/json".toMediaType()),
            headersOf("Content-Type", "application/json"))
        val api = object : PlurxApi by unused {
            override suspend fun subtitleTextOptions(id: Long): Response<ResponseBody> {
                if (++optionReads > 1) { started.complete(Unit); released.await() }
                return response(Net.json.encodeToString(options().copy(subtitles = listOf(options().subtitles.single().copy(index = 7)))))
            }
            override suspend fun subtitleTextJob(id: String) = response(Net.json.encodeToString(job()))
            override suspend fun captionOptions(id: Long): Response<ResponseBody> = awaitCancellation()
        }
        val captionIntent = ProgressiveCaptionIntent()
        val captions = ProgressiveCaptions({ api }, this, this, 41, captionIntent, { 1L }, { 0L }, { it },
            { error("No playback mutation belongs to a held text Use") }, { true })
        val translationScope = CoroutineScope(coroutineContext + Job(coroutineContext[Job]))
        val translation = SubtitleTextTranslation({ api }, translationScope, 41, intent, { true }, { captionIntent.selectionRevision })
        try {
        translation.loadOptions()
        withTimeout(1_000) { while (translation.job == null) yield() }
        val adopted = mutableListOf<Long>()
        translation.use { index -> adopted += index }
        started.await()
        newChoice?.invoke(captions)
        released.complete(Unit)
        withTimeout(1_000) { translationScope.coroutineContext[Job]!!.children.toList().joinAll() }
        assertEquals(if (expectedAdoptions == 0) emptyList<Long>() else listOf(7L), adopted)
        } finally { captions.release(); translation.release(); translationScope.cancel() }
    }
    @Test fun heldTextUseCannotOverrideNewerOffIntent() = heldUse({ it.off() }, 0)
    @Test fun heldTextUseCannotOverrideNewerOrdinaryCaptionIntent() = heldUse({ captions ->
        // Controller.switchSubtitle accepted an ordinary track: its common transition clears progressive intent.
        captions.off()
    }, 0)
    @Test fun heldTextUseCannotOverrideNewerProgressiveCaptionIntent() = heldUse({
        it.select(ProgressiveCaptionSelection("transcribe", "ja"))
    }, 0)
    @Test fun heldTextUseAdoptsStableCaptionAtItsRefreshedIndexWhenIntentIsUnchanged() = heldUse(null, 1)

}
