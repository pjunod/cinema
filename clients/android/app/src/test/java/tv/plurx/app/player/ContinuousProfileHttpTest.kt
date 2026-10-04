package tv.plurx.app.player

import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.util.concurrent.Executors
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ContinuousProfileHttpTest {
    @Test fun capturedAuthorityAndResponseBoundsSurviveOtherProfileChanges() = runBlocking {
        val authority = mutableListOf<String?>()
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val executor = Executors.newSingleThreadExecutor { runnable -> Thread(runnable, "continuous-http-fixture").apply { isDaemon = true } }
        server.executor = executor
        server.createContext("/api/v1/") { exchange ->
            authority += exchange.requestHeaders.getFirst("Authorization")
            exchange.requestBody.close()
            val response = when (exchange.requestURI.path) {
                "/api/v1/large" -> "{\"value\":\"${"a".repeat(200)}\"}"
                else -> "{\"version\":1}"
            }.toByteArray()
            if (exchange.requestURI.path == "/api/v1/redirect") {
                exchange.responseHeaders.add("Location", "/api/v1/followed")
                exchange.sendResponseHeaders(302, -1)
            } else {
                exchange.sendResponseHeaders(200, response.size.toLong())
                exchange.responseBody.use { it.write(response) }
            }
            exchange.close()
        }
        server.start()
        val previous = tv.plurx.app.data.Session.token
        val transport = ContinuousProfileHttp("http://127.0.0.1:${server.address.port}", "creating-fixture-profile")
        try {
            tv.plurx.app.data.Session.token = "different-fixture-profile"
            assertEquals(1L, transport.request("/api/v1/snapshot").number("version"))
            assertTrue(runCatching { transport.request("/api/v1/large", limit = 32) }.isFailure)
            assertEquals(302, (runCatching { transport.request("/api/v1/redirect") }.exceptionOrNull() as? ContinuousQualityHttpFailure)?.status)
            assertTrue(runCatching { transport.request("https://foreign.invalid/api/v1/family") }.isFailure)
            assertTrue(runCatching { transport.request("/api/v1/../private") }.isFailure)
            assertEquals(listOf("Bearer creating-fixture-profile", "Bearer creating-fixture-profile", "Bearer creating-fixture-profile"), authority)
            transport.close()
            assertTrue(runCatching { transport.request("/api/v1/snapshot") }.isFailure)
            assertEquals(3, authority.size)
        } finally {
            tv.plurx.app.data.Session.token = previous
            transport.close()
            server.stop(0)
            executor.shutdownNow()
        }
    }
}
