package tv.plurx.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test

/**
 * Manual watched state and next episode through a synthetic B. Both are B
 * viewer routes only: no Local item route, no Source address, no numeric
 * Local ID. Protocol evidence, not a real pinned Source.
 */
class SharedCatalogueActionsTest {
    private val library = SharedLibraryIdentity("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993")
    private val foreign = SharedLibraryIdentity("44444444-4444-4444-8444-444444444444", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993")
    private val requests = mutableListOf<Request>()
    private val bodies = mutableListOf<String>()
    /** id -> (kind, parent id) in a Source hierarchy; ids above 2^53 stay strings. */
    private val items = linkedMapOf(
        "9007199254740995" to ("series" to null),
        "9007199254740996" to ("season" to "9007199254740995"),
        "9007199254740997" to ("season" to "9007199254740995"),
        // Source order is page order, not numeric order.
        "9223372036854775801" to ("episode" to "9007199254740996"),
        "12" to ("episode" to "9007199254740996"),
        "9223372036854775807" to ("episode" to "9007199254740996"),
        "40" to ("episode" to "9007199254740997"),
        "50" to ("movie" to null),
    )
    private var childLibrary = library
    private var handler: (Request, String) -> Pair<Int, JsonObject> = { request, body -> route(request, body) }

    @Before fun setup() { Session.origin = "https://b.test"; Session.token = "fixture-bearer" }
    @After fun teardown() { Session.token = null; Session.origin = "" }

    private fun client() = SharedLibraryClient.forTest(OkHttpClient.Builder().addInterceptor { chain ->
        val request = chain.request(); requests += request
        val body = request.body?.let { val buffer = okio.Buffer(); it.writeTo(buffer); buffer.readUtf8() } ?: ""
        bodies += body
        val (status, reply) = handler(request, body)
        Response.Builder().request(request).protocol(Protocol.HTTP_1_1).code(status).message("Fixture").body(reply.toString().toResponseBody()).build()
    }.build())

    private fun wireItem(id: String, owner: SharedLibraryIdentity = library) = buildJsonObject {
        val (kind, parent) = items.getValue(id)
        put("source", "shared"); put("reference", Json.encodeToJsonElement(owner.reference(id))); put("title", "Item $id"); put("kind", kind)
        parent?.let { put("parent", Json.encodeToJsonElement(owner.reference(it))) }
        put("genres", buildJsonArray {})
    }
    private fun route(request: Request, body: String): Pair<Int, JsonObject> {
        val prefix = "/api/v1/shared/imports/${library.import_id}/items/"
        val path = request.url.encodedPath
        assertTrue(path, path.startsWith(prefix))
        val rest = path.removePrefix(prefix).split("/")
        val id = rest[0]
        return when {
            rest.size == 1 -> 200 to buildJsonObject {
                put("item", wireItem(id)); put("files", buildJsonArray {}); put("delivery_status", "unavailable"); put("lifecycle_generation", 5)
            }
            rest[1] == "children" -> {
                // Two rows a page, so a season crosses a cursor.
                val children = items.filter { it.value.second == id }.keys.toList()
                val start = request.url.queryParameter("cursor")?.removePrefix("page-")?.toInt() ?: 0
                val next = start + 2
                200 to buildJsonObject {
                    put("items", buildJsonArray { children.drop(start).take(2).forEach { add(wireItem(it, childLibrary)) } })
                    put("next_cursor", if (next < children.size) JsonPrimitive("page-$next") else JsonNull)
                    put("catalogue_revision", 1); put("scope_generation", 1); put("catalogue_generation", 1)
                }
            }
            rest[1] == "watched" -> {
                assertEquals("POST", request.method)
                val watched = Json.parseToJsonElement(body).jsonObject.getValue("watched").jsonPrimitive.boolean
                200 to buildJsonObject { put("updated", 1); put("watch", buildJsonObject {
                    put("position_ms", 0); put("watched", watched); put("sequence", 9); put("updated_at_ms", 1)
                }) }
            }
            else -> 404 to buildJsonObject { put("code", "sharing_not_found"); put("message", "Not found") }
        }
    }

    @Test fun manualWatchedPostsOnlyToTheBPrivateSharedRoute(): Unit = runBlocking {
        val api = client()
        val watch = api.setWatched(library.reference("12"), true)
        assertTrue(watch.watched); assertEquals(9L, watch.sequence)
        assertEquals("/api/v1/shared/imports/${library.import_id}/items/12/watched", requests.single().url.encodedPath)
        assertEquals(buildJsonObject { put("watched", true) }, Json.parseToJsonElement(bodies.single()))
        assertFalse(api.setWatched(library.reference("12"), false).watched)
        // An answer for the other state is not this action's result.
        handler = { _, _ -> 200 to buildJsonObject { put("updated", 1); put("watch", buildJsonObject {
            put("position_ms", 0); put("watched", false); put("sequence", 10); put("updated_at_ms", 1) }) } }
        assertTrue(runCatching { api.setWatched(library.reference("12"), true) }.isFailure)
        // B refuses a series typed; the refusal reaches the caller as it is.
        handler = { _, _ -> 409 to buildJsonObject { put("code", "sharing_watch_unsupported"); put("message", "Not watchable") } }
        assertEquals("sharing_watch_unsupported", (runCatching { api.setWatched(library.reference("9007199254740995"), true) }.exceptionOrNull() as RefusalException).code)
        assertTrue(requests.none { it.url.encodedPath.startsWith("/api/v1/items") })
    }

    @Test fun nextEpisodeFollowsSourceOrderThroughBViewerRoutes(): Unit = runBlocking {
        val api = client()
        assertEquals(library.reference("12"), api.nextEpisode(library.reference("9223372036854775801")))
        // Across a page boundary of the same season.
        assertEquals(library.reference("9223372036854775807"), api.nextEpisode(library.reference("12")))
        // The last episode of a season continues with the first of the next one.
        assertEquals(library.reference("40"), api.nextEpisode(library.reference("9223372036854775807")))
        // The last episode of the series, and anything that is not an episode, has none.
        assertNull(api.nextEpisode(library.reference("40")))
        assertNull(api.nextEpisode(library.reference("50")))
        assertTrue(requests.all { it.method == "GET" && it.url.encodedPath.startsWith("/api/v1/shared/imports/${library.import_id}/items/") })
    }

    @Test fun nextEpisodeRefusesARowFromAnotherImport(): Unit = runBlocking {
        childLibrary = foreign
        assertTrue(runCatching { client().nextEpisode(library.reference("12")) }.isFailure)
    }
}
