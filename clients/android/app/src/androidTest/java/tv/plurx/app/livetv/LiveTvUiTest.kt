package tv.plurx.app.livetv

import android.content.res.Configuration
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.assertHasClickAction
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertIsFocused
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.compose.ui.test.onAllNodesWithText
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.onRoot
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.printToString
import androidx.test.platform.app.InstrumentationRegistry
import java.net.InetAddress
import java.net.ServerSocket
import java.net.SocketException
import java.util.Collections
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import tv.plurx.app.data.Session
import tv.plurx.app.ui.theme.PlurxTheme

/** Loopback only: no production profile, device, tuner or external network. */
class LiveTvUiTest {
    @get:Rule val compose = createComposeRule()
    private lateinit var server: ServerSocket
    private lateinit var worker: Thread
    private var previousToken: String? = null
    private val requests = Collections.synchronizedList(mutableListOf<String>())
    private val unexpectedPaths = Collections.synchronizedList(mutableListOf<String>())
    private val fixtureErrors = Collections.synchronizedList(mutableListOf<String>())
    private val accepted = AtomicInteger(0)
    private var enabled = false
    private var generation = 4

    /**
     * Bind the literal address the client dials. `InetAddress.getLoopbackAddress()`
     * answers `::1` wherever `java.net.preferIPv6Addresses` is set, which binds a
     * socket the `http://127.0.0.1` origin below can never reach — the fixture then
     * looks alive while every request is refused.
     */
    private val loopback: InetAddress = InetAddress.getByName("127.0.0.1")
    private val origin: String get() = "http://127.0.0.1:${server.localPort}"

    /** The 10-foot profile, where initial focus is a product requirement. */
    private val television: Boolean
        get() = InstrumentationRegistry.getInstrumentation().targetContext.resources
            .configuration.uiMode and Configuration.UI_MODE_TYPE_MASK ==
            Configuration.UI_MODE_TYPE_TELEVISION

    @Before fun startFixture() {
        previousToken = Session.token
        Session.token = "live-tv-ui-fixture"
        server = ServerSocket(0, 8, loopback)
        server.soTimeout = 1000
        worker = thread(name = "live-tv-ui-fixture", isDaemon = true) {
            while (!server.isClosed) {
                try {
                    server.accept().use { socket ->
                        accepted.incrementAndGet()
                        socket.soTimeout = 3000
                        val input = socket.getInputStream().bufferedReader()
                        val request = input.readLine() ?: return@use
                        var length = 0
                        var authorization: String? = null
                        while (true) {
                            val line = input.readLine() ?: break
                            if (line.isEmpty()) break
                            if (line.startsWith("Content-Length:", true)) length = line.substringAfter(':').trim().toInt()
                            if (line.startsWith("Authorization:", true)) authorization = line.substringAfter(':').trim()
                        }
                        val body = CharArray(length)
                        var read = 0
                        while (read < length) { val count = input.read(body, read, length - read); if (count < 0) break; read += count }
                        requests += request + " " + String(body)
                        val path = request.split(' ')[1]
                        val response = when {
                            authorization != "Bearer live-tv-ui-fixture" -> {
                                fixtureErrors += "unauthenticated $request (authorization=$authorization)"
                                "{}"
                            }
                            path == "/api/v1/live-tv/channels" -> """{"freshness":"fresh","channels":[
                                {"id":"one","guide_number":"7.1","guide_name":"Fixture News","favorite":true,"drm":false,"support":"ready"},
                                {"id":"protected","guide_number":"107.1","guide_name":"Protected News","favorite":false,"drm":true,"support":"drm_unsupported"}]}"""
                            path == "/api/v1/live-tv/readiness/refresh" ->
                                """{"ready":true,"generation":$generation,"checks":[{"id":"tuner","ready":true,"message":"Fixture tuner reachable"}]}"""
                            path == "/api/v1/settings" -> {
                                if (request.startsWith("PUT ")) {
                                    val write = org.json.JSONObject(String(body))
                                    check(write.getLong("live_tv_config_generation") == generation.toLong())
                                    check(write.length() == 2 && write.has("live_tv_enabled"))
                                    enabled = write.getBoolean("live_tv_enabled")
                                    generation += 1
                                }
                                """{"live_tv_enabled":$enabled,"live_tv_device_ipv4":"192.168.4.20","live_tv_owner_node_id":"fixture-voter",
                                    "live_tv_max_sessions":2,"live_tv_output_height":720,"live_tv_config_generation":$generation,
                                    "live_tv_transition_from_owner_node_id":"","live_tv_transition_drain_before":0}"""
                            }
                            else -> {
                                unexpectedPaths += path
                                "{}"
                            }
                        }
                        val bytes = response.toByteArray()
                        socket.getOutputStream().apply {
                            write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: ${bytes.size}\r\nConnection: close\r\n\r\n".toByteArray())
                            write(bytes); flush()
                        }
                    }
                } catch (_: java.net.SocketTimeoutException) { /* Bounded shutdown polling. */ }
                catch (closing: SocketException) { if (!server.isClosed) fixtureErrors += "socket: $closing" }
                // A fixture fault must name itself in the failure message instead of
                // killing this thread and leaving the screen on a silent 10s timeout.
                catch (fault: Throwable) { fixtureErrors += "fault: $fault" }
            }
        }
    }

    @After fun stopFixture() {
        Session.token = previousToken
        server.close()
        worker.join(4000)
        assertFalse("fixture thread must stop", worker.isAlive)
    }

    /**
     * A missing node is the symptom of whatever the screen actually did — a refused
     * connection, an unauthenticated request, a decode failure rendered as an error
     * message. Report that evidence rather than the bare timeout.
     */
    private fun awaitText(text: String) {
        try {
            compose.waitUntil(10_000) { compose.onAllNodesWithText(text).fetchSemanticsNodes().isNotEmpty() }
        } catch (timeout: ComposeTimeoutException) {
            fail(
                buildString {
                    appendLine("never rendered \"$text\" within 10s")
                    appendLine("origin=$origin boundAddress=${server.inetAddress} boundPort=${server.localPort}")
                    appendLine("acceptedConnections=${accepted.get()}")
                    appendLine("requests=${requests.toList()}")
                    appendLine("unexpectedPaths=${unexpectedPaths.toList()}")
                    appendLine("fixtureErrors=${fixtureErrors.toList()}")
                    appendLine("semantics=" + runCatching { compose.onRoot().printToString(maxDepth = 100) }
                        .getOrElse { "unreadable: $it" })
                },
            )
        }
    }

    @Test fun channelsAreSearchableAndProtectedChannelsCannotStart() {
        compose.setContent { PlurxTheme { LiveTvScreen(origin) {} } }
        awaitText("7.1 · Fixture News")
        compose.onNodeWithText("DRM unsupported").assertIsNotEnabled()
        // Initial focus is the 10-foot navigation contract, and it is what the
        // television profile must prove. On the phone profile this node was
        // observed with Focused = 'false': a touch device has no focus cursor to
        // place, and pinning one on Back would draw a focus ring nobody asked
        // for. Assert reachability there instead of a focus state the product
        // does not owe a touch screen.
        if (television) compose.onNodeWithText("Back").assertIsFocused()
        else compose.onNodeWithText("Back").assertHasClickAction()
        compose.onNodeWithText("Find a channel").performTextInput("Fixture")
        compose.onNodeWithText("7.1 · Fixture News").assertIsDisplayed()
        compose.onNodeWithText("Watch live").assertIsDisplayed()
        assertTrue(requests.none { it.startsWith("POST ") })
        assertEquals(emptyList<String>(), fixtureErrors.toList())
        assertEquals(emptyList<String>(), unexpectedPaths.toList())
    }

    @Test fun developerEnableIsASeparateGenerationBoundAction() {
        compose.setContent { PlurxTheme { LiveTvDeveloperScreen(origin) {} } }
        awaitText("Enable Live TV")
        compose.onNodeWithText("Enable Live TV").performScrollTo().performClick()
        awaitText("Disable Live TV and drain sessions")
        assertEquals(1, requests.count { it.startsWith("PUT /api/v1/settings ") })
        assertTrue(requests.none { it.startsWith("POST ") })
        assertEquals(emptyList<String>(), fixtureErrors.toList())
    }
}
