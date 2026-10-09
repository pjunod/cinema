package tv.plurx.app.invitations

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Session
import tv.plurx.app.remote.RemoteWire
import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.concurrent.Executors

/** Actual synthetic Router reply schemas; tracked fixtures replace all emitted proofs with A43. */
class InvitationRouterSamplesTest {
    private fun samples(exactReadOnly: Boolean = false): Map<String, ByteArray> {
        val raw = if (exactReadOnly) System.getenv("CINEMA_INVITATION_ROUTER_SAMPLES") else null
        val bytes = if (raw == null) javaClass.getResourceAsStream("/cinema-invitation-router-samples.json")!!.use { it.readBytes() }
            else java.io.File(raw).also { require(it.length() <= 65536) }.readBytes()
        val fixture = RemoteWire.objectBody(bytes, 65536)
        assertTrue(fixture.getValue("synthetic_only").jsonPrimitive.boolean)
        assertFalse(fixture.getValue("live").jsonPrimitive.boolean)
        return fixture.getValue("samples").jsonArray.associate { row ->
            val value = row.jsonObject
            value.getValue("operation").jsonPrimitive.content to value.getValue("response").toString().toByteArray(Charsets.UTF_8)
        }
    }
    @Test fun productionParserAcceptsActualHomeRouterRepliesAndEnrollmentRelations() {
        val rows = samples(exactReadOnly = true)
        val registration = InvitationWire.phoneReply(rows.getValue("phones"))
        val availability = InvitationWire.availability(rows.getValue("availability"))
        assertEquals(registration.phone.installation_id, availability.phone.installation_id)
        assertTrue(availability.phone.phone_generation > registration.phone.phone_generation)
        val consent = InvitationWire.consentReply(rows.getValue("consent_on"))
        val start = InvitationWire.start(rows.getValue("transport_start"))
        val confirm = InvitationWire.consentReply(rows.getValue("transport_confirm"))
        assertEquals("fcm", start.consent.transport); assertNotNull(start.ticket)
        assertEquals(consent.consent.receiver_id, start.consent.receiver_id)
        assertTrue(start.consent.consent_generation > consent.consent.consent_generation)
        assertEquals(start.consent.consent_generation, confirm.consent.consent_generation)
        assertEquals(start.consent.transport_generation, confirm.consent.transport_generation)
        val page = InvitationWire.consentPage(rows.getValue("consents_list"))
        assertTrue(page.consents.any { it.receiver_id == confirm.consent.receiver_id })
        val lookup = InvitationWire.lookup(rows.getValue("lookup"))
        assertEquals(confirm.consent.receiver_id, lookup.receiver_id)
    }
    @Test fun realLoopbackHomeTransportDecodesRedactedRouterSchemaAndUsesOnlySelectedHomeAuthority() = runBlocking {
        val body = samples().getValue("phones")
        val expected = InvitationWire.phoneReply(body)
        assertEquals("A".repeat(43), expected.phone_secret)
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 1)
        val executor = Executors.newSingleThreadExecutor(); server.executor = executor
        server.createContext("/api/remote/v1/phones") { exchange ->
            try {
                assertEquals("POST", exchange.requestMethod)
                assertEquals(listOf("Bearer synthetic-human-native"), exchange.requestHeaders["Authorization"])
                assertNull(exchange.requestHeaders["X-Cinema-Grant-Secret"]); assertNull(exchange.requestHeaders["X-Cinema-Phone-Secret"])
                val input = exchange.requestBody.use { it.readNBytes(65537) }; require(input.size <= 65536)
                val request = RemoteWire.objectBody(input)
                assertEquals(expected.phone.installation_id, request.getValue("installation_id").jsonPrimitive.content)
                assertEquals(INVITATION_VERSION, request.getValue("version").jsonPrimitive.content)
                exchange.responseHeaders.add("Content-Type", "application/json")
                exchange.sendResponseHeaders(200, body.size.toLong()); exchange.responseBody.use { it.write(body) }
            } finally { exchange.close() }
        }
        server.start(); val client = InvitationTransport.client()
        try {
            val api = InvitationApi(Session.PlaybackAuthorization("http://127.0.0.1:" + server.address.port, "synthetic-human-native", 1), client) { true }
            assertEquals(expected, api.register(expected.phone.installation_id, "Android phone"))
        } finally { server.stop(0); executor.shutdownNow(); client.connectionPool.evictAll(); client.dispatcher.executorService.shutdown() }
    }
}
