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

class PlaybackFileContextTest {
    private val ref = SharedPlaybackReference("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993", "9223372036854775807")
    private val file = "9007199254740993"
    private val base = "/api/v1/shared/imports/${ref.import_id}/files/signed_locator-ABC123"
    private val revision = "a".repeat(64)
    private suspend fun fetch(locator: String?, mutate: (JsonObject) -> JsonObject = { it }, change: () -> Unit = {}): PlaybackFileContext {
        val item = Json.parseToJsonElement(Json.encodeToString(ref))
        val row = mutate(buildJsonObject {
            put("file_id", file); put("revision", revision)
            put("reference", buildJsonObject { put("item", item); put("file_id", file); put("revision", revision) })
            if (locator != null) put("file_base", locator)
        })
        val body = buildJsonObject { put("files", buildJsonArray { add(row) }) }.toString()
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            assertEquals("b.test", chain.request().url.host)
            assertEquals("Bearer fixture-bearer", chain.request().header("Authorization"))
            change()
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("OK")
                .body(body.toResponseBody()).build()
        }.build()
        return PlaybackFileContext.authenticatedDetailForTest(ref, file, client)
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
    @Test fun localExactIdsAndClosedResources() {
        assertEquals("/api/v1/files/9223372036854775807/direct", PlaybackFileContext.local(Long.MAX_VALUE).path("direct"))
        for (id in listOf("01", "-1", "9223372036854775808", "1.0", "1/2")) assertThrows(IllegalArgumentException::class.java) { PlaybackFileContext.local(id) }
        val local = PlaybackFileContext.local(7)
        for (resource in listOf("../direct", "direct?token=x", "//a.test", "subs/1/../2", "hls/foreign/status")) {
            assertThrows(IllegalArgumentException::class.java) { local.path(resource) }
        }
    }
}
