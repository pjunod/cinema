package tv.plurx.app.data

import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.Collections
import java.util.UUID
import java.util.concurrent.Executors
import kotlinx.coroutines.runBlocking
import okhttp3.Request
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Before
import org.junit.Test

/**
 * The read-after echo is attached per call site (handoff §5.1 "Android"):
 * the current profile's API and the library pager's bound API echo and
 * capture; Coil images, Media3 segments, the captured-token logout, offline
 * books and the unverified LAN probe do neither — through the same [Net]
 * entry points those call sites use.
 */
class ReadAfterCallSitesTest {
    private data class Seen(val method: String, val path: String, val readAfter: String?)

    private lateinit var server: HttpServer
    private val executor = Executors.newSingleThreadExecutor { Thread(it, "read-after-fixture").apply { isDaemon = true } }
    private val seen = Collections.synchronizedList(mutableListOf<Seen>())
    private lateinit var origin: String
    private var previousOrigin = ""
    private var previousToken: String? = null
    private val token = "fixture-${UUID.randomUUID()}"

    @Before fun start() {
        server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        server.executor = executor
        server.createContext("/") { exchange ->
            val path = exchange.requestURI.path
            seen += Seen(exchange.requestMethod, path, exchange.requestHeaders.getFirst(ReadAfterFloor.READ_AFTER_HEADER))
            exchange.requestBody.close()
            when {
                // The one watch write: a progress post answered with its index.
                path.endsWith("/progress") -> {
                    exchange.responseHeaders.add(ReadAfterFloor.COMMIT_INDEX_HEADER, "77")
                    exchange.sendResponseHeaders(204, -1)
                }
                // Writes from an older peer: no receipt. A bound caller would forget.
                exchange.requestMethod != "GET" -> {
                    exchange.sendResponseHeaders(204, -1)
                }
                // Every read offers a foreign, larger index that must never be adopted
                // by an unbound caller.
                else -> {
                    val body = "{}".toByteArray()
                    exchange.responseHeaders.add(ReadAfterFloor.COMMIT_INDEX_HEADER, "999")
                    exchange.sendResponseHeaders(200, body.size.toLong())
                    exchange.responseBody.use { it.write(body) }
                }
            }
            exchange.close()
        }
        server.start()
        origin = "http://127.0.0.1:${server.address.port}"
        previousOrigin = Session.origin
        previousToken = Session.token
        Session.origin = origin
        Session.token = token
    }

    @After fun stop() {
        Session.origin = previousOrigin
        Session.token = previousToken
        server.stop(0)
        executor.shutdownNow()
    }

    private fun held(): String? = ReadAfter.floor.request(ReadAfter.floor.generation()).index

    private fun lastSeen(path: String): Seen = synchronized(seen) { seen.last { it.path == path } }

    /** Make the current profile hold index 77 through its own write. */
    private fun write() = runBlocking {
        Net.profileApi(origin).progress(5, ProgressReq(position_ms = 1_000))
        assertEquals("77", held())
    }

    @Test fun theCurrentProfilesApiEchoesAndCaptures() = runBlocking {
        write()
        runCatching { Net.profileApi(origin).hubs() }
        assertEquals("77", lastSeen("/api/v1/hubs").readAfter)
        // Its own reads are API reads, so their index is adopted.
        assertEquals("999", held())
    }

    @Test fun theLibraryPagersBoundApiEchoes() = runBlocking {
        write()
        val pager = Net.api(origin, Net.profileClient(token), ReadAfter.floor.generation())
        runCatching { pager.libraryItems(3, limit = 200, offset = 0, sort = "title") }
        assertEquals("77", lastSeen("/api/v1/libraries/3/items").readAfter)
    }

    @Test fun aPagerBuiltForAnEndedSessionNeitherEchoesNorCaptures() = runBlocking {
        write()
        val generation = ReadAfter.floor.generation()
        Session.token = "$token-next"
        Net.profileApi(origin).progress(5, ProgressReq(position_ms = 2_000))
        assertEquals("77", held())
        val stale = Net.api(origin, Net.profileClient(token), generation)
        runCatching { stale.libraryItems(3) }
        assertNull(lastSeen("/api/v1/libraries/3/items").readAfter)
        assertEquals("77", held())
    }

    @Test fun coilImageRequestsCarryNothingAndCaptureNothing() {
        write()
        // Coil's ImageLoader is built on Net.client (PlurxApp); its requests are plain calls.
        Net.client.newCall(Request.Builder().url("$origin/api/v1/images/7/poster").build()).execute().close()
        assertNull(lastSeen("/api/v1/images/7/poster").readAfter)
        assertEquals("77", held())
    }

    @Test fun media3SegmentRequestsCarryNothingAndCaptureNothing() {
        write()
        // Media3's HTTP source is Net.dataSourceFactory(), whose call factory is Net.client.
        val factory = Net.dataSourceFactory()
        val callFactory = factory.javaClass.getDeclaredField("callFactory").apply { isAccessible = true }.get(factory)
        assertSame(Net.client, callFactory)
        Net.client.newCall(Request.Builder().url("$origin/api/v1/hls/session/seg-00001.ts").build()).execute().close()
        assertNull(lastSeen("/api/v1/hls/session/seg-00001.ts").readAfter)
        assertEquals("77", held())
    }

    @Test fun theCapturedTokenLogoutCarriesNothingAndForgetsNothing() = runBlocking {
        write()
        // AppViewModel.logout: Net.api(capturedOrigin, Net.profileClient(capturedToken)).logout()
        Net.api(origin, Net.profileClient(token)).logout()
        assertEquals(Seen("POST", "/api/v1/auth/logout", null), lastSeen("/api/v1/auth/logout"))
        // An unindexed write on a bound call would have forgotten 77.
        assertEquals("77", held())
    }

    @Test fun offlineBookRequestsCarryNothingAndCaptureNothing() = runBlocking {
        write()
        // OfflineBooks.transfer: Net.api(request.origin, Net.profileClient(request.token))
        val api = Net.api(origin, Net.profileClient(token))
        runCatching { api.openPublication(9) }
        runCatching { api.bookContent(9).body()?.close() }
        assertNull(lastSeen("/api/v1/files/9/publication").readAfter)
        assertNull(lastSeen("/api/v1/files/9/content").readAfter)
        assertEquals("77", held())
    }

    @Test fun theLanProbeCarriesNothingAndCapturesNothing() = runBlocking {
        write()
        // AppViewModel.rediscoverSavedServer: Net.api(candidateOrigin).server()
        runCatching { Net.api(origin).server() }
        assertNull(lastSeen("/api/v1/server").readAfter)
        assertEquals("77", held())
    }

    @Test fun anOriginChangeClearsTheFloor() {
        write()
        Session.origin = "$origin/"
        assertNull(held())
    }

    @Test fun signOutClearsTheFloorAndSettingTheSameTokenAgainDoesNot() {
        write()
        Session.token = token
        assertEquals("77", held())
        Session.token = null
        assertNull(held())
    }
}
