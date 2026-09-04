package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue

/**
 * The ask: publish evidence, then wait — briefly — for the verdict it earns.
 *
 * These drive the real [PlaybackControlSession] over a real
 * [PlaybackControlReporter], with only the HTTP client stubbed. The wiring
 * between the two is the whole milestone, and a test that stubbed the session
 * would leave exactly that free — which is how the Apple mirror shipped a
 * floor read after its own publish and nothing noticed.
 *
 * Real time rather than `runTest`'s virtual clock, deliberately: the transport
 * hops to `Dispatchers.IO`, so a virtual clock would run the ask's whole bound
 * out before the exchange it is waiting for could return, and the tests would
 * pass or flake for reasons unrelated to the code.
 */
class PlaybackControlAskTest {

    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    private fun scope() = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    /**
     * Answers the bootstrap exchange with `none` and everything after it with
     * [action].
     *
     * Answering the first one with the verdict under test would stop the
     * reporter before the ask exists — a terminal ends reporting, correctly —
     * and the ask would then be measuring that rather than the verdict. It is
     * also the real shape: the ask provokes a *later* exchange, which is what
     * the sequence floor is for.
     */
    private fun transport(
        action: String,
        reason: String? = null,
        message: String? = null,
        sequence: AtomicLong = AtomicLong(0),
    ): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor(
            Interceptor { chain ->
                val accepted = sequence.incrementAndGet()
                val answered = if (accepted == 1L) "none" else action
                val extra = if (accepted == 1L) "" else buildString {
                    if (reason != null) append(""","reason":"$reason"""")
                    if (message != null) append(""","code":"unsupported","message":"$message"""")
                }
                val body = """{"protocol":"${PlaybackControl.PROTOCOL}",""" +
                    """"generation":"$GENERATION","control_epoch":7,""" +
                    """"accepted_sequence":$accepted,""" +
                    """"action":{"type":"$answered"$extra}}"""
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body(body.toResponseBody("application/json".toMediaType()))
                    .build()
            },
        ).build()
        return PlaybackControlTransport("https://cinema.example", client, json)
    }

    private fun refusingTransport(): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor(
            Interceptor { chain ->
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(503)
                    .message("busy")
                    .body(
                        """{"code":"control_unavailable"}"""
                            .toResponseBody("application/json".toMediaType()),
                    )
                    .build()
            },
        ).build()
        return PlaybackControlTransport("https://cinema.example", client, json)
    }

    private fun ownerChangingTransport(sequence: AtomicLong): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor(
            Interceptor { chain ->
                val request = sequence.incrementAndGet()
                val status = if (request == 1L) 200 else 409
                val body = if (request == 1L) {
                    """{"protocol":"${PlaybackControl.PROTOCOL}",""" +
                        """"generation":"$GENERATION","control_epoch":7,""" +
                        """"accepted_sequence":1,"action":{"type":"none"}}"""
                } else {
                    """{"code":"owner_changed",""" +
                        """"generation":"44444444-4444-4444-8444-444444444444",""" +
                        """"control_epoch":9,"retry_after_ms":250}"""
                }
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(status)
                    .message(if (status == 200) "OK" else "owner changed")
                    .body(body.toResponseBody("application/json".toMediaType()))
                    .build()
            },
        ).build()
        return PlaybackControlTransport("https://cinema.example", client, json)
    }

    private fun heldTerminalTransport(
        sequence: AtomicLong,
        terminalArrived: CountDownLatch,
        releaseTerminal: CountDownLatch,
    ): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor(
            Interceptor { chain ->
                val accepted = sequence.incrementAndGet()
                val action = if (accepted == 2L) {
                    terminalArrived.countDown()
                    releaseTerminal.await(5, TimeUnit.SECONDS)
                    """{"type":"terminal","code":"unsupported","message":"stale intent"}"""
                } else {
                    """{"type":"none"}"""
                }
                val body = """{"protocol":"${PlaybackControl.PROTOCOL}",""" +
                    """"generation":"$GENERATION","control_epoch":7,""" +
                    """"accepted_sequence":$accepted,"action":$action}"""
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body(body.toResponseBody("application/json".toMediaType()))
                    .build()
            },
        ).build()
        return PlaybackControlTransport("https://cinema.example", client, json)
    }

    /**
     * The ask's floor is the reporter's own request counter, so it only means
     * anything once the reporter has built a request. `begin` launches the
     * pump, so a test that asks immediately reads a floor of zero and settles
     * on the bootstrap exchange — which in production cannot happen, because
     * a stall is minutes into a session.
     */
    private suspend fun awaitFirstExchange(sequence: AtomicLong) {
        val deadline = monotonicNowMs() + 5_000
        while (sequence.get() < 1 && monotonicNowMs() < deadline) {
            kotlinx.coroutines.delay(10)
        }
        assertTrue(sequence.get() >= 1, "the session never exchanged")
        kotlinx.coroutines.delay(50)
    }

    private fun bootstrap() = ControlBootstrap(
        protocol = PlaybackControl.PROTOCOL,
        url = "/api/v1/hls/session-1/control",
        generation = GENERATION,
        controlEpoch = 7,
        nextExchangeMs = PlaybackControl.MIN_EXCHANGE_MS,
        leaseTimeoutMs = 300_000,
    )

    private fun observation() = PlayerControlObservation(
        positionMs = 4_000,
        durationMs = 7_200_000,
        bufferedFromMs = 4_000,
        bufferedThroughMs = 34_000,
        rate = 1.0,
        isPaused = false,
        isEnded = false,
        isSeeking = false,
        hasStarted = true,
        isLikelyToKeepUp = true,
        selection = ClientSelection(
            quality = QualitySelection.Auto,
            audioTrack = 0,
            subtitle = SubtitleSelection(SubtitleMode.OFF),
            audioOffsetMs = 0,
            codec = CodecPolicy.AUTO,
            dynamicRange = DynamicRangePolicy.AUTO,
        ),
        capabilities = DynamicCapabilities(
            platform = "android",
            maxHeight = 2_160,
            codecs = listOf(CodecPolicy.H264),
            dynamicRanges = listOf(DynamicRangePolicy.SDR),
            dualPlayerPreparation = false,
        ),
    )

    @Test
    fun `an ask is answered by the exchange it provoked`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        try {
            val sequence = AtomicLong(0)
            session.begin(
                bootstrap(),
                ::observation,
                transport("hold", reason = "no_room", sequence = sequence),
            )
            awaitFirstExchange(sequence)
            var published = false
            val verdict = session.askForAction(
                boundMs = 5_000,
                capMs = 8_000,
                publish = { published = true },
            )
            assertTrue(published, "the evidence went out")
            assertEquals("hold", verdict?.type)
            assertEquals("no_room", verdict?.reason)
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `a terminal verdict answers the ask and arms the session`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        try {
            val sequence = AtomicLong(0)
            session.begin(
                bootstrap(),
                ::observation,
                transport("terminal", message = "No decoder for this.", sequence = sequence),
            )
            awaitFirstExchange(sequence)
            val verdict = session.askForAction(boundMs = 5_000, capMs = 8_000, publish = {})
            assertEquals("terminal", verdict?.type)
            assertEquals("No decoder for this.", session.terminalVerdict?.message)
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `clearing verdict fences a terminal response already in flight`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        val sequence = AtomicLong(0)
        val terminalArrived = CountDownLatch(1)
        val releaseTerminal = CountDownLatch(1)
        try {
            session.begin(
                bootstrap(),
                ::observation,
                heldTerminalTransport(sequence, terminalArrived, releaseTerminal),
            )
            awaitFirstExchange(sequence)
            session.playerChanged()
            assertTrue(terminalArrived.await(5, TimeUnit.SECONDS), "terminal request never arrived")

            session.clearVerdict()
            session.playerChanged()
            releaseTerminal.countDown()
            val deadline = monotonicNowMs() + 5_000
            while (sequence.get() < 3 && monotonicNowMs() < deadline) {
                kotlinx.coroutines.delay(10)
            }

            assertTrue(sequence.get() >= 3, "the stale terminal must not stop the reporter")
            assertNull(session.terminalVerdict, "the old viewer intent must not re-arm")
        } finally {
            releaseTerminal.countDown()
            session.end()
            scope.cancel()
        }
    }

    /**
     * The ask always settles. A stalled viewer waiting on something nothing
     * will resolve is worse than the guess the client would have made.
     */
    @Test
    fun `an ask with no reporter answers at once and publishes nothing`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        try {
            var published = false
            val verdict = session.askForAction(
                boundMs = 5_000,
                capMs = 8_000,
                publish = { published = true },
            )
            assertNull(verdict)
            assertFalse(published)
        } finally {
            scope.cancel()
        }
    }

    /**
     * Ruling D3's bound is real, not decorative: past it the client takes its
     * own path rather than waiting longer on a server that is not answering.
     * A refusal is not a verdict.
     */
    @Test
    fun `an ask nobody answers settles at its bound`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        try {
            session.begin(bootstrap(), ::observation, refusingTransport())
            val startedAt = monotonicNowMs()
            val verdict = session.askForAction(
                boundMs = CONTROL_ASK_MS,
                capMs = CONTROL_ASK_CAP_MS,
                publish = {},
            )
            assertNull(verdict)
            assertTrue(
                monotonicNowMs() - startedAt < CONTROL_ASK_CAP_MS * 3,
                "the bound is the bound",
            )
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `an owner change settles the current ask without waiting on the new owner`() = runBlocking {
        val scope = scope()
        val session = PlaybackControlSession(scope)
        try {
            val sequence = AtomicLong(0)
            session.begin(bootstrap(), ::observation, ownerChangingTransport(sequence))
            awaitFirstExchange(sequence)
            val startedAt = monotonicNowMs()
            val verdict = session.askForAction(boundMs = 5_000, capMs = 8_000, publish = {})
            assertNull(verdict)
            assertTrue(
                monotonicNowMs() - startedAt < 2_000,
                "the old ask is released while the reporter adopts the new owner",
            )
        } finally {
            session.end()
            scope.cancel()
        }
    }

    private companion object {
        const val GENERATION = "11111111-1111-4111-8111-111111111111"
    }
}
