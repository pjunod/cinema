package tv.plurx.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.*
import org.junit.Test
import retrofit2.Retrofit
import retrofit2.converter.kotlinx.serialization.asConverterFactory
import okhttp3.MediaType.Companion.toMediaType

class SharedPlaybackModelsTest {
    private val ref = SharedPlaybackReference("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993", "9223372036854775807")
    private val file = "9007199254740993"
    private val base = "/api/v1/shared/imports/${ref.import_id}/files/${"L".repeat(236)}"
    private val revision = "a".repeat(64)
    private suspend fun fetch(locator: String?, mutate: (JsonObject) -> JsonObject = { it }, change: () -> Unit = {}): PlaybackFileContext {
        val item = Json.parseToJsonElement(Json.encodeToString(ref))
        val row = mutate(buildJsonObject {
            put("file_id", file); put("revision", revision)
            put("reference", buildJsonObject { put("item", item); put("file_id", file); put("revision", revision); put("lifecycle_generation", 1) })
            if (locator != null) put("file_base", locator)
        })
        val body = buildJsonObject { put("lifecycle_generation", 1); put("files", buildJsonArray { add(row) }) }.toString()
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            assertEquals("b.test", chain.request().url.host)
            assertEquals("Bearer fixture-bearer", chain.request().header("Authorization"))
            change()
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("OK")
                .body(body.toResponseBody()).build()
        }.build()
        return PlaybackFileContext.authenticatedDetailForTest(ref, file, client)
    }
    private fun binding() = buildJsonObject {
        put("item", Json.encodeToJsonElement(ref)); put("file_id", file); put("revision", revision); put("lifecycle_generation", 1)
    }
    private fun decision(change: (JsonObject) -> JsonObject = { it }): SharedDecision {
        val wire = buildJsonObject {
            put("file_id", file); put("reference", binding()); put("method", "remux")
            put("play_url", "$base/stream.mp4?audio=4095")
            put("delivery", buildJsonObject { put("mode", "remux"); put("url", "$base/stream.mp4?audio=4095"); put("sessions_url", "$base/hls/sessions") })
            put("vod_indexed", true); put("delivered_audio", buildJsonObject { put("codec", "aac"); put("channels", 6) })
            put("convert_dolby_vision", true); put("container", "mp4"); put("prior_kbps", 4294967295L)
            put("prefer_segmented", "high-bitrate"); put("source", buildJsonObject { put("dv_profile", 7); put("dv_el_present", true); put("frame_rate", "24000/1001") })
            put("future", buildJsonObject { put("exact_integer", Long.MAX_VALUE) })
        }
        return SharedDecision.decode(change(wire).toString())
    }
    @Test fun distinctDecisionPreservesWireWithoutAcquiringMediaAuthority(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        val result = decision().validated(context)
        assertEquals(file, result.fileId)
        assertEquals(4294967295L, result.wire.getValue("prior_kbps").jsonPrimitive.long)
        assertEquals(Long.MAX_VALUE, result.wire.getValue("future").jsonObject.getValue("exact_integer").jsonPrimitive.long)
        assertEquals("24000/1001", result.wire.getValue("source").jsonObject.getValue("frame_rate").jsonPrimitive.content)
        assertThrows(IllegalArgumentException::class.java) { context.translatedDeliveryPath(result.playUrl) }
        PlaybackSubject.Shared(context).validated()
        assertThrows(IllegalArgumentException::class.java) { PlaybackSubject.Local(ref.item_id, context).validated() }
        for (bad in listOf("https://a.test$base/direct", "/api/v1/files/7/direct", "$base/stream.mp4?audio=4096", "$base/stream.mp4?audio=01", "$base/stream.mp4?audio=1&audio=2", "$base/direct?session=44444444-4444-4444-8444-444444444444", "$base/direct?token=x", "$base/direct?")) {
            assertNotNull(runCatching { decision { JsonObject(it + ("play_url" to JsonPrimitive(bad))) }.validated(context) }.exceptionOrNull())
        }
    }
    @Test fun fullReferenceCollisionAndRetiredAccountFailClosed(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        for (key in listOf("import_id", "server_id", "catalogue_epoch", "library_id", "item_id")) {
            assertNotNull(runCatching { decision { wire ->
                val binding = wire.getValue("reference").jsonObject
                val item = binding.getValue("item").jsonObject
                val replacement = if (key in setOf("library_id", "item_id")) "7" else "55555555-5555-4555-8555-555555555555"
                JsonObject(wire + ("reference" to JsonObject(binding + ("item" to JsonObject(item + (key to JsonPrimitive(replacement)))))))
            }.validated(context) }.exceptionOrNull())
        }
        assertNotNull(runCatching { decision { JsonObject(it + ("file_id" to JsonPrimitive(9007199254740993L))) } }.exceptionOrNull())
        for (key in listOf("file_id", "revision")) {
            assertNotNull(runCatching { decision { wire ->
                val binding = wire.getValue("reference").jsonObject
                JsonObject(wire + ("reference" to JsonObject(binding + (key to JsonPrimitive(if (key == "file_id") "7" else "b".repeat(64))))))
            }.validated(context) }.exceptionOrNull())
        }
        assertNotNull(runCatching { decision { wire ->
            val binding = wire.getValue("reference").jsonObject
            JsonObject(wire + ("reference" to JsonObject(binding + ("file_id" to JsonPrimitive(9007199254740993L)))))
        } }.exceptionOrNull())
        val result = decision()
        Session.token = null; Session.token = "fixture-bearer"
        assertThrows(IllegalArgumentException::class.java) { result.validated(context) }
    }
    private fun manifest(change: (JsonObject) -> JsonObject = { it }): SharedPGSManifest {
        val wire = buildJsonObject {
            put("schema", 1); put("generation", revision); put("file_id", file); put("reference", binding())
            put("track_index", 2); put("kind", "pgs"); put("timebase", "source_ms"); put("duration_ms", 1000)
            put("cues", buildJsonArray { add(buildJsonObject {
                put("id", "cue"); put("start_ms", 0); put("end_ms", 500); put("canvas_width", 1920); put("canvas_height", 1080)
                put("objects", buildJsonArray { add(buildJsonObject {
                    put("image", "overlay/$revision/objects/${"b".repeat(64)}.png"); put("x", 0); put("y", 0); put("width", 100); put("height", 50)
                }) })
            }) })
        }
        return SharedPGSManifest.decode(change(wire).toString())
    }
    @Test fun sharedPgsIdentityTimingAndContainment(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        assertEquals(file, manifest().validated(context, 2).fileId)
        assertNotNull(runCatching { manifest { JsonObject(it + ("file_id" to JsonPrimitive(9007199254740993L))) } }.exceptionOrNull())
        assertThrows(IllegalArgumentException::class.java) { manifest().validated(context, 3) }
        for (image in listOf("https://a.test/object.png", "overlay/../objects/$revision.png", "overlay/${"c".repeat(64)}/objects/$revision.png")) {
            val invalid = manifest().let { it.copy(cues = it.cues.map { cue -> cue.copy(objects = cue.objects.map { obj -> obj.copy(image = image) }) }) }
            assertThrows(IllegalArgumentException::class.java) { invalid.validated(context, 2) }
        }
        val invalidTiming = manifest().let { it.copy(cues = it.cues.map { cue -> cue.copy(endMs = 1001) }) }
        assertThrows(IllegalArgumentException::class.java) { invalidTiming.validated(context, 2) }
        val invalidBounds = manifest().let { it.copy(cues = it.cues.map { cue -> cue.copy(objects = cue.objects.map { obj -> obj.copy(width = Int.MAX_VALUE) }) }) }
        assertThrows(IllegalArgumentException::class.java) { invalidBounds.validated(context, 2) }
    }
    @Test fun startRequiresAlreadyAdmittedExactBSessionAndClosedControl(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        val session = "44444444-4444-4444-8444-444444444444"
        val bound = context.withSession(session)
        val start = HlsStart(session_id = session, playlist_url = "/api/v1/hls/$session/master.m3u8?native=1&subtitle=2&diagnostic=video-only",
            control = tv.plurx.app.player.ControlBootstrap("plurx-playback-control-v1", "/api/v1/hls/$session/control", "55555555-5555-4555-8555-555555555555", 1, 1000, 5000))
        val wireStart = Json.encodeToJsonElement(start).jsonObject
        val sharedStart = SharedStart.decode(JsonObject(wireStart + mapOf("prior_kbps" to JsonPrimitive(4294967295L), "plan_notes" to buildJsonArray { add("retained") })).toString()).validated(bound)
        assertEquals(4294967295L, sharedStart.wire.getValue("prior_kbps").jsonPrimitive.long)
        assertEquals("retained", sharedStart.wire.getValue("plan_notes").jsonArray.single().jsonPrimitive.content)
        assertThrows(IllegalArgumentException::class.java) { SharedStartValidation.validated(start, context) }
        for (query in listOf("?token=x", "?native=1&native=0", "?subtitle=4096", "?diagnostic=upstream", "?native=%31", "?")) {
            assertThrows(IllegalArgumentException::class.java) { SharedStartValidation.validated(start.copy(playlist_url = "/api/v1/hls/$session/index.m3u8$query"), bound) }
        }
        assertThrows(IllegalArgumentException::class.java) { SharedStartValidation.validated(start.copy(control = start.control!!.copy(url = "/api/v1/hls/55555555-5555-4555-8555-555555555555/control")), bound) }
    }
}
