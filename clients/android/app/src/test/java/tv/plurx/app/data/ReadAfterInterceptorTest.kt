package tv.plurx.app.data

import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.concurrent.TimeUnit
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.ConnectionPool
import okhttp3.Dispatcher
import okhttp3.EventListener
import kotlinx.coroutines.runBlocking
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test

/** Actual OkHttp I/O against two owned loopback HTTP fixtures. No lab,
 * provider, emulator, device, install or live-network-cost measurement. */
class ReadAfterInterceptorTest {
    private data class Reply(val code: Int = 200, val indexes: List<String> = emptyList(), val location: String? = null, val body: String = "{}")
    private data class Captured(val echo: String?, val bearer: String?)
    private class Fixture {
        val server: HttpServer = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 1)
        val origin = "http://127.0.0.1:${server.address.port}"
        private val replies = ArrayDeque<() -> Reply>()
        val requests = mutableListOf<Captured>()
        init {
            server.createContext("/") { exchange ->
                exchange.requestBody.use { it.readBytes() }
                val reply = synchronized(this) {
                    requests.add(Captured(exchange.requestHeaders.getFirst("X-Plurx-Read-After"), exchange.requestHeaders.getFirst("Authorization")))
                    if (replies.isEmpty()) null else replies.removeFirst()
                }?.invoke() ?: Reply(500)
                reply.indexes.forEach { exchange.responseHeaders.add("X-Plurx-Commit-Index", it) }
                reply.location?.let { exchange.responseHeaders.add("Location", it) }
                exchange.responseHeaders.add("Content-Type", "application/json")
                if (reply.code == 204) exchange.sendResponseHeaders(204, -1)
                else {
                    val body = reply.body.toByteArray()
                    exchange.sendResponseHeaders(reply.code, body.size.toLong())
                    exchange.responseBody.use { it.write(body) }
                }
                exchange.close()
            }
            server.start()
        }
        fun enqueue(reply: Reply) = enqueue { reply }
        @Synchronized fun enqueue(reply: () -> Reply) { replies.addLast(reply) }
        @Synchronized fun last(): Captured = requests.last()
        fun stop() { server.stop(0) }
    }
    private lateinit var primary: Fixture
    private lateinit var foreign: Fixture
    private lateinit var adapter: ReadAfterInterceptor
    private lateinit var client: OkHttpClient
    @Volatile private var generation = 1L
    @Volatile private var token = "fixture-a"
    @Before fun setup() {
        primary = Fixture(); foreign = Fixture()
        adapter = ReadAfterInterceptor({ ReadAfterAuthorization(primary.origin, token, generation) })
        client = OkHttpClient.Builder().connectTimeout(2, TimeUnit.SECONDS).readTimeout(2, TimeUnit.SECONDS)
            .addInterceptor(adapter).addNetworkInterceptor(adapter.redirectGuard).build()
    }
    @After fun cleanup() {
        primary.stop(); foreign.stop(); client.connectionPool.evictAll(); client.dispatcher.executorService.shutdown()
    }
    private fun request(path: String, method: String = "GET", bearer: String = token): Request {
        val builder = Request.Builder().url(if (path.startsWith("http:")) path else primary.origin + "/api/v1/" + path)
            .header("Authorization", "Bearer $bearer")
            .tag(ReadAfterAuthorization::class.java, ReadAfterAuthorization(primary.origin, token, generation))
        return builder.method(method, if (method == "GET" || method == "HEAD") null else "{}".toRequestBody("application/json".toMediaType())).build()
    }
    private fun exchange(request: Request) { client.newCall(request).execute().use { it.body?.string() } }
    private fun establish() {
        primary.enqueue(Reply(204, listOf("18446744073709551615")))
        client.newCall(request("items/7/progress", "POST")).execute().use { assertEquals(204, it.code) }
    }

    @Test fun noContentWriteEchoesExactFloorAndAbsentWriteClearsIt() {
        establish()
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertEquals("18446744073709551615", primary.last().echo)
        assertEquals("Bearer fixture-a", primary.last().bearer)
        primary.enqueue(Reply(204)); exchange(request("publication/s1", "DELETE"))
        primary.enqueue(Reply()); exchange(request("items/7"))
        assertNull(primary.last().echo)
    }

    @Test fun publicForeignAndCapturedProfileRequestsNeverBorrowReceipts() {
        establish()
        for (candidate in listOf(request("server"), request("server/"), request("%73erver"),
            request(foreign.origin + "/api/v1/libraries"), request("libraries", bearer = "captured-profile"))) {
            val fixture = if (candidate.url.port == foreign.server.address.port) foreign else primary
            fixture.enqueue(Reply(indexes = listOf("7")))
            exchange(candidate.newBuilder().header("X-Plurx-Read-After", "999").build())
            assertNull(fixture.last().echo)
            assertEquals(candidate.header("Authorization"), fixture.last().bearer)
        }
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertEquals("18446744073709551615", primary.last().echo)
    }

    @Test fun changedAuthRejectsInFlightReceiptBeforeTheNextRequest() {
        establish()
        primary.enqueue { token = "fixture-new"; generation = 2; Reply(indexes = listOf("99")) }
        exchange(request("libraries"))
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertNull(primary.last().echo)
        assertEquals("Bearer fixture-new", primary.last().bearer)
    }

    @Test fun redirectNeverForwardsOrLearnsTheAccountFloor() {
        establish()
        primary.enqueue(Reply(302, location = foreign.origin + "/api/v1/libraries"))
        foreign.enqueue(Reply(indexes = listOf("19"))); exchange(request("libraries"))
        assertNull(foreign.last().echo)
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertEquals("18446744073709551615", primary.last().echo)
        primary.enqueue(Reply(302, location = foreign.origin + "/api/v1/libraries"))
        foreign.enqueue(Reply(indexes = listOf("19"))); exchange(request("items/7/progress", "POST"))
        assertNull(foreign.last().echo)
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertNull(primary.last().echo)
    }

    @Test fun duplicateCommitHeadersAreUnknownNotTheFirstIndex() {
        establish()
        primary.enqueue(Reply(indexes = listOf("19", "20"))); exchange(request("libraries"))
        primary.enqueue(Reply()); exchange(request("libraries"))
        assertNull(primary.last().echo)
    }

    @Test fun actualNativeApiEchoesTheNoContentProgressReceipt() = runBlocking {
        val saved = Session.playbackAuthorization()
        Session.origin = primary.origin; Session.token = "fixture-native"
        // Use Net's real auth/receipt/diagnostic interceptors. Only the Android
        // clock and DEBUG event listener need JVM-compatible fixture inputs.
        val owned = Net.authenticatedClient { System.nanoTime() / 1_000_000 }.newBuilder().dispatcher(Dispatcher()).connectionPool(ConnectionPool())
            .eventListener(EventListener.NONE).build()
        try {
            val api = Net.api(primary.origin, owned)
            primary.enqueue(Reply(204, listOf("18446744073709551615")))
            api.progress(7, ProgressReq(position_ms = 10, duration_ms = 100))
            primary.enqueue(Reply(body = "[]")); assertEquals(emptyList<Library>(), api.libraries())
            assertEquals("18446744073709551615", primary.last().echo)
            assertEquals("Bearer fixture-native", primary.last().bearer)
            primary.enqueue(Reply(204)); api.progress(7, ProgressReq(position_ms = 20))
            primary.enqueue(Reply(body = "[]")); assertEquals(emptyList<Library>(), api.libraries())
            assertNull(primary.last().echo)
        } finally {
            Session.origin = saved.origin; Session.token = saved.token
            owned.connectionPool.evictAll(); owned.dispatcher.executorService.shutdown()
        }
    }

    @Test fun authorizationTimeGenerationFencesReusedBearerAndUnboundRequests() {
        val guarded = ReadAfterInterceptor({ ReadAfterAuthorization(primary.origin, token, generation) })
        val owned = OkHttpClient.Builder().connectTimeout(2, TimeUnit.SECONDS).readTimeout(2, TimeUnit.SECONDS)
            .addInterceptor(guarded).addNetworkInterceptor(guarded.redirectGuard).build()
        try {
            val old = request("libraries").newBuilder().tag(ReadAfterAuthorization::class.java,
                ReadAfterAuthorization(primary.origin, token, generation)).build()
            generation = 3 // sign out/in with the exact same bearer text
            fun bound(path: String, method: String = "GET") = request(path, method).newBuilder()
                .tag(ReadAfterAuthorization::class.java, ReadAfterAuthorization(primary.origin, token, generation)).build()
            primary.enqueue(Reply(204, listOf("33")))
            owned.newCall(bound("items/7/progress", "POST")).execute().use { assertEquals(204, it.code) }
            primary.enqueue(Reply(indexes = listOf("99"))); owned.newCall(old).execute().use { it.body?.string() }
            assertNull(primary.last().echo)
            primary.enqueue(Reply(204, listOf("77")))
            owned.newCall(request("auth/logout", "POST").newBuilder().tag(ReadAfterAuthorization::class.java, null).build())
                .execute().use { assertEquals(204, it.code) }
            assertNull(primary.last().echo)
            primary.enqueue(Reply()); owned.newCall(bound("libraries")).execute().use { it.body?.string() }
            assertEquals("33", primary.last().echo)
        } finally {
            owned.connectionPool.evictAll(); owned.dispatcher.executorService.shutdown()
        }
    }

    @Test fun authenticatedIngressSharesReceiptAcrossVotersAndUnknownPeerWriteClearsIt() = runBlocking {
        val saved = Session.playbackAuthorization()
        Session.origin = primary.origin; Session.token = "fixture-cohort"
        val owned = Net.authenticatedClient { System.nanoTime() / 1_000_000 }.newBuilder().dispatcher(Dispatcher()).connectionPool(ConnectionPool())
            .eventListener(EventListener.NONE).build()
        try {
            val a = Net.api(primary.origin, owned); val b = Net.api(foreign.origin, owned)
            primary.enqueue(Reply(body = "{\"node_urls\":[\"${foreign.origin}\"]}"))
            assertEquals(listOf(foreign.origin), a.clusterIngress().node_urls)
            assertEquals(listOf(foreign.origin), Session.readAfterAuthorization().peers)
            foreign.enqueue(Reply(204, listOf("41"))); b.progress(7, ProgressReq(position_ms = 10))
            primary.enqueue(Reply(body = "[]")); a.libraries(); assertEquals("41", primary.last().echo)
            primary.enqueue(Reply(204, listOf("42"))); a.progress(7, ProgressReq(position_ms = 20))
            foreign.enqueue(Reply(body = "[]")); b.libraries(); assertEquals("42", foreign.last().echo)
            foreign.enqueue(Reply(204)); b.progress(7, ProgressReq(position_ms = 30))
            primary.enqueue(Reply(body = "[]")); a.libraries(); assertNull(primary.last().echo)
        } finally {
            Session.origin = saved.origin; Session.token = saved.token
            owned.connectionPool.evictAll(); owned.dispatcher.executorService.shutdown()
        }
    }

    @Test fun lateIngressCannotAdmitPeersIntoTheNextAuthorizationGeneration() = runBlocking {
        val saved = Session.playbackAuthorization()
        Session.origin = primary.origin; Session.token = "fixture-cohort"
        val owned = Net.authenticatedClient { System.nanoTime() / 1_000_000 }.newBuilder().dispatcher(Dispatcher()).connectionPool(ConnectionPool())
            .eventListener(EventListener.NONE).build()
        try {
            primary.enqueue {
                Session.token = null; Session.token = "fixture-cohort"
                Reply(body = "{\"node_urls\":[\"${foreign.origin}\"]}")
            }
            Net.api(primary.origin, owned).clusterIngress()
            assertEquals(emptyList<String>(), Session.readAfterAuthorization().peers)
            Session.configureNodeOrigins(listOf(foreign.origin), primary.origin)
            assertEquals(emptyList<String>(), Session.readAfterAuthorization().peers)
            Session.origin = "https://a.test"
            Session.recordReadAfterPeers(listOf("https://b.test:443", "http://downgrade.test", "https://bad.test/path"),
                Session.readAfterAuthorization())
            assertEquals(listOf("https://b.test"), Session.readAfterAuthorization().peers)
        } finally {
            Session.origin = saved.origin; Session.token = saved.token
            owned.connectionPool.evictAll(); owned.dispatcher.executorService.shutdown()
        }
    }

    @Test fun capturedDvrWriteRetiresOnlyItsOwnAccountGeneration() = runBlocking {
        val saved = Session.playbackAuthorization()
        Session.origin = primary.origin; Session.token = "fixture-cohort"
        val owned = Net.authenticatedClient { System.nanoTime() / 1_000_000 }.newBuilder().dispatcher(Dispatcher()).connectionPool(ConnectionPool())
            .eventListener(EventListener.NONE).build()
        try {
            val api = Net.api(primary.origin, owned)
            val captured = tv.plurx.app.livetv.DvrApi(primary.origin, "fixture-cohort")
            primary.enqueue(Reply(204, listOf("41"))); api.progress(7, ProgressReq(position_ms = 10))
            primary.enqueue(Reply(204)); captured.deleteRule("rule-fixture")
            assertEquals("41", primary.last().echo)
            primary.enqueue(Reply(body = "[]")); api.libraries(); assertNull(primary.last().echo)
            Session.token = "new-fixture"
            primary.enqueue(Reply(204, listOf("44"))); api.progress(7, ProgressReq(position_ms = 20))
            primary.enqueue(Reply(204)); captured.deleteRule("rule-fixture")
            assertEquals("Bearer fixture-cohort", primary.last().bearer); assertNull(primary.last().echo)
            primary.enqueue(Reply(body = "[]")); api.libraries(); assertEquals("44", primary.last().echo)
        } finally {
            Session.origin = saved.origin; Session.token = saved.token
            owned.connectionPool.evictAll(); owned.dispatcher.executorService.shutdown()
        }
    }
}
