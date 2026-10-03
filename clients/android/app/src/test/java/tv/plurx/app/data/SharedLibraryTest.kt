package tv.plurx.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.*
import org.junit.After
import org.junit.Before
import org.junit.Test

class SharedLibraryTest {
    private val first = SharedLibraryIdentity("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222", "33333333-3333-4333-8333-333333333333", "9007199254740993")
    private val other = SharedLibraryIdentity("44444444-4444-4444-8444-444444444444", "55555555-5555-4555-8555-555555555555", "66666666-6666-4666-8666-666666666666", "9007199254740993")
    private val item = "9223372036854775807"
    private val revision = "a".repeat(64)
    private val requests = mutableListOf<Request>()
    private var handler: (Request) -> Pair<Int, JsonObject> = { 500 to buildJsonObject {} }
    @Before fun setup() { Session.origin = "https://b.test"; Session.token = "fixture-bearer" }
    @After fun teardown() { Session.token = null; Session.origin = "" }
    private fun client(): SharedLibraryClient = SharedLibraryClient.forTest(OkHttpClient.Builder().addInterceptor { chain ->
        val request = chain.request(); requests += request
        assertEquals("b.test", request.url.host); assertEquals("Bearer fixture-bearer", request.header("Authorization"))
        val (status, body) = handler(request)
        Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(status).message("Fixture").body(body.toString().toResponseBody()).build()
    }.build())
    private fun assignment(source: SharedLibraryIdentity) = JsonObject(Json.encodeToJsonElement(source).jsonObject + mapOf("source_name" to JsonPrimitive(if (source == first) "Source A" else "Source C"), "availability" to JsonPrimitive("unverified")))
    private fun wireItem(source: SharedLibraryIdentity, id: String = item) = buildJsonObject {
        put("source", "shared"); put("reference", Json.encodeToJsonElement(source.reference(id))); put("title", "Distinct Source title"); put("kind", "movie"); put("genres", buildJsonArray {})
    }
    private fun page(source: SharedLibraryIdentity, cursor: String?, ids: List<String>): SharedLibraryPage = Json.decodeFromJsonElement(buildJsonObject {
        put("items", buildJsonArray { ids.forEach { add(wireItem(source, it)) } }); put("next_cursor", cursor?.let(::JsonPrimitive) ?: JsonNull)
        put("catalogue_revision", 2); put("scope_generation", 3); put("catalogue_generation", 4)
    })
    private fun detail(source: SharedLibraryIdentity) = buildJsonObject {
        put("item", wireItem(source)); put("files", buildJsonArray { add(buildJsonObject {
            put("file_id", "9007199254740993"); put("revision", revision); put("size", "9007199254740993"); put("container", "mkv"); put("video_codec", "hevc")
            put("reference", buildJsonObject { put("item", Json.encodeToJsonElement(source.reference(item))); put("file_id", "9007199254740993"); put("revision", revision) })
        }) })
        put("watch", buildJsonObject { put("position_ms", 12000); put("watched", false); put("sequence", 1); put("updated_at_ms", 1) })
        put("delivery_status", "unavailable")
    }
    @Test fun actualBOnlyBrowsePreservesIdsAndIndependentSourceFailure(): Unit = runBlocking {
        handler = { request -> when {
            request.url.encodedPath == "/api/v1/shared/libraries" -> 200 to buildJsonObject { put("libraries", buildJsonArray { add(assignment(first)); add(assignment(other)) }) }
            first.import_id in request.url.encodedPath -> 503 to buildJsonObject { put("code", "sharing_source_unavailable"); put("message", "Source unavailable") }
            else -> 200 to buildJsonObject {
                put("import_id", other.import_id); put("server_id", other.server_id); put("catalogue_epoch", other.catalogue_epoch)
                put("libraries", buildJsonArray {
                    add(buildJsonObject { put("library_id", other.library_id); put("name", "Remote movies"); put("kind", "movie"); put("anime", false) })
                    add(buildJsonObject { put("library_id", "7"); put("name", "Unassigned"); put("kind", "movie"); put("anime", false) })
                })
            }
        } }
        val api = client(); val assigned = api.assignments()
        assertEquals(first.library_id, assigned[0].library_id); assertNotEquals(assigned[0].id, assigned[1].id)
        val error = runCatching { api.libraries(listOf(assigned[0])) }.exceptionOrNull() as RefusalException
        assertEquals("sharing_source_unavailable", error.code)
        val working = api.libraries(listOf(assigned[1])); assertEquals(1, working.size); assertEquals(other, working[0].identity)
        assertTrue(requests.all { it.url.encodedPath.startsWith("/api/v1/shared/") })
    }
    @Test fun cursorAdvancesAcrossDuplicatesWithoutCrossSourceCollision() {
        var browse = SharedBrowseAccumulator()
        browse = browse.append(page(first, "opaque-1", listOf(item)), null, first)
        browse = browse.append(page(first, "opaque-2", listOf(item)), "opaque-1", first)
        assertEquals(1, browse.items.size); assertEquals("opaque-2", browse.nextCursor)
        browse = browse.append(page(first, null, listOf("9007199254740993")), "opaque-2", first)
        assertEquals(2, browse.items.size)
        assertNotEquals(page(first, null, listOf(item)).items[0].id, page(other, null, listOf(item)).items[0].id)
        assertThrows(IllegalArgumentException::class.java) { browse.append(page(other, null, listOf(item)), null, first) }
    }
    @Test fun actualDetailUsesStringFileReferenceWithoutLocalFallback(): Unit = runBlocking {
        handler = { request ->
            assertEquals("/api/v1/shared/imports/${first.import_id}/items/$item", request.url.encodedPath)
            200 to detail(first)
        }
        val api = client(); val result = api.detail(first.reference(item))
        assertEquals("9007199254740993", result.files[0].file_id); assertEquals(item, result.item.reference.item_id)
        assertEquals("unavailable", result.delivery_status); assertEquals(12000L, result.watch?.position_ms)
        handler = { 200 to detail(other) }
        assertNotNull(runCatching { api.detail(first.reference(item)) }.exceptionOrNull())
        handler = {
            val value = detail(first); val file = value.getValue("files").jsonArray.single().jsonObject
            200 to JsonObject(value + ("files" to buildJsonArray { add(JsonObject(file + ("file_id" to JsonPrimitive(9007199254740993L)))) }))
        }
        assertNotNull(runCatching { api.detail(first.reference(item)) }.exceptionOrNull())
    }
    @Test fun actualPageRouteQueryAndRetiredAuthorizationFence(): Unit = runBlocking {
        handler = { request ->
            assertEquals("/api/v1/shared/imports/${first.import_id}/libraries/${first.library_id}/items", request.url.encodedPath)
            assertEquals("A+B & 雪/?", request.url.queryParameter("q")); assertEquals("opaque+=/", request.url.queryParameter("cursor")); assertTrue(request.url.encodedQuery!!.contains("opaque%2B"))
            200 to buildJsonObject { put("items", buildJsonArray { add(wireItem(first)) }); put("catalogue_revision", 1); put("scope_generation", 1); put("catalogue_generation", 1) }
        }
        val api = client(); val result = api.page(first, q = "A+B & 雪/?", cursor = "opaque+=/")
        assertEquals(item, result.items[0].reference.item_id)
        handler = { Session.token = "other-account"; 200 to buildJsonObject { put("libraries", buildJsonArray { add(assignment(first)) }) } }
        assertNotNull(runCatching { api.assignments() }.exceptionOrNull())
    }
    @Test fun savedChoiceAndPendingEditSurviveUnknownReadiness(): Unit = runBlocking {
        handler = { request -> when {
            request.url.encodedPath == "/api/v1/sharing/status" -> 503 to buildJsonObject { put("code", "sharing_authority_unavailable"); put("message", "Unknown readiness") }
            request.method == "PUT" -> {
                val buffer = okio.Buffer(); requireNotNull(request.body).writeTo(buffer)
                assertFalse(Json.parseToJsonElement(buffer.readUtf8()).jsonObject.getValue("enabled").jsonPrimitive.boolean)
                200 to buildJsonObject { put("enabled", false) }
            }
            else -> 200 to buildJsonObject { put("enabled", true) }
        } }
        val api = client(); var draft = SharedSharingDraft().received(api.settings(), 0)
        assertTrue(draft.enabled); assertNotNull(runCatching { api.management("status") }.exceptionOrNull()); assertTrue(draft.enabled)
        draft = draft.choose(false); val revision = draft.revision
        assertFalse(api.save(draft.enabled)); draft = draft.choose(true).received(false, revision)
        assertTrue(draft.enabled)
    }
}
