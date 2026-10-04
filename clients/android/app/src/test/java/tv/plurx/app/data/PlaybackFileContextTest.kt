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

class PlaybackFileContextTest {
    private val ref = SharedPlaybackReference("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993", "9223372036854775807")
    private val file = "9007199254740993"
    private val base = "/api/v1/shared/imports/${ref.import_id}/files/${"L".repeat(236)}"
    private val revision = "a".repeat(64)
    private suspend fun fetch(locator: String?, mutate: (JsonObject) -> JsonObject = { it }, detailBody: (String) -> String = { it }, unknownLength: Boolean = false, change: () -> Unit = {}): PlaybackFileContext {
        val item = Json.parseToJsonElement(Json.encodeToString(ref))
        val row = mutate(buildJsonObject {
            put("file_id", file); put("revision", revision)
            put("reference", buildJsonObject { put("item", item); put("file_id", file); put("revision", revision); put("lifecycle_generation", 1) })
            if (locator != null) put("file_base", locator)
        })
        val body = detailBody(buildJsonObject { put("lifecycle_generation", 1); put("files", buildJsonArray { add(row) }) }.toString())
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            assertEquals("b.test", chain.request().url.host)
            assertEquals("Bearer fixture-bearer", chain.request().header("Authorization"))
            change()
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("OK")
                .body(if (unknownLength) object : okhttp3.ResponseBody() {
                    override fun contentType(): okhttp3.MediaType? = null
                    override fun contentLength(): Long = -1
                    override fun source(): okio.BufferedSource = okio.Buffer().writeUtf8(body)
                } else body.toResponseBody()).build()
        }.build()
        return PlaybackFileContext.authenticatedDetailForTest(ref, file, client)
    }
    @Test fun authenticatedDetailHonorsFourMiBBoundWithoutGrantingMissingLocator(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val largeDetail: (String) -> String = { text ->
            val template = Json.parseToJsonElement(text).jsonObject.getValue("files").jsonArray[0].jsonObject
            val chapters = buildJsonArray {
                repeat(1024) { index -> add(buildJsonObject {
                    put("index", index); put("title", "c".repeat(512)); put("start_ms", index * 1000); put("end_ms", (index + 1) * 1000)
                }) }
            }
            val rows = buildJsonArray {
                repeat(3) { index ->
                    val id = if (index == 0) file else index.toString()
                    add(JsonObject(template + mapOf(
                        "file_id" to JsonPrimitive(id), "size" to JsonPrimitive("1048576"), "chapters" to chapters,
                        "audio_streams" to JsonArray(emptyList()), "subtitle_streams" to JsonArray(emptyList()),
                        "skip_regions" to JsonArray(emptyList()), "dolby_vision" to JsonObject(emptyMap()),
                        "audio_offset_ms" to JsonPrimitive(0), "probed" to JsonPrimitive(true),
                        "reference" to JsonObject(template.getValue("reference").jsonObject + ("file_id" to JsonPrimitive(id)))
                    )))
                }
            }
            val result = buildJsonObject { put("lifecycle_generation", 1); put("files", rows) }.toString()
            assertTrue(result.toByteArray().size > 1_048_576); assertTrue(result.toByteArray().size < 4_194_304)
            // Valid JSON whitespace tests the exact byte limit without adding synthetic wire fields.
            result + " ".repeat(4_194_304 - result.toByteArray().size)
        }
        val context = fetch(base, detailBody = largeDetail, unknownLength = true)
        assertEquals(ref, context.reference); assertEquals(file, context.sourceFileId); assertNull(context.sessionId)
        assertThrows(IllegalArgumentException::class.java) { context.path("direct") }
        assertNotNull(runCatching { fetch(base, detailBody = { largeDetail(it) + " " }) }.exceptionOrNull())
        assertNotNull(runCatching { fetch(base, detailBody = { largeDetail(it) + " " }, unknownLength = true) }.exceptionOrNull())
        assertNotNull(runCatching { fetch(null, detailBody = largeDetail) }.exceptionOrNull())
    }
    @Test fun authenticatedBContextPreservesExactReferenceAndSessionAuthority(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        assertEquals(ref, context.reference); assertEquals(file, context.sourceFileId)
        assertNotEquals(PlaybackFileContext.local(file).sourceKey, context.sourceKey)
        assertEquals("$base/decision", context.path("decision"))
        assertThrows(IllegalArgumentException::class.java) { context.path("direct") }
        val bound = context.withSession("44444444-4444-4444-8444-444444444444")
        assertEquals("$base/direct?session=44444444-4444-4444-8444-444444444444", bound.path("direct"))
        assertNull(context.sessionId)
        assertThrows(IllegalArgumentException::class.java) { context.withSession("44444444-4444-1444-8444-444444444444") }
        Session.token = "fixture-bearer"
        assertEquals("$base/decision", context.path("decision"))
        Session.token = null; Session.token = "fixture-bearer"
        assertThrows(IllegalArgumentException::class.java) { context.path("decision") }
    }
    @Test fun missingForeignMalformedLocatorAndAuthorizationChangeFailClosed(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        for (candidate in listOf(null, "/api/v1/files/7", "https://a.test$base", "$base?token=x", "$base/..", "$base%2f", "$base#fragment", "$base\n")) {
            val failure = runCatching { fetch(candidate) }.exceptionOrNull()
            assertNotNull("invalid locator $candidate", failure)
        }
        assertNotNull(runCatching { fetch(base) { Session.token = "other-account" } }.exceptionOrNull())
    }
    @Test fun exactReferenceFileAndRevisionMustMatchAuthenticatedDetail(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val changes: List<(JsonObject) -> JsonObject> = listOf(
            { JsonObject(it + ("file_id" to JsonPrimitive(9_007_199_254_740_993L))) },
            { JsonObject(it + ("revision" to JsonPrimitive("malformed"))) },
            { JsonObject(it + ("reference" to JsonObject(it.getValue("reference").jsonObject + ("file_id" to JsonPrimitive("7"))))) },
            { row ->
                val binding = row.getValue("reference").jsonObject
                val item = JsonObject(binding.getValue("item").jsonObject + ("server_id" to JsonPrimitive("55555555-5555-4555-8555-555555555555")))
                JsonObject(row + ("reference" to JsonObject(binding + ("item" to item))))
            },
        )
        for (change in changes) assertNotNull(runCatching { fetch(base, mutate = change) }.exceptionOrNull())
    }
    @Test fun sharedFullQueryVocabularyAndTranslatedMediaCannotAcquireAuthority(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val context = fetch(base)
        assertThrows(IllegalArgumentException::class.java) { context.localId(7) }
        val bound = context.withSession("44444444-4444-4444-8444-444444444444")
        val fields = linkedMapOf("client" to "android", "device" to "Native client", "profile" to "android-directplay-any",
            "vcodec" to "hevc,h264", "vmaxheight" to "hevc:2160,h264:1080", "acodec" to "aac,eac3", "container" to "mkv,mp4",
            "maxheight" to "2160", "hdr" to "1", "dv" to "1", "dvhls" to "1", "hdr10t" to "1", "dvprofile" to "5,8",
            "start" to "12.5", "audio" to "2", "audio_offset_ms" to "-100")
        val path = bound.path("stream.mp4", fields)
        val url = okhttp3.HttpUrl.Builder().scheme("https").host("b.test").encodedPath(path.substringBefore('?'))
            .encodedQuery(path.substringAfter('?')).build()
        for ((key, value) in fields) assertEquals(value, url.queryParameter(key))
        assertEquals(path, bound.translatedDeliveryPath(path))
        for (value in listOf("https://a.test$path", "/api/v1/files/7/direct", "$base/direct?token=x", "$base/direct?session=55555555-5555-4555-8555-555555555555", "$base/stream.mp4?achannels=6", "$base/stream.mp4?audio=1&audio=2")) {
            assertThrows(IllegalArgumentException::class.java) { bound.translatedDeliveryPath(value) }
        }
        assertThrows(IllegalArgumentException::class.java) { bound.path("decision", mapOf("upstreamURL" to "https://a.test")) }
    }
    @Test fun actualLocalRetrofitCallersPreserveExactIdsAndRejectSharedFallback(): Unit = runBlocking {
        Session.origin = "https://b.test"; Session.token = "fixture-bearer"
        val shared = fetch(base)
        val requests = mutableListOf<okhttp3.Request>()
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            requests += chain.request()
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(503).message("unavailable")
                .body("{}".toResponseBody()).build()
        }.build()
        val api = Retrofit.Builder().baseUrl("https://b.test/api/v1/").client(client)
            .addConverterFactory(Net.json.asConverterFactory("application/json".toMediaType())).build().create(PlurxApi::class.java)
        val id = 9_007_199_254_740_993L
        runCatching { api.decision(id, mapOf("container" to "mkv", "dvhls" to "1")) }
        assertEquals("/api/v1/files/9007199254740993/decision", requests.last().url.encodedPath)
        assertEquals("1", requests.last().url.queryParameter("dvhls"))
        api.pgsOverlayManifest(id, 2)
        assertEquals("/api/v1/files/9007199254740993/subs/2/overlay.json", requests.last().url.encodedPath)
        api.pgsOverlayObject(id, 2, revision, revision)
        assertEquals("/api/v1/files/9007199254740993/subs/2/overlay/$revision/objects/$revision.png", requests.last().url.encodedPath)
        runCatching { api.createHlsSession(id, CreateSessionReq(playback_id = "44444444-4444-4444-8444-444444444444")) }
        assertEquals("/api/v1/files/9007199254740993/hls/sessions", requests.last().url.encodedPath)
        val count = requests.size
        assertNotNull(runCatching { api.decisionForContext(shared, emptyMap()) }.exceptionOrNull())
        assertNotNull(runCatching { api.pgsOverlayManifestForContext(shared, 2) }.exceptionOrNull())
        assertEquals(count, requests.size)
    }
    @Test fun localExactIdsAndClosedResources() {
        assertEquals("/api/v1/files/9223372036854775807/direct", PlaybackFileContext.local(Long.MAX_VALUE).path("direct"))
        for (id in listOf("01", "-1", "9223372036854775808", "1.0", "1/2")) assertThrows(IllegalArgumentException::class.java) { PlaybackFileContext.local(id) }
        val decoded = Net.json.decodeFromString<MediaFileDto>("{\"id\":9007199254740993,\"filename\":\"exact\"}")
        assertEquals("/api/v1/files/9007199254740993/direct", PlaybackFileContext.local(decoded.id).path("direct"))
        assertThrows(IllegalArgumentException::class.java) { PlaybackFileContext.local(decoded.id).localId(7) }
        val local = PlaybackFileContext.local(7)
        for (resource in listOf("../direct", "direct?token=x", "//a.test", "subs/1/../2", "hls/foreign/status")) {
            assertThrows(IllegalArgumentException::class.java) { local.path(resource) }
        }
    }
}
