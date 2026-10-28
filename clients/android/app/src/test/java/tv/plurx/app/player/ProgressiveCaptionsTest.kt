package tv.plurx.app.player

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.decodeFromString
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
import okhttp3.Headers.Companion.headersOf
import okhttp3.ResponseBody
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.*
import org.junit.Test
import retrofit2.Response
import tv.plurx.app.data.*

class ProgressiveCaptionsTest {
    private val hash = "a".repeat(64)
    private val profile = CaptionProfile(hash, "whisper-small", "transcribe", null, listOf("ja", "en"))
    private fun options() = CaptionOptions(1, "41", "9007199254740993", "123", 120_000, 125,
        "source-clock-v1", listOf(CaptionAudio(1, 1, 9, CaptionCapability(true), "ja")), listOf(profile),
        CaptionCapability(true), CaptionPermissions(true, true, true, true), emptyList(), null, 1000)
    private fun binding() = captionBinding(options(), 1, ProgressiveCaptionSelection("transcribe", "ja"))
    private fun manifest() = CaptionManifest(1, hash, "41", "b".repeat(64), profile.profile_id, 1, "ja", "ja",
        "source-clock-v1", "2", "3", CaptionOperationView("operation", "2", "running"),
        CaptionSnapshot("lease", "2", 30_000), emptyList())
    private fun segment() = CaptionSegment(1, hash, "c".repeat(64), 0, 30_000,
        listOf(CaptionCue("d".repeat(64), 1000, 2000, "<literal> 日本語 & text")), CaptionProvenance(hash, "b".repeat(64), "1"))
    private fun verify(segment: CaptionSegment, manifest: CaptionManifest = manifest()): CaptionSegment {
        val bytes = Json.encodeToString(segment).encodeToByteArray()
        return verifiedCaptionSegment(bytes, CaptionSegmentRef(segment.segment_id, captionDigest(bytes), 0, 30_000), manifest, binding())
    }
    private fun refuses(block: () -> Unit) {
        try { block(); fail("Expected protocol refusal") } catch (_: RuntimeException) { }
    }

    @Test fun selectedAudioUsesServerOrdinalRatherThanGlobalFfprobeIndex() {
        assertEquals(1L, binding().audio.audio_ordinal)
        assertEquals(9L, binding().audio.source_index)
        refuses { captionBinding(options(), 9, ProgressiveCaptionSelection("transcribe", "ja")) }
        refuses { captionBinding(options(), null, ProgressiveCaptionSelection("transcribe", "ja")) }
    }
    @Test fun unverifiedClockOrLegacyAmbiguousAudioCannotStartGeneration() {
        refuses { captionBinding(options().copy(source_origin_ms = null), 1, ProgressiveCaptionSelection("transcribe", "ja")) }
        refuses { captionBinding(options().copy(audio_tracks = listOf(options().audio_tracks.single().copy(audio_ordinal = null))),
            1, ProgressiveCaptionSelection("transcribe", "ja")) }
        refuses { captionBinding(options().copy(audio_tracks = listOf(options().audio_tracks.single().copy(capability = CaptionCapability(false)))),
            1, ProgressiveCaptionSelection("transcribe", "ja")) }
    }
    @Test fun taskAndSpokenLanguageHaveSeparateOutputContracts() {
        assertEquals("ja", binding().outputLanguage)
        val translation = options().copy(models = listOf(profile.copy(task = "translate", output_language = "en")))
        assertEquals("en", captionBinding(translation, 1, ProgressiveCaptionSelection("translate", "ja")).outputLanguage)
        refuses { captionBinding(options(), 1, ProgressiveCaptionSelection("translate", "ja")) }
        refuses { captionBinding(options(), 1, ProgressiveCaptionSelection("transcribe", "unknown")) }
    }
    @Test fun decimalCountersRemainExactAboveJavascriptIntegerPrecision() {
        assertEquals("9007199254740994", nextCaptionCounter("9007199254740993"))
        listOf("01", "-1", "1.0", "1e3", "9223372036854775808").forEach { value -> refuses { captionCounter(value) } }
        refuses { nextCaptionCounter(Long.MAX_VALUE.toString()) }
    }
    @Test fun viewsCannotMoveBackwardOrChangeOperationIdentity() {
        val fence = CaptionViewFence()
        fence.accept("a", "9007199254740993", "9007199254740994")
        refuses { fence.accept("a", "9007199254740992", "9007199254740995") }
        refuses { fence.accept("a", "9007199254740994", "9007199254740993") }
        refuses { fence.accept("b", "9007199254740994", "9007199254740995") }
        fence.accept("a", "9007199254740994", "9007199254740995")
    }
    @Test fun stopLatchRequiresExplicitResumeAndSurvivesLateHeartbeat() {
        val intent = ProgressiveCaptionIntent()
        assertTrue(intent.acceptDemand("running"))
        intent.stopped = true
        assertFalse(intent.acceptDemand("running"))
        assertTrue(intent.stopped)
        assertFalse(intent.acceptDemand("stopped", explicitResume = true))
        assertTrue(intent.stopped)
        assertTrue(intent.acceptDemand("queued", explicitResume = true))
        assertFalse(intent.stopped)
    }
    @Test fun immutableSegmentRequiresExactBytesAndSourceProvenance() {
        assertEquals("<literal> 日本語 & text", verify(segment()).cues.single().text)
        val bytes = Json.encodeToString(segment()).encodeToByteArray()
        val ref = CaptionSegmentRef(segment().segment_id, captionDigest(bytes), 0, 30_000)
        refuses { verifiedCaptionSegment(bytes + byteArrayOf(32), ref, manifest(), binding()) }
        refuses { verify(segment().copy(provenance = segment().provenance.copy(source_sha256 = "e".repeat(64)))) }
        refuses { verify(segment().copy(provenance = segment().provenance.copy(operation_epoch = "3"))) }
        refuses { verify(segment().copy(cues = listOf(segment().cues.single().copy(start_ms = 30_000, end_ms = 31_000)))) }
    }
    @Test fun cueWindowUsesHalfOpenBoundariesAndEvictsOnSeek() {
        val window = CaptionWindow()
        window.replace(listOf(verify(segment())), 0, 120_000)
        assertEquals("", window.textAt(999))
        assertEquals("<literal> 日本語 & text", window.textAt(1000))
        assertEquals(2000L, window.nextBoundary(1000))
        assertEquals("", window.textAt(2000))
        window.replace(listOf(verify(segment())), 100_000, 120_000)
        assertTrue(window.cues.isEmpty())
        assertNull(window.nextBoundary(100_000))
    }
    @Test fun oversizedCueWindowsAreRefusedBeforeReplacingVisibleCues() {
        val window = CaptionWindow()
        window.replace(listOf(verify(segment())), 0, 120_000)
        val tooMany = segment().copy(cues = (0..1024).map { CaptionCue(it.toString(), 1000, 2000, "text") })
        refuses { window.replace(listOf(tooMany), 0, 120_000) }
        assertEquals(1, window.cues.size)
    }
    @Test fun responseReaderBoundsActualBodyEvenWithoutLength() = runBlocking {
        val response = Response.success<ResponseBody>(object : ResponseBody() {
            override fun contentType() = "application/json".toMediaType()
            override fun contentLength() = -1L
            override fun source() = okio.Buffer().writeUtf8("x".repeat(16_385))
        }, headersOf("Content-Type", "application/json"))
        try { captionBytes(response, 16_384); fail("Expected byte cap") } catch (_: IllegalArgumentException) { }
        assertArrayEquals("{}".encodeToByteArray(), captionBytes(Response.success("{}".toResponseBody("application/json".toMediaType()), headersOf("Content-Type", "application/json"))))
    }
    @Test fun legacySubtitleOriginDefaultsToEmbeddedAndDownloadedIsDistinct() {
        assertEquals("embedded", Net.json.decodeFromString<SubTrack>("""{"index":0,"codec":"subrip"}""").source)
        val downloaded = SubTrack(0, "subrip", language = "eng", source = "downloaded")
        val embedded = SubTrack(1, "subrip", language = "eng")
        assertNull(embeddedTextTrackIndex(0, listOf(downloaded, embedded), listOf("en")))
        assertEquals(0, embeddedTextTrackIndex(1, listOf(downloaded, embedded), listOf("en")))
        assertEquals("plurx-downloaded-0", downloadedCaptionId(0))
    }
    private fun partialOperation() = CaptionOperation(1, "operation", "7", "11", "partial", retryable = false,
        asset_id = hash, content_revision = "b".repeat(64), source_size = "9007199254740993", source_mtime = "123",
        audio_index = 1, task = "transcribe", spoken_language = "ja", output_language = "ja", profile_id = hash,
        source_check = CaptionSourceCheck("verified", "9007199254740993", "9007199254740993"), server_now_ms = 1_000,
        failed_ranges = listOf(CaptionFailedRange(2, "9007199254740993", "worker_lost", true, 1)))
    @Test fun partialOperationRetriesOnlyTheExactPublishedFailedRangeAndPreservesCues() {
        val operation = partialOperation()
        val target = captionRetryTarget(operation, operation.failed_ranges.single())
        val attempt = captionRetryAttempt(target, "request", 60_000, "playback", 17_000, 1.0, false)
        assertNull(attempt.operation)
        val body = Net.json.encodeToString(requireNotNull(attempt.range))
        assertTrue(body.contains("\"scope\":\"range\""))
        assertTrue(body.contains("\"core_index\":2"))
        assertTrue(body.contains("\"expected_range_revision\":\"9007199254740993\""))
        assertTrue(body.contains("\"expected_operation_state\":\"partial\""))
        assertFalse(body.contains("expected_view_revision"))
        assertFalse(body.contains("\"expected_state\""))
        val window = CaptionWindow()
        window.replace(listOf(verify(segment())), 0, 120_000)
        assertTrue(captionRetryTargetCurrent(target, operation))
        assertEquals("<literal> 日本語 & text", window.textAt(1_000))
    }
    @Test fun staleFailedRangeTargetCannotRedirectToANewRevisionRangeOrOperation() {
        val operation = partialOperation()
        val target = captionRetryTarget(operation, operation.failed_ranges.single())
        assertFalse(captionRetryTargetCurrent(target, operation.copy(view_revision = "12")))
        assertFalse(captionRetryTargetCurrent(target, operation.copy(operation_epoch = "8")))
        assertFalse(captionRetryTargetCurrent(target, operation.copy(operation_id = "other")))
        assertFalse(captionRetryTargetCurrent(target, operation.copy(failed_ranges = listOf(operation.failed_ranges.single().copy(range_revision = "9007199254740994")))))
        assertFalse(captionRetryTargetCurrent(target, operation.copy(failed_ranges = listOf(operation.failed_ranges.single().copy(core_index = 3)))))
        refuses { captionRetryTarget(operation) }
        refuses { captionRetryTarget(operation, operation.failed_ranges.single().copy(retryable = false)) }
    }
    @Test fun operationResumeRemainsDistinctFromRangeRetryAndFailurePagesAreBounded() {
        val operation = partialOperation().copy(state = "stopped")
        val attempt = captionRetryAttempt(captionRetryTarget(operation), "request", 60_000, "playback", 0, 1.0, true)
        assertNull(attempt.range)
        assertEquals("operation", attempt.operation?.scope)
        assertEquals("11", attempt.operation?.expected_view_revision)
        captionValidateFailures(operation.failed_ranges, "cursor")
        refuses { captionValidateFailures(operation.failed_ranges + operation.failed_ranges, null) }
        refuses { captionValidateFailures(listOf(operation.failed_ranges.single().copy(core_index = 2048)), null) }
        refuses { captionValidateFailures(listOf(operation.failed_ranges.single().copy(range_revision = "01")), null) }
    }

    @Test fun lostDemandRenewalReplaysExactIdentityAndLateCompletionCannotClearItsSuccessor() {
        val pending = CaptionDemandAttempt()
        val receipt = CaptionReceipt(1, "accepted", "operation", "7", "11", "running", demand_id = "demand",
            demand_generation = "9007199254740993", server_now_ms = 1_000)
        var ids = 0
        fun request(value: CaptionReceipt = receipt) = pending.request(value, "7") {
            CaptionDemand(1, "request-${++ids}", 60_000, "7", nextCaptionCounter(requireNotNull(value.demand_generation)),
                "playback", 1_000, 1.0, false)
        }
        val first = request()
        pending.finish(first, ambiguous = true)
        assertSame(first, request())
        assertEquals(1, ids)
        assertEquals("9007199254740994", first.demand_generation)
        val newer = request(receipt.copy(demand_id = "replacement"))
        assertNotSame(first, newer)
        pending.finish(first, ambiguous = false)
        assertSame(newer, request(receipt.copy(demand_id = "replacement")))
        pending.finish(newer, ambiguous = false)
        assertNotSame(newer, request(receipt.copy(demand_id = "replacement")))
    }

}
