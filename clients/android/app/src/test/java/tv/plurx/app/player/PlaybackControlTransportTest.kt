package tv.plurx.app.player

import kotlinx.serialization.json.Json
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
        // These four are the whole retryable set. If a rename on the server
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
        assertEquals("owner_changed", failure(409, """{"code":"owner_changed"}""").code)
    }
}
