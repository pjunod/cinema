package tv.plurx.app.data

import java.io.IOException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.Request
import okhttp3.ResponseBody.Companion.toResponseBody
import okhttp3.MediaType.Companion.toMediaType
import okio.Buffer
import org.junit.Assert.*
import org.junit.Test

class SharedDecisionClientTest {
    private val reference = SharedPlaybackReference("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333", "9007199254740993", "9223372036854775807")
    private val base = "/api/v1/shared/imports/${reference.import_id}/files/${"L".repeat(236)}"
    private val revision = "a".repeat(64)
    private fun login() { Session.origin = "https://b.test"; Session.token = "decision-bearer" }
    private fun binding(file: String, lifecycle: JsonElement = JsonPrimitive(Long.MAX_VALUE)) = buildJsonObject {
        put("item", Json.encodeToJsonElement(reference)); put("file_id", file); put("revision", revision); put("lifecycle_generation", lifecycle)
    }
    private suspend fun context(file: String = "0", change: (JsonObject) -> JsonObject = { it }): PlaybackFileContext {
        val detail = change(buildJsonObject { put("lifecycle_generation", Long.MAX_VALUE); put("files", buildJsonArray { add(buildJsonObject {
            put("file_id", file); put("revision", revision); put("file_base", base); put("reference", binding(file))
        }) }) }).toString()
        val transport = OkHttpClient.Builder().addInterceptor { response(it.request(), detail) }.build()
        return PlaybackFileContext.authenticatedDetailForTest(reference, file, transport)
    }
    private fun response(request: Request, text: String, status: Int = 200): Response = Response.Builder().request(request).protocol(Protocol.HTTP_1_1)
        .code(status).message("fixture").body(text.toResponseBody("application/json".toMediaType())).build()
    private fun wire(file: String = "0", change: (JsonObject) -> JsonObject = { it }): String = change(buildJsonObject {
        put("file_id", file); put("reference", binding(file)); put("method", "remux"); put("play_url", "$base/stream.mp4?audio=2")
        put("delivery", buildJsonObject { put("mode", "remux"); put("url", "$base/stream.mp4?audio=2"); put("sessions_url", "$base/hls/sessions"); put("audio", 2); put("requires_hls", true); put("preserve_dolby_vision", true) })
        put("source", buildJsonObject { put("hdr", "dolby_vision"); put("container", "mkv"); put("width", 3840) })
        put("audio", buildJsonArray { add(buildJsonObject { put("index", 2); put("codec", "aac"); put("channels", 6) }) })
        put("subtitles", buildJsonArray { add(buildJsonObject { put("index", 4); put("codec", "hdmv_pgs_subtitle") }) })
        put("selection", buildJsonObject { put("audio_index", 2); put("subtitle_index", -1) })
        put("audio_offset_ms", -500); put("delivered_dynamic_range", "dolby_vision"); put("delivered_dolby_vision_profile", 8)
        put("quality_candidate_id", "candidate"); put("future", buildJsonObject { put("exact", Long.MAX_VALUE) })
    }).toString()
    private fun caps() = capsDocument(videoCodecCaps(listOf(VideoDecoderLimit("h264", 2160), VideoDecoderLimit("hevc", 2160))),
        listOf("aac"), setOf(HdrType.DOLBY_VISION), listOf(8), ClientInfo("android", "fixture", "existing pure capability builder"))
    @Test fun existingV2BuilderExactSourceStringsAndTypedNegotiation(): Unit = runBlocking {
        login()
        for (file in listOf("0", "9007199254740993", Long.MAX_VALUE.toString())) {
            val context = context(file); var body: JsonObject? = null
            val originalAudio = mutableListOf("aac"); val document = caps().copy(audio = originalAudio)
            val transport = OkHttpClient.Builder().addInterceptor { chain ->
                val request = chain.request(); assertEquals("POST", request.method); assertEquals("b.test", request.url.host); assertEquals("$base/decision", request.url.encodedPath)
                assertEquals("Bearer decision-bearer", request.header("Authorization")); assertEquals("-1", request.url.queryParameter("subtitle"))
                val buffer = Buffer(); request.body!!.writeTo(buffer); body = Json.parseToJsonElement(buffer.readUtf8()).jsonObject
                originalAudio.clear()
                response(request, wire(file))
            }.build()
            val result = SharedDecisionClient.forTest(transport).decisionForTest(context, document, mapOf("audio" to "2", "subtitle" to "-1", "force" to "original", "audio_offset_ms" to "-500"))
            assertEquals(Json.encodeToJsonElement(result.caps), body!!.getValue("caps")); assertEquals(2, result.caps.v); assertEquals(listOf("aac"), result.caps.audio)
            assertEquals(file, result.decision.fileId); assertEquals(Long.MAX_VALUE, context.lifecycleGeneration)
            val fields = result.decision.presentation
            assertEquals(2L, fields.delivery!!.audio); assertTrue(fields.delivery.requires_hls); assertTrue(fields.delivery.preserve_dolby_vision)
            assertEquals(6, fields.audio.first().channels); assertEquals(4L, fields.subtitles.first().index); assertEquals(8, fields.delivered_dolby_vision_profile); assertEquals(-500L, fields.audio_offset_ms)
            assertEquals(Long.MAX_VALUE, result.decision.wire.getValue("future").jsonObject.getValue("exact").jsonPrimitive.long)
            assertNotNull(runCatching { context.path("direct") }.exceptionOrNull())
        }
    }
    @Test fun lifecycleAndExactLocatorAreRequiredWithoutLocalFallback(): Unit = runBlocking {
        login()
        for (invalid in listOf(JsonPrimitive(0), JsonPrimitive(-1), JsonPrimitive(1.5), JsonPrimitive("1"), JsonPrimitive(true), JsonNull)) {
            assertNotNull(runCatching { context { JsonObject(it + ("lifecycle_generation" to invalid)) } }.exceptionOrNull())
        }
        assertNotNull(runCatching { context { JsonObject(it - "lifecycle_generation") } }.exceptionOrNull())
        assertNotNull(runCatching { context { detail ->
            val row = detail.getValue("files").jsonArray.single().jsonObject
            val binding = row.getValue("reference").jsonObject
            val source = JsonObject(binding.getValue("item").jsonObject + ("library_id" to JsonPrimitive(9007199254740993L)))
            val replacement = JsonObject(row + ("reference" to JsonObject(binding + ("item" to source))))
            JsonObject(detail + ("files" to JsonArray(listOf(replacement))))
        } }.exceptionOrNull())
        for (bad in listOf("/api/v1/files/0", base + "A", base.dropLast(1), "https://a.test$base")) {
            assertNotNull(runCatching { context { detail ->
                val row = detail.getValue("files").jsonArray.single().jsonObject
                JsonObject(detail + ("files" to JsonArray(listOf(JsonObject(row + ("file_base" to JsonPrimitive(bad)))))))
            } }.exceptionOrNull())
        }
        assertNotNull(runCatching { context { detail ->
            val row = detail.getValue("files").jsonArray.single().jsonObject
            val replacement = JsonObject(row + ("reference" to binding("0", JsonPrimitive(1))))
            JsonObject(detail + ("files" to JsonArray(listOf(replacement))))
        } }.exceptionOrNull())
    }
    @Test fun boundedBodiesForeignResponsesAndRefusalsNeverRetry(): Unit = runBlocking {
        login(); val context = context()
        for (size in listOf(4_194_304, 4_194_305)) {
            val original = wire { JsonObject(it + ("large_future" to JsonPrimitive("x".repeat(1_048_577)))) }; val text = original + " ".repeat(size - original.toByteArray().size)
            val transport = OkHttpClient.Builder().addInterceptor { response(it.request(), text) }.build()
            val result = runCatching { SharedDecisionClient.forTest(transport).decisionForTest(context, caps()) }
            if (size == 4_194_304) assertNotNull(result.getOrThrow().decision.wire["large_future"]) else assertTrue(result.isFailure)
        }
        for (status in listOf(302, 401, 403, 409, 500)) {
            var calls = 0; val transport = OkHttpClient.Builder().addInterceptor { calls++; response(it.request(), "", status).newBuilder().header("Location", "https://a.test").build() }.build()
            assertTrue(runCatching { SharedDecisionClient.forTest(transport).decisionForTest(context, caps()) }.isFailure); assertEquals(1, calls)
        }
        val transport = OkHttpClient.Builder().addInterceptor { response(it.request().newBuilder().url("https://a.test$base/decision").build(), wire()) }.build()
        assertTrue(runCatching { SharedDecisionClient.forTest(transport).decisionForTest(context, caps()) }.isFailure)
        for (key in listOf("file_id", "revision", "lifecycle_generation")) {
            val text = wire { val binding = it.getValue("reference").jsonObject; JsonObject(it + ("reference" to JsonObject(binding + (key to if (key == "lifecycle_generation") JsonPrimitive(1) else JsonPrimitive("7"))))) }
            val client = OkHttpClient.Builder().addInterceptor { response(it.request(), text) }.build()
            assertTrue(runCatching { SharedDecisionClient.forTest(client).decisionForTest(context, caps()) }.isFailure)
        }
    }
    @Test fun actualBlockedCallCancelsOnAccountChangeAndCoroutineCancellation(): Unit = runBlocking {
        for (accountChange in listOf(true, false)) {
            login(); val context = context(); val began = CompletableDeferred<Unit>(); val stopped = CompletableDeferred<Unit>()
            val client = OkHttpClient.Builder().addInterceptor { chain ->
                began.complete(Unit)
                while (!chain.call().isCanceled()) Thread.sleep(2)
                stopped.complete(Unit); throw IOException("actual call cancelled")
            }.build()
            val task = async { runCatching { SharedDecisionClient.forTest(client).decisionForTest(context, caps()) } }
            withTimeout(3000) { began.await() }
            if (accountChange) { Session.origin = "https://new.test"; Session.token = "new-bearer" } else task.cancel()
            withTimeout(3000) { stopped.await() }; assertTrue(runCatching { task.await().getOrThrow() }.isFailure)
            if (accountChange) assertEquals("new-bearer", Session.token)
        }
    }
    @Test fun unapprovedQueryAndOldContextCannotIssueRequest(): Unit = runBlocking {
        login(); val context = context(); var calls = 0
        val transport = OkHttpClient.Builder().addInterceptor { calls++; response(it.request(), wire()) }.build(); val client = SharedDecisionClient.forTest(transport)
        for (query in listOf(mapOf("token" to "x"), mapOf("audio" to "01"), mapOf("subtitle" to "-2"), mapOf("audio_offset_ms" to "15001"))) {
            assertTrue(runCatching { client.decisionForTest(context, caps(), query) }.isFailure)
        }
        assertTrue(runCatching { client.decisionForTest(context, caps().copy(v = 1)) }.isFailure)
        assertTrue(runCatching { client.decisionForTest(context, caps().copy(client = caps().client.copy(ua = "x".repeat(131_073)))) }.isFailure)
        Session.token = "replaced"; assertTrue(runCatching { SharedDecisionClient.forTest(transport).decisionForTest(context, caps()) }.isFailure)
        assertEquals(0, calls)
    }
    // Synthetic complete B response through the actual authenticated client;
    // this does not qualify a physical Source producer or device playback.
    @Test fun initialStartRetainsWholeRequestAndBoundBContext(): Unit = runBlocking {
        login(); val context = context("9223372036854775807"); val session = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        val reply = buildJsonObject {
            put("session_id", session); put("playlist_url", "/api/v1/hls/$session/master.m3u8?native=1&subtitle=2"); put("vod", true); put("start_seconds", 0); put("duration_ms", 90_000)
            put("control", buildJsonObject { put("protocol", "plurx-playback-control-v1"); put("url", "/api/v1/hls/$session/control"); put("generation", "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"); put("control_epoch", 1); put("next_exchange_ms", 5_000); put("lease_timeout_ms", 300_000) })
            put("future", buildJsonObject { put("exact", Long.MAX_VALUE) })
        }.toString()
        var sent: String? = null
        val transport = OkHttpClient.Builder().addInterceptor { chain ->
            assertEquals("$base/hls/sessions", chain.request().url.encodedPath); assertEquals("Bearer decision-bearer", chain.request().header("Authorization"))
            val buffer = Buffer(); chain.request().body!!.writeTo(buffer); sent = buffer.readUtf8(); response(chain.request(), reply)
        }.build()
        val request = CreateSessionReq(playback_id = "shared-browser", request_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc", start = 12.5, height = 720, copy = true, caps = caps())
        val result = SharedDecisionClient.forTest(transport).start(context, request)
        assertEquals("9223372036854775807", result.context.sourceFileId); assertEquals(reference, result.context.reference); assertEquals(session, result.context.sessionId)
        assertEquals(request, result.request); assertEquals(Net.json.encodeToJsonElement(request).jsonObject, Json.parseToJsonElement(sent!!).jsonObject)
        assertEquals(Long.MAX_VALUE, result.start.wire["future"]!!.jsonObject["exact"]!!.jsonPrimitive.long)
        assertTrue(runCatching { result.context.localId() }.isFailure)
    }
    @Test fun initialStartRefusesUnsupportedFieldsBeforeNetwork(): Unit = runBlocking {
        login(); val context = context(); var calls = 0
        val client = SharedDecisionClient.forTest(OkHttpClient.Builder().addInterceptor { calls++; response(it.request(), "{}") }.build())
        val base = CreateSessionReq(playback_id = "shared", request_id = "cccccccc-cccc-4ccc-8ccc-cccccccccccc", caps = caps())
        for (request in listOf(base.copy(previous_session_id = ""), base.copy(control_sequence = 0), base.copy(reopen_reason = ReopenReason.Stall), base.copy(subtitle_burn = 0), base.copy(preserve_dolby_vision = true), base.copy(request_id = base.request_id!!.uppercase()))) {
            assertTrue(runCatching { client.start(context, request) }.isFailure)
        }
        assertEquals(0, calls)
    }

}
