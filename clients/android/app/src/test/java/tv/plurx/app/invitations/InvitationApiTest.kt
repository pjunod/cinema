package tv.plurx.app.invitations

import kotlinx.coroutines.*
import okhttp3.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Session
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

class InvitationApiTest {
    private val id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private val secret = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
    private val broker = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    private fun response(request: Request, body: String, generation: String? = null) = Response.Builder()
        .request(request).protocol(Protocol.HTTP_1_1).code(200).message("OK")
        .body(body.toResponseBody("application/json".toMediaType())).also { if (generation != null) it.header("X-Cinema-Broker-Generation", generation) }.build()
    private fun phone(generation: Long = 2, installation: String = id) = """{"version":"cinema.invitation.v1","phone":{"installation_id":"$installation","name":"Phone","platform":"android","phone_generation":$generation,"created_at":0,"permission_granted":false,"resident_active":false}}"""
    @Test fun homePhoneCasReplyAndLateAuthorizationAreFenced() = runBlocking {
        var body = phone()
        val seen = mutableListOf<Request>()
        val client = InvitationTransport.client().newBuilder().addInterceptor { chain -> seen += chain.request(); response(chain.request(), body) }.build()
        val api = InvitationApi(Session.PlaybackAuthorization("https://home.test", "human-native", 1), client) { true }
        assertEquals(2L, api.availability(id, 1, false, false, secret).phone.phone_generation)
        assertEquals("Bearer human-native", seen.single().header("Authorization"))
        assertEquals(secret, seen.single().header("X-Cinema-Phone-Secret"))
        assertEquals("https://home.test/api/remote/v1/phones/$id/availability", seen.single().url.toString())
        body = phone(1); assertTrue(runCatching { api.availability(id, 1, false, false, secret) }.isFailure)
        body = phone(2, broker); assertTrue(runCatching { api.availability(id, 1, false, false, secret) }.isFailure)
        val entered = CountDownLatch(1); val release = CountDownLatch(1); var current = true
        val delayedClient = client.newBuilder().also { builder -> builder.interceptors().clear() }.addInterceptor { chain ->
            entered.countDown(); check(release.await(2, TimeUnit.SECONDS)); response(chain.request(), phone())
        }.build()
        val old = InvitationApi(Session.PlaybackAuthorization("https://home.test", "old-native", 1), delayedClient) { current }
        val pending = async(Dispatchers.Default) { runCatching { old.availability(id, 1, false, false, secret) } }
        assertTrue(entered.await(2, TimeUnit.SECONDS)); current = false; release.countDown()
        assertTrue(pending.await().isFailure)
    }
    @Test fun brokerClaimContainsOnlyTicketAuthorityAndRequiresExactGeneration() = runBlocking {
        var generation: String? = broker
        var seen: Request? = null
        val client = InvitationTransport.client().newBuilder().proxy(java.net.Proxy.NO_PROXY).addInterceptor { chain ->
            seen = chain.request(); response(chain.request(), "{\"version\":\"cinema.invitation.v1\",\"status\":\"claimed\"}", generation)
        }.build()
        val ticket = InvitationTicket(id, secret, 200, "https://broker.test", broker)
        val api = InvitationBrokerApi(client)
        assertEquals("claimed", api.claim(ticket, "real-sdk-token-fixture", { true }, { 100 }).status)
        val request = requireNotNull(seen)
        assertEquals("Bearer $secret", request.header("Authorization"))
        assertNull(request.header("X-Cinema-Phone-Secret")); assertNull(request.header("X-Cinema-Grant-Secret")); assertNull(request.header("X-Cinema-Broker-Generation"))
        assertEquals("https://broker.test/broker/v1/tickets/claim", request.url.toString())
        generation = null; assertTrue(runCatching { api.claim(ticket, "token", { true }, { 100 }) }.isFailure)
        generation = id; assertTrue(runCatching { api.claim(ticket, "token", { true }, { 100 }) }.isFailure)
        assertTrue(runCatching { api.claim(ticket, "token", { true }, { 200 }) }.isFailure)
    }
}
