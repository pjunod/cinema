package tv.plurx.app.player

import kotlinx.serialization.json.Json
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/**
 * The transport's whole job is turning an HTTP response into the fields the
 * reporter classifies on. Getting these wrong makes a retryable refusal look
 * terminal — the reporter stops reporting for the rest of a film — or a
 * terminal one look retryable, which is a client hammering a server that has
 * already said no.
 */
class PlaybackControlTransportTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    private fun failure(status: Int, body: String) =
        PlaybackControlTransport.failure(status, body, json)

    @Test
    fun `a status survives a body that says nothing`() {
        listOf("", "not json at all", "[]", "null", "{}").forEach { body ->
            val value = failure(503, body)
            assertEquals(503, value.status, "body was '$body'")
            assertNull(value.code)
        }
    }

    @Test
    fun `a typed refusal carries its code`() {
        val value = failure(429, """{"code":"control_rate_limited","message":"slow down"}""")
        assertEquals(429, value.status)
        assertEquals("control_rate_limited", value.code)
        assertNull(value.retryAfterMs)
    }

    @Test
    fun `a retry-after is carried through`() {
        val value = failure(503, """{"code":"control_unavailable","retry_after_ms":4000}""")
        assertEquals(4_000, value.retryAfterMs)
    }

    private fun headerFailure(header: String, body: String = """{"code":"serving_fenced","message":"this node has lost quorum serving authority"}""") =
        Response.Builder()
            .request(Request.Builder().url("https://media.example/api/v1/hls/session-1/control").build())
            .protocol(Protocol.HTTP_1_1).code(503).message("Service Unavailable")
            .header("rEtRy-AfTeR", header).body(body.toResponseBody()).build().use {
                PlaybackControlTransport.failure(it, json)
            }

    @Test
    fun headerOnlyServingFenceCarriesTheServersDelay() {
        val value = headerFailure("1")
        assertEquals(503, value.status)
        assertEquals("serving_fenced", value.code)
        assertEquals(1_000L, value.retryAfterMs)
        assertEquals(1_000L, headerFailure(" 1 ", "not json").retryAfterMs)
    }

    @Test
    fun headerAndBodyCannotShortenEitherValidDelay() {
        assertEquals(4_000L, headerFailure("1", """{"retry_after_ms":4000}""").retryAfterMs)
        assertEquals(4_000L, headerFailure("4", """{"retry_after_ms":1000}""").retryAfterMs)
        assertEquals(1_000L, headerFailure("1", """{"retry_after_ms":999999}""").retryAfterMs)
    }

    @Test
    fun malformedOrOverBudgetHeadersPreserveTheLegacyBody() {
        listOf("", "-1", "+1", "1.5", "soon", "61", "9999999999999999999999").forEach { header ->
            assertNull(headerFailure(header).retryAfterMs, "header was '$header'")
            assertEquals(4_000L, headerFailure(header, """{"retry_after_ms":4000}""").retryAfterMs)
        }
        assertEquals(60_000L, headerFailure("60").retryAfterMs)
        assertEquals(0L, headerFailure("0").retryAfterMs)
    }

    @Test
    fun `an owner change carries the new owner`() {
        val value = failure(
            409,
            """{"code":"owner_changed","generation":"44444444-4444-4444-8444-444444444444",""" +
                """"control_epoch":9}""",
        )
        assertEquals("owner_changed", value.code)
        assertEquals("44444444-4444-4444-8444-444444444444", value.generation)
        assertEquals(9, value.controlEpoch)
    }

    @Test
    fun `a field of the wrong type is ignored rather than crashing the exchange`() {
        val value = failure(
            409,
            """{"code":"owner_changed","generation":42,"control_epoch":"nine",""" +
                """"retry_after_ms":"soon"}""",
        )
        assertEquals("owner_changed", value.code)
        assertNull(value.generation, "a numeric generation is not a generation")
        assertNull(value.controlEpoch)
        assertNull(value.retryAfterMs)
    }

    @Test
    fun `the classifications the reporter depends on round-trip`() {
        // These are the typed outcomes used by the retry owner. If a rename on the server
        // ever breaks one, this is where it shows up rather than in a client
        // that quietly stopped reporting.
        assertEquals("owner_transition", failure(425, """{"code":"owner_transition"}""").code)
        assertEquals(
            "control_rate_limited",
            failure(429, """{"code":"control_rate_limited"}""").code,
        )
        assertEquals(
            "control_unavailable",
            failure(503, """{"code":"control_unavailable"}""").code,
        )
        assertEquals("serving_fenced", failure(503, """{"code":"serving_fenced"}""").code)
        assertEquals("owner_changed", failure(409, """{"code":"owner_changed"}""").code)
    }
}
