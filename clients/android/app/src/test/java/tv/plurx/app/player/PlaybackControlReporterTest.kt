package tv.plurx.app.player

import tv.plurx.app.data.PlaybackQuality
import kotlinx.coroutines.delay
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.launch
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.currentTime
import kotlinx.coroutines.test.advanceTimeBy
import kotlinx.coroutines.test.runCurrent
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val GENERATION = "11111111-1111-4111-8111-111111111111"
private const val CLIENT_ID = "22222222-2222-4222-8222-222222222222"

private fun bootstrap(
    generation: String = GENERATION,
    epoch: Long = 7,
) = ControlBootstrap(
    protocol = PlaybackControl.PROTOCOL,
    url = "/api/v1/hls/session-1/control",
    generation = generation,
    controlEpoch = epoch,
    nextExchangeMs = 5_000,
    leaseTimeoutMs = 300_000,
)

private fun capabilities(maxHeight: Int = 2_160) = DynamicCapabilities(
    platform = "android",
    maxHeight = maxHeight,
    codecs = listOf(CodecPolicy.HEVC, CodecPolicy.H264),
    dynamicRanges = listOf(DynamicRangePolicy.DOLBY_VISION, DynamicRangePolicy.HDR10, DynamicRangePolicy.SDR),
    dualPlayerPreparation = false,
)

private fun snapshot(
    position: Long = 1_000,
    render: RenderState = RenderState.RENDERING,
    demand: PlaybackDemand = PlaybackDemand.ACTIVE,
    maxHeight: Int = 2_160,
    observation: ClientObservation? = null,
) = PlaybackControlSnapshot(
    demand = demand,
    positionMs = position,
    bufferedFromMs = position,
    bufferedThroughMs = position + 10_000,
    playbackRate = 1.0,
    renderState = render,
    selection = ClientSelection(
        quality = QualitySelection.Auto,
        audioTrack = 0,
        subtitle = SubtitleSelection(SubtitleMode.OFF),
        audioOffsetMs = 0,
        codec = CodecPolicy.AUTO,
        dynamicRange = DynamicRangePolicy.AUTO,
    ),
    capabilities = capabilities(maxHeight),
    observation = observation,
)

private fun accept(request: ControlRequest) = ControlResponse(
    protocol = PlaybackControl.PROTOCOL,
    generation = request.generation,
    controlEpoch = request.controlEpoch,
    acceptedSequence = request.sequence,
    action = ControlAction("none"),
)

/**
 * Every moving part the reporter depends on, driven by the test scheduler.
 *
 * `runTest`'s virtual clock is both the reporter's clock and its pacing sleep,
 * so a backoff of four seconds costs the test nothing and is still exercised
 * exactly. Anything not explicitly queued is accepted, so a test states only
 * the outcomes it cares about and the reporter's own cadence cannot run it
 * out of answers.
 */
private class Harness(private val scope: TestScope) {
    val requests = mutableListOf<ControlRequest>()
    val exchanges = mutableListOf<PlaybackControlReporter.Exchange>()
    val paces = mutableListOf<Long>()
    private val outcomes = ArrayDeque<Result<ControlResponse>>()
    var sourceRevision = 0L
    var current = snapshot()
        set(value) { field = value; sourceRevision += 1 }
    var intentGeneration = 0L
    var owner = PlaybackControlCaptureOwner("test-player", 1)
    var holds = false

    fun enqueue(values: List<Result<ControlResponse>>) = outcomes.addAll(values)

    fun enqueue(value: Result<ControlResponse>) = enqueue(listOf(value))

    val send: suspend (String, ControlRequest) -> ControlResponse = { _, request ->
        requests += request
        if (holds) {
            // Longer than the exchange deadline, so the deadline is what ends it.
            delay(PlaybackControl.EXCHANGE_DEADLINE_MS * 10)
        }
        (outcomes.removeFirstOrNull() ?: Result.success(accept(request))).getOrThrow()
    }

    val pace: suspend (Long) -> Unit = { milliseconds ->
        paces += milliseconds
        delay(milliseconds)
    }

    val now: () -> Long = { scope.currentTime }

    /**
     * The source stopped publishing. Not a hypothetical: a session empties its
     * capture slot synchronously on teardown, so every reporter that outlives
     * its session by one exchange reads exactly this.
     */
    var sourceIsGone = false
    val captureOf: () -> PlaybackControlCapture? = {
        if (sourceIsGone) null
        else PlaybackControlCapture(current, intentGeneration, owner, sourceRevision)
    }
    val onExchange: (PlaybackControlReporter.Exchange) -> Unit = { exchanges += it }
}

private fun reporter(harness: Harness, value: ControlBootstrap = bootstrap()) =
    PlaybackControlReporter.create(
        bootstrap = value,
        clientInstanceId = CLIENT_ID,
        owner = harness.owner,
        capture = harness.captureOf,
        send = harness.send,
        pace = harness.pace,
        now = harness.now,
        onExchange = harness.onExchange,
    )

class PlaybackControlBootstrapTest {
    @Test
    fun `a valid bootstrap is accepted`() {
        assertTrue(bootstrap().isValid)
    }

    @Test
    fun `a foreign protocol is refused`() {
        assertFalse(bootstrap().copy(protocol = "plurx-playback-control-v2").isValid)
    }

    @Test
    fun `only a session control path is accepted`() {
        listOf(
            "/api/v1/hls/session-1/control",
            "/api/v1/hls/9f0c/control",
        ).forEach { assertTrue(ControlBootstrap.isSessionControlPath(it), it) }

        listOf(
            "https://elsewhere.example/api/v1/hls/session-1/control",
            "//elsewhere.example/api/v1/hls/session-1/control",
            "/api/v1/hls/session-1/status",
            "/api/v1/hls/session-1/control/extra",
            "/api/v1/hls//control",
            "/api/v1/hls/../control",
            "/api/v2/hls/session-1/control",
            "control",
        ).forEach { assertFalse(ControlBootstrap.isSessionControlPath(it), it) }
    }

    @Test
    fun `a non uuid generation is refused`() {
        assertFalse(bootstrap().copy(generation = "session-1").isValid)
        assertFalse(bootstrap().copy(generation = "1111-1111-4111-8111-111111111111").isValid)
    }

    @Test
    fun `an exchange cadence outside the protocol bounds is refused`() {
        assertFalse(bootstrap().copy(nextExchangeMs = 100).isValid)
        assertFalse(bootstrap().copy(nextExchangeMs = 60_001).isValid)
    }

    @Test
    fun `a lease shorter than the exchange cadence is refused`() {
        assertFalse(bootstrap().copy(leaseTimeoutMs = 1_000).isValid)
    }

    @Test
    fun `a reporter refuses an invalid bootstrap or identity`() = runTest {
        val harness = Harness(this)
        assertNull(reporter(harness, bootstrap().copy(controlEpoch = 0)))
        assertNull(
            PlaybackControlReporter.create(
                bootstrap = bootstrap(),
                clientInstanceId = "not-a-uuid",
                owner = harness.owner,
                capture = harness.captureOf,
                send = harness.send,
                pace = harness.pace,
                now = harness.now,
            ),
        )
    }
}

class PlaybackControlExchangeTest {
    @Test
    fun `the first request carries the whole snapshot and its capabilities`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()
        subject.stop()

        val request = harness.requests.first()
        assertEquals(PlaybackControl.PROTOCOL, request.protocol)
        assertEquals(1, request.sequence)
        assertEquals(GENERATION, request.generation)
        assertEquals(7, request.controlEpoch)
        assertEquals(CLIENT_ID, request.clientInstanceId)
        assertEquals(PlaybackDemand.ACTIVE, request.demand)
        assertEquals(RenderState.RENDERING, request.renderState)
        assertEquals(1_000, request.positionMs)
        assertEquals(11_000, request.bufferedThroughMs)
        assertEquals(capabilities(), request.capabilities)
        assertEquals(1, subject.status().acceptedSequence)
    }

    @Test
    fun `unchanged capabilities are sent once and a change resends them`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(20_001)
        val beforeChange = harness.requests.toList()
        harness.current = snapshot(position = 3_000, maxHeight = 1_080)
        subject.notify()
        advanceTimeBy(10_001)
        subject.stop()

        assertTrue(beforeChange.size >= 4, "the cadence should have produced several exchanges")
        assertEquals(
            capabilities(),
            beforeChange.first().capabilities,
            "the first exchange of a generation must carry them",
        )
        assertTrue(
            beforeChange.drop(1).all { it.capabilities == null },
            "unchanged capabilities are already on the server",
        )
        assertNotNull(
            harness.requests.firstOrNull { it.capabilities == capabilities(1_080) },
            "a capability change must be resent",
        )
    }

    @Test
    fun `sequences are monotonic and never reused`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(25_001)
        subject.stop()

        val sequences = harness.requests.map { it.sequence }
        assertTrue(sequences.size >= 4, "expected several exchanges, got $sequences")
        assertEquals((1L..sequences.size).toList(), sequences)
    }

    @Test
    fun `the newest snapshot wins while an exchange is in flight`() = runTest {
        val harness = Harness(this)
        harness.holds = true
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()

        harness.current = snapshot(position = 5_000)
        subject.notify()
        harness.current = snapshot(position = 9_000)
        subject.notify()

        val status = subject.status()
        assertTrue(status.inFlight)
        assertTrue(status.pending, "one pending snapshot, not two")
        assertEquals(1, harness.requests.size, "no second exchange overlaps the first")
        subject.stop()
    }

    @Test
    fun `an end demand is the last exchange`() = runTest {
        val harness = Harness(this)
        harness.current = snapshot(render = RenderState.ENDED, demand = PlaybackDemand.END)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)

        assertTrue(subject.isStopped())
        assertEquals(1, harness.requests.size)
    }
}

class PlaybackControlRefusalTest {
    @Test
    fun `a response for another generation is terminal`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    "33333333-3333-4333-8333-333333333333",
                    7,
                    1,
                    ControlAction("none"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals(1, harness.requests.size)
        assertEquals("protocol:generation", harness.exchanges.last().failure)
    }

    @Test
    fun `a response acknowledging another sequence is terminal`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(PlaybackControl.PROTOCOL, GENERATION, 7, 99, ControlAction("none")),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals("protocol:accepted_sequence", harness.exchanges.last().failure)
    }

    @Test
    fun `an action other than none is terminal rather than obeyed`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    // A type this client has genuinely never heard of. It used
                    // to be the literal `"prepare_replacement"`, which is the
                    // name a client *declares* and never a tag the server
                    // sends — so once the `prepare` handler landed this test
                    // would have gone on passing while its name and its point
                    // became false. The property it protects, that an
                    // unrecognised action is fatal, is the one the new
                    // vocabulary must not weaken.
                    ControlAction("promote_replacement"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals("protocol:action", harness.exchanges.last().failure)
        assertEquals(1, harness.requests.size)
    }

    @Test
    fun `a hold is an explanation and keeps the reporter running`() = runTest {
        // The point of declaring the action. The server is saying production is
        // deliberately not advancing; a client that stopped reporting there
        // would go silent for the rest of the film exactly when the server had
        // just explained itself.
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    ControlAction("hold", "working_set"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertFalse(subject.isStopped(), "a hold is not a reason to stop reporting")
        assertNull(harness.exchanges.first().failure)
        subject.stop()
    }

    @Test
    fun `an unrecognised hold reason is still a hold`() = runTest {
        // A reason this client has never heard of is a newer server, not a
        // broken one. Refusing over one unknown word is the same silence by a
        // different route.
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    ControlAction("hold", "a_reason_from_next_year"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertFalse(subject.isStopped())
        subject.stop()
    }

    @Test
    fun `a terminal verdict ends reporting without a protocol error`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    ControlAction("terminal", code = "unsupported", message = "no"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped(), "a terminal verdict ends reporting")
        assertNull(harness.exchanges.first().failure)
    }

    @Test
    fun `a retry is an answer and keeps the reporter running`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    ControlAction("retry_resource", reason = "reader_failed", afterMs = 9_000),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertFalse(subject.isStopped(), "a retry is not a reason to stop reporting")
        assertNull(harness.exchanges.first().failure)
        subject.stop()
    }

    @Test
    fun `a verdict missing the field it would be acted on is terminal`() = runTest {
        // Inside the declared vocabulary but unusable. Worse than an unknown
        // action, because this one would be acted on.
        listOf(
            ControlAction("terminal", message = "no code"),
            ControlAction("retry_resource", reason = "reader_failed"),
            ControlAction("retry_resource", reason = "reader_failed", afterMs = 0),
            ControlAction("retry_resource", reason = "reader_failed", afterMs = 60_001),
        ).forEach { action ->
            val harness = Harness(this)
            harness.enqueue(
                Result.success(
                    ControlResponse(PlaybackControl.PROTOCOL, GENERATION, 7, 1, action),
                ),
            )
            val subject = assertNotNull(reporter(harness))
            subject.start(backgroundScope)
            advanceTimeBy(30_001)
            assertTrue(subject.isStopped(), "$action must stop the reporter")
            assertEquals("protocol:action", harness.exchanges.last().failure)
        }
    }

    @Test
    fun `a hold without its reason is terminal`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(PlaybackControl.PROTOCOL, GENERATION, 7, 1, ControlAction("hold")),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals("protocol:action", harness.exchanges.last().failure)
    }

    @Test
    fun `the request declares the actions this client accepts`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(PlaybackControl.PROTOCOL, GENERATION, 7, 1, ControlAction("none")),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertEquals(
            listOf("hold", "retry_resource", "terminal", "prepare_replacement"),
            harness.requests.first().supportedActions,
        )
        subject.stop()
    }
}

class PlaybackControlFailureTest {
    @Test
    fun `a retryable control failure replays the exact request`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(ControlTransportException(status = 429, code = "control_rate_limited")),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1_001)
        subject.stop()

        assertTrue(harness.requests.size >= 2)
        assertEquals(1, harness.requests[0].sequence)
        assertEquals(1, harness.requests[1].sequence, "a refused exchange does not consume a sequence")
        assertEquals(harness.requests[0], harness.requests[1], "the replay is the same request")
    }

    @Test
    fun `a transport failure with no status is retried`() = runTest {
        val harness = Harness(this)
        harness.enqueue(Result.failure(ControlTransportException()))
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(6_001)
        assertFalse(subject.isStopped())
        subject.stop()
        assertEquals(listOf(1L, 1L), harness.requests.take(2).map { it.sequence })
    }

    @Test
    fun `an unauthorized exchange stops the reporter`() = runTest {
        val harness = Harness(this)
        harness.enqueue(Result.failure(ControlTransportException(status = 403, code = "forbidden")))
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals(1, harness.requests.size)
    }

    @Test
    fun `pause grace expiry stops without reopening or retrying`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(status = 410, code = "pause_grace_expired"),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals(1, harness.requests.size)
    }

    @Test
    fun `a retry honours the server's retry-after`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(
                    status = 503,
                    code = "control_unavailable",
                    retryAfterMs = 4_000,
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(5_001)
        subject.stop()
        assertTrue(
            harness.paces.contains(4_000),
            "the server asked for 4s and got 4s, not the 500ms default: ${harness.paces}",
        )
    }

    @Test
    fun `a retry without a retry-after uses the control backoff`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(ControlTransportException(status = 425, code = "owner_transition")),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1_001)
        subject.stop()
        assertTrue(harness.paces.contains(500), "${harness.paces}")
    }

    @Test
    fun `an absurd retry-after is ignored in favour of the default`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(
                    status = 429,
                    code = "control_rate_limited",
                    retryAfterMs = 999_999,
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1_001)
        subject.stop()
        assertFalse(harness.paces.contains(999_999))
        assertTrue(harness.paces.contains(500), "${harness.paces}")
    }

    @Test
    fun `an exchange that never answers ends at the deadline`() = runTest {
        val harness = Harness(this)
        harness.holds = true
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(PlaybackControl.EXCHANGE_DEADLINE_MS * 3)
        subject.stop()

        assertEquals("transport:408:exchange_deadline", harness.exchanges.first().failure)
        assertTrue(
            harness.requests.size >= 2,
            "the deadline frees the in-flight slot rather than stranding it",
        )
        assertEquals(
            listOf(1L, 1L),
            harness.requests.take(2).map { it.sequence },
            "a timed-out exchange was never accepted, so it keeps its sequence",
        )
    }
}

class PlaybackControlOwnerChangeTest {
    private val newGeneration = "44444444-4444-4444-8444-444444444444"

    @Test
    fun `an owner change adopts the new generation and restarts the sequence`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(
                    status = 409,
                    code = "owner_changed",
                    generation = newGeneration,
                    controlEpoch = 9,
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1_001)
        subject.stop()

        assertTrue(harness.requests.size >= 2)
        assertEquals(GENERATION, harness.requests[0].generation)
        assertEquals(newGeneration, harness.requests[1].generation)
        assertEquals(9, harness.requests[1].controlEpoch)
        assertEquals(1, harness.requests[1].sequence, "a new owner issued no sequence yet")
        assertEquals(
            capabilities(),
            harness.requests[1].capabilities,
            "the new owner has never been told this player's capabilities",
        )
    }

    @Test
    fun `an owner change naming no newer owner is not adopted`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(
                    status = 409,
                    code = "owner_changed",
                    generation = GENERATION,
                    controlEpoch = 7,
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped(), "a 409 that names the current owner is not a handoff")
        assertEquals(GENERATION, subject.bootstrap.generation)
        assertEquals(7, subject.bootstrap.controlEpoch)
    }

    @Test
    fun `an owner change with a malformed generation is not adopted`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.failure(
                ControlTransportException(
                    status = 409,
                    code = "owner_changed",
                    generation = "elsewhere",
                    controlEpoch = 9,
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals(GENERATION, subject.bootstrap.generation)
    }
}

class PlaybackControlSnapshotTest {
    @Test
    fun `a snapshot whose runway precedes its position is refused`() {
        assertFalse(snapshot().copy(bufferedThroughMs = 999).isValid)
    }

    @Test
    fun `a manual quality outside the encoder range is refused`() {
        val base = snapshot()
        assertFalse(
            base.copy(
                selection = base.selection.copy(quality = QualitySelection.Manual(4_320)),
            ).isValid,
        )
        assertTrue(
            base.copy(
                selection = base.selection.copy(quality = QualitySelection.Manual(1_080)),
            ).isValid,
        )
    }

    @Test
    fun `capabilities must name at least one codec and range`() {
        val base = snapshot()
        assertFalse(base.copy(capabilities = base.capabilities.copy(codecs = emptyList())).isValid)
        assertFalse(
            base.copy(capabilities = base.capabilities.copy(dynamicRanges = emptyList())).isValid,
        )
    }

    @Test
    fun `an invalid snapshot is not reported`() = runTest {
        val harness = Harness(this)
        harness.current = snapshot().copy(bufferedThroughMs = -1)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        subject.stop()
        assertEquals(
            0,
            harness.requests.size,
            "a player state the server would reject is not sent",
        )
    }
}

class PlaybackControlObservationTest {
    @Test
    fun `detail without a code is dropped`() {
        assertNull(ClientObservation(errorDetail = "went wrong").bounded())
    }

    @Test
    fun `detail is flattened and bounded`() {
        val bounded = assertNotNull(
            ClientObservation(
                droppedFrames = 12,
                decoderState = DecoderState.STARVED,
                errorCode = ClientErrorCode.DECODER,
                errorDetail = "bad\r\nline\u0000break " + "e".repeat(900),
            ).bounded(),
        )
        val detail = assertNotNull(bounded.errorDetail)
        assertTrue(detail.toByteArray(Charsets.UTF_8).size <= PlaybackControl.MAX_ERROR_DETAIL_BYTES)
        assertFalse(detail.contains('\r'))
        assertFalse(detail.contains('\n'))
        assertFalse(detail.contains('\u0000'))
        assertTrue(detail.startsWith("bad  line break "))
        assertEquals(12, bounded.droppedFrames)
        assertEquals(DecoderState.STARVED, bounded.decoderState)
    }

    @Test
    fun `a negative dropped frame count is dropped`() {
        val bounded = assertNotNull(
            ClientObservation(droppedFrames = -1, decoderState = DecoderState.READY).bounded(),
        )
        assertNull(bounded.droppedFrames)
        assertEquals(DecoderState.READY, bounded.decoderState)
    }
}

class PlaybackControlWireTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    @Test
    fun `the request encodes the server's field names`() {
        val encoded = json.encodeToString(
            ControlRequest.serializer(),
            ControlRequest(
                protocol = PlaybackControl.PROTOCOL,
                generation = GENERATION,
                controlEpoch = 7,
                clientInstanceId = CLIENT_ID,
                sequence = 3,
                demand = PlaybackDemand.HOLD,
                positionMs = 12,
                bufferedFromMs = 12,
                bufferedThroughMs = 34,
                playbackRate = 0.0,
                renderState = RenderState.WAITING,
                seekTargetMs = 56,
                observedDownloadBps = 78,
                selection = ClientSelection(
                    quality = QualitySelection.Manual(1_080),
                    audioTrack = 1,
                    subtitle = SubtitleSelection(SubtitleMode.OVERLAY, 2),
                    audioOffsetMs = -40,
                    codec = CodecPolicy.HEVC,
                    dynamicRange = DynamicRangePolicy.DOLBY_VISION,
                ),
                capabilities = capabilities(),
                observation = ClientObservation(
                    droppedFrames = 9,
                    decoderState = DecoderState.READY,
                ),
                supportedActions = PlaybackControl.SUPPORTED_ACTIONS,
            ),
        )
        listOf(
            "\"protocol\":", "\"control_epoch\":", "\"client_instance_id\":", "\"position_ms\":",
            "\"buffered_from_ms\":", "\"buffered_through_ms\":", "\"playback_rate\":",
            "\"render_state\":", "\"seek_target_ms\":", "\"observed_download_bps\":",
            "\"audio_track\":", "\"audio_offset_ms\":", "\"dynamic_range\":",
            "\"max_height\":", "\"dynamic_ranges\":", "\"dual_player_preparation\":",
            "\"dropped_frames\":", "\"decoder_state\":", "\"supported_actions\":",
        ).forEach { assertTrue(encoded.contains(it), "missing $it in $encoded") }
        assertTrue(encoded.contains("\"mode\":\"manual\""))
        assertTrue(encoded.contains("\"height\":1080"))
        assertTrue(encoded.contains("\"dynamic_range\":\"dolby_vision\""))
        assertTrue(encoded.contains("\"render_state\":\"waiting\""))
        assertTrue(encoded.contains("\"demand\":\"hold\""))
        assertTrue(
            encoded.contains(
                "\"supported_actions\":" +
                    "[\"hold\",\"retry_resource\",\"terminal\",\"prepare_replacement\"]",
            ),
        )
        assertFalse(encoded.contains("\"error_detail\""), "explicitNulls is off; absent means absent")
    }

    @Test
    fun `an auto quality is tagged on mode with no other field`() {
        val encoded = json.encodeToString(QualitySelection.serializer(), QualitySelection.Auto)
        assertEquals("{\"mode\":\"auto\"}", encoded)
    }

    @Test
    fun `original is explicit even when source dimensions are unavailable`() {
        val encoded = json.encodeToString(QualitySelection.serializer(), QualitySelection.Original)
        assertEquals("{\"mode\":\"original\"}", encoded)
    }

    @Test
    fun `the bootstrap decodes the server's field names`() {
        val decoded = json.decodeFromString(
            ControlBootstrap.serializer(),
            """
            {"protocol":"plurx-playback-control-v1",
             "url":"/api/v1/hls/abc/control",
             "generation":"11111111-1111-4111-8111-111111111111",
             "control_epoch":7,"next_exchange_ms":5000,"lease_timeout_ms":60000}
            """.trimIndent(),
        )
        assertTrue(decoded.isValid)
        assertEquals(7, decoded.controlEpoch)
        assertEquals(5_000, decoded.nextExchangeMs)
        assertEquals(60_000, decoded.leaseTimeoutMs)
    }

    @Test
    fun `the response decodes and consumes subtitle readiness`() {
        val decoded = json.decodeFromString(
            ControlResponse.serializer(),
            """
            {"protocol":"plurx-playback-control-v1",
             "generation":"11111111-1111-4111-8111-111111111111",
             "control_epoch":7,"accepted_sequence":4,"server_time_unix_ms":1,
             "lease":{"state":"active","renew_after_ms":5000,"expires_at_unix_ms":2},
             "delivery":{"subtitle_readiness":"warming"},
             "effective_selection":{"quality_auto":true,"height":1080,
               "audio_offset_ms":0,"codec":"server_selected"},
             "action":{"type":"none"}}
            """.trimIndent(),
        )
        assertEquals(4, decoded.acceptedSequence)
        assertEquals("warming", decoded.delivery?.subtitleReadiness)
        assertEquals("none", decoded.action.type)
        // `effective_selection` used to be an ignored key and this fixture
        // carried `{}` to prove it was ignored. The client consumes it now, and
        // the server declares four of its fields plainly — so the fixture
        // carries what a server actually sends.
        assertEquals(1_080L, decoded.effectiveSelection?.height)
    }

    @Test
    fun `a server that sends no effective selection is still understood`() {
        // The field is informational on the response path, and a server old
        // enough to omit it is not a server this client should stop reporting
        // to.
        val decoded = json.decodeFromString(
            ControlResponse.serializer(),
            """
            {"protocol":"plurx-playback-control-v1",
             "generation":"11111111-1111-4111-8111-111111111111",
             "control_epoch":7,"accepted_sequence":4,"action":{"type":"none"}}
            """.trimIndent(),
        )
        assertNull(decoded.effectiveSelection)
    }

    @Test
    fun `subtitle readiness is closed and a ready edge retries once`() {
        mapOf(
            "ready" to true,
            "warming" to false,
            "unavailable" to false,
            "a_value_from_next_year" to false,
            "" to false,
        ).forEach { (value, expected) ->
            assertEquals(expected, SubtitleReadinessDecision.meansReady(value), value)
        }
        assertFalse(SubtitleReadinessDecision.meansReady(null))

        val transition = SubtitleReadinessRetryState()
        assertFalse(transition.record(null))
        assertFalse(transition.record("warming"))
        assertTrue(transition.record("ready"))
        assertFalse(transition.record("ready"), "control cadence cannot retry again")
    }
}

/**
 * Reporting now rather than at the server's cadence.
 *
 * `notify` only replaces the waiting snapshot; the pump stays asleep for
 * `next_exchange_ms`, which the server may set as high as a minute. That is
 * right for a position update and fatal for a recovery owner: the owner's own
 * reopen ends the reporter before the pump wakes, so its evidence is not sent
 * late — it is never sent at all. The web reporter has always drained inline
 * at that call site for exactly this reason.
 */
class PlaybackControlUrgentNotifyTest {

    @Test
    fun `terminal latch cannot silence newer source before or after stop`() = runTest {
        for (transition in listOf("callback", "ordinary", "urgent", "explicit", "end", "protocol")) {
            val owner = PlaybackControlCaptureOwner("terminal-source", 1)
            var latest = PlaybackControlCapture(
                if (transition == "end") snapshot().copy(demand = PlaybackDemand.END) else snapshot(),
                0, owner, 0,
            )
            val next = PlaybackControlCapture(snapshot(position = 9_000), 1, owner, 1)
            val calls = mutableListOf<ControlRequest>()
            lateinit var subject: PlaybackControlReporter
            subject = assertNotNull(PlaybackControlReporter.create(
                bootstrap(), CLIENT_ID, owner, { latest },
                send = { _, request ->
                    calls += request
                    if (calls.size != 1) accept(request)
                    else if (transition == "protocol") accept(request).copy(generation = "44444444-4444-4444-8444-444444444444")
                    else accept(request).copy(action = ControlAction(type = "terminal", code = "unsupported", message = "A"))
                },
                pace = { delay(it) }, now = { currentTime },
                onExchange = {
                    if (transition == "callback" && it.request.sequence == 1L) {
                        latest = next
                        launch { subject.notifyUrgently(this@runTest, next) }
                    }
                },
            ))
            subject.start(this); runCurrent()
            if (transition != "callback") {
                assertTrue(subject.isStopped())
                subject.notify(latest)
                assertTrue(subject.isStopped(), "same intent cannot revive its terminal verdict")
                if (transition == "explicit") subject.stop()
                latest = next
                if (transition == "ordinary") subject.notify(next)
                else subject.notifyUrgently(this, next)
            }
            advanceTimeBy(251); runCurrent()
            val permanent = transition in listOf("explicit", "end", "protocol")
            assertEquals(if (permanent) 1 else 2, calls.size, transition)
            assertEquals(permanent, subject.isStopped(), transition)
            if (!permanent) assertEquals(9_000L, calls[1].positionMs)
            subject.stop()
        }
    }

    @Test
    fun `owner reset survives cleared source and waits for its own attachment`() = runTest {
        for (foreignOwner in listOf(false, true)) {
            val owner = PlaybackControlCaptureOwner("source", 1)
            var latest: PlaybackControlCapture? = PlaybackControlCapture(snapshot(), 0, owner, 0)
            val calls = mutableListOf<ControlRequest>()
            val first = CompletableDeferred<ControlResponse>()
            val subject = assertNotNull(PlaybackControlReporter.create(
                bootstrap(), CLIENT_ID, owner, { latest },
                send = { _, request -> calls += request; if (calls.size == 1) first.await() else accept(request) },
                pace = { delay(it) }, now = { currentTime },
            ))
            subject.start(this); runCurrent()
            latest = null
            val nextGeneration = "44444444-4444-4444-8444-444444444444"
            first.completeExceptionally(ControlTransportException(
                status = 409, code = "owner_changed", generation = nextGeneration, controlEpoch = 8,
            ))
            runCurrent(); advanceTimeBy(251); runCurrent()
            assertFalse(subject.isStopped(), "temporary capture gap cannot silence reporting")
            assertEquals(1, calls.size, "owner adoption sends no guessed or old body")
            latest = PlaybackControlCapture(snapshot(position = 9_000), 1,
                if (foreignOwner) owner.copy(attachmentGeneration = 2) else owner, 1)
            val floor = subject.notifyUrgently(this, latest)
            runCurrent()
            if (foreignOwner) {
                assertNull(floor)
                assertEquals(1, calls.size, "the old reporter cannot borrow attachment B")
            } else {
                assertEquals(1L, floor)
                assertEquals(2, calls.size)
                assertEquals(nextGeneration, calls[1].generation)
                assertEquals(8L, calls[1].controlEpoch)
                assertEquals(1L, calls[1].sequence)
                assertEquals(9_000L, calls[1].positionMs)
            }
            subject.stop()
        }
    }

    @Test
    fun `reversed enqueue keeps newest source for changed and unchanged intent`() = runTest {
        for (sameIntent in listOf(false, true)) {
            val owner = PlaybackControlCaptureOwner("ordered-source", 1)
            var latest: PlaybackControlCapture? = PlaybackControlCapture(snapshot(), 0, owner, 0)
            val calls = mutableListOf<ControlRequest>()
            val exchanges = mutableListOf<PlaybackControlReporter.Exchange>()
            val first = CompletableDeferred<ControlResponse>()
            val subject = assertNotNull(PlaybackControlReporter.create(
                bootstrap(), CLIENT_ID, owner, { latest },
                send = { _, request -> calls += request; if (calls.size == 1) first.await() else accept(request) },
                pace = { delay(it) }, now = { currentTime }, onExchange = { exchanges += it },
            ))
            subject.start(this); runCurrent()
            val older = PlaybackControlCapture(snapshot(position = 1_000), 1, owner, 1)
            val newer = PlaybackControlCapture(snapshot(position = 9_000), if (sameIntent) 1 else 2, owner, 2)
            latest = newer
            subject.notify(newer)
            subject.notifyUrgently(this, older) // actor/dispatcher delivery reversed
            first.complete(accept(calls[0])); runCurrent()
            advanceTimeBy(251); runCurrent()
            assertEquals(9_000L, calls[1].positionMs)
            assertTrue(exchanges[1].capture === newer)
            subject.stop()
        }
    }

    @Test
    fun `cleared source suspends queued work and old reporter never adopts another attachment`() = runTest {
        for (replaceOwner in listOf(false, true)) {
            val owner = PlaybackControlCaptureOwner("lifetime", 1)
            val captured = PlaybackControlCapture(snapshot(position = 1_000), 1, owner, 1)
            var latest: PlaybackControlCapture? = captured
            val calls = mutableListOf<ControlRequest>()
            val subject = assertNotNull(PlaybackControlReporter.create(
                bootstrap(), CLIENT_ID, owner, { latest },
                send = { _, request -> calls += request; accept(request) },
                pace = { delay(it) }, now = { currentTime },
            ))
            subject.notify(captured)
            latest = if (replaceOwner) PlaybackControlCapture(
                snapshot(position = 9_000), 2, PlaybackControlCaptureOwner("lifetime", 2), 2,
            ) else null
            subject.start(this); runCurrent()
            advanceTimeBy(5_001); runCurrent()
            assertTrue(calls.isEmpty(), "queued and cadence admission must use the bound current attachment")
            assertNull(subject.notifyUrgently(this, captured), "late explicit enqueue cannot resurrect cleared ownership")
            subject.stop()
        }
    }

    @Test
    fun `started capture keeps source intent through mutation retry and coalescing`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        val codecs = mutableListOf(CodecPolicy.H264)
        harness.current = snapshot(position = 1_000).copy(
            capabilities = snapshot().capabilities.copy(codecs = codecs),
        )
        harness.intentGeneration = 1
        val captured = assertNotNull(harness.captureOf())
        harness.enqueue(Result.failure(ControlTransportException(status = 429, code = "control_rate_limited")))
        subject.notifyUrgently(this, captured)
        runCurrent()
        // Once started, a refused request retains its exact source capture.
        codecs += CodecPolicy.AUTO
        harness.current = snapshot(position = 9_000)
        harness.intentGeneration = 2
        assertEquals(1_000, harness.requests.single().positionMs)
        assertEquals(listOf(CodecPolicy.H264), harness.requests.single().capabilities?.codecs)
        val second = assertNotNull(harness.captureOf())
        subject.notify(second)
        harness.current = snapshot(position = 12_000)
        harness.intentGeneration = 3
        val newest = assertNotNull(harness.captureOf())
        subject.notify(newest)
        advanceTimeBy(501)
        runCurrent()
        advanceTimeBy(251)
        runCurrent()
        assertEquals(listOf(1_000L, 1_000L, 12_000L), harness.requests.take(3).map { it.positionMs })
        assertTrue(harness.exchanges[0].capture === captured)
        assertTrue(harness.exchanges[1].capture === captured, "retry retains the same immutable capture")
        assertTrue(harness.exchanges[2].capture === newest, "coalescing never combines capture fields")
        subject.stop()
    }

    @Test
    fun `terminal from previous attachment cannot stop reused intent number`() = runTest {
        val harness = Harness(this)
        val result = CompletableDeferred<ControlResponse>()
        val subject = assertNotNull(PlaybackControlReporter.create(
            bootstrap = bootstrap(), clientInstanceId = CLIENT_ID,
            owner = harness.owner,
            capture = harness.captureOf,
            send = { _, request -> harness.requests += request; result.await() },
            pace = harness.pace, now = harness.now, onExchange = harness.onExchange,
        ))
        subject.start(this)
        runCurrent()
        val original = harness.requests.single()
        harness.owner = PlaybackControlCaptureOwner("test-player", 2)
        harness.current = snapshot(position = 9_000)
        result.complete(accept(original).copy(action = ControlAction(
            type = "terminal", code = "unsupported", message = "old attachment",
        )))
        runCurrent()
        assertFalse(subject.status().stopped)
        assertEquals(1, harness.exchanges.single().capture.owner.attachmentGeneration)
        subject.stop()
    }

    @Test
    fun `an ordinary report waits out the server's cadence`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(this)
        runCurrent()
        assertEquals(1, harness.requests.size)

        subject.notify()
        advanceTimeBy(PlaybackControl.MIN_EXCHANGE_MS * 4)
        runCurrent()
        assertEquals(1, harness.requests.size, "the cadence is unchanged for a position update")
        subject.stop()
    }

    @Test
    fun `an urgent report is sent as soon as the rate limit allows`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(this)
        runCurrent()
        assertEquals(1, harness.requests.size)
        val evidence = ClientObservation(
            decoderState = DecoderState.STARVED,
            errorCode = ClientErrorCode.NETWORK,
            errorDetail = "stall",
        )
        harness.current = snapshot(observation = evidence)

        assertEquals(2L, subject.notifyUrgently(this), "the replacement can retain this request floor")
        advanceTimeBy(PlaybackControl.MIN_EXCHANGE_MS + 1)
        runCurrent()

        assertEquals(2, harness.requests.size, "the evidence went out without waiting")
        assertEquals(DecoderState.STARVED, harness.requests[1].observation?.decoderState)
        assertEquals(ClientErrorCode.NETWORK, harness.requests[1].observation?.errorCode)
        subject.stop()
    }

    /**
     * The swap that wakes the pump is one critical section. Releasing the lock
     * between clearing and setting it would let a concurrent `start()` — whose
     * guard is `pump != null` — launch a second run loop, and `stop()` can
     * only cancel the one it can see.
     */
    @Test
    fun `waking the pump does not leave a second one running`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(this)
        runCurrent()
        harness.current = snapshot(position = 2_000)
        subject.notifyUrgently(this)
        subject.start(this)
        assertNotNull(subject.notifyUrgently(this))
        advanceTimeBy(PlaybackControl.MIN_EXCHANGE_MS * 2)
        runCurrent()
        subject.stop()
        val after = harness.requests.size

        // A stopped reporter has no pump left anywhere. If the swap had
        // orphaned one, it would keep exchanging past the stop.
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(after, harness.requests.size, "nothing exchanges after stop")
        assertTrue(subject.status().stopped)
    }

    /**
     * An urgent report from a reporter that has already stopped is a no-op
     * rather than a resurrection: a terminal verdict ends reporting, and a
     * recovery owner firing afterwards must not restart it.
     */
    @Test
    fun `a stopped reporter cannot be woken`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(this)
        runCurrent()
        val before = harness.requests.size
        subject.stop()

        assertNull(subject.notifyUrgently(this))
        advanceTimeBy(60_000)
        runCurrent()
        assertEquals(before, harness.requests.size)
    }
}

/**
 * The prepared-replacement half of the protocol, at the reporter's boundary.
 *
 * These assert the *encoded* request and the *decoded* response rather than the
 * in-memory objects, because the serializer is what ships: `explicitNulls` is
 * off and `encodeDefaults` is on nowhere, so a field can be perfectly correct
 * in Kotlin and simply absent on the wire — which is the failure
 * `supported_actions` already carries a comment about.
 */
class PlaybackControlPreparedReplacementTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    private val actionId = "33333333-3333-4333-8333-333333333333"
    private val successor = "44444444-4444-4444-8444-444444444444"

    private fun preparedAction(
        playlistUrl: String = "/api/v1/hls/$successor/index.m3u8",
    ) = ControlAction(
        type = "prepare",
        actionId = actionId,
        sessionId = successor,
        playlistUrl = playlistUrl,
        mediaOriginMs = 0,
        effectiveSelection = EffectiveSelection(
            qualityAuto = true,
            height = 1_080,
            audioOffsetMs = 0,
            codec = "server_selected",
            dynamicRange = "sdr",
        ),
    )

    private fun request(
        demand: PlaybackDemand = PlaybackDemand.ACTIVE,
        acknowledgement: ActionAcknowledgement? = null,
    ) = ControlRequest(
        protocol = PlaybackControl.PROTOCOL,
        generation = GENERATION,
        controlEpoch = 7,
        clientInstanceId = CLIENT_ID,
        sequence = 4,
        demand = demand,
        positionMs = 1_000,
        bufferedThroughMs = 11_000,
        playbackRate = 1.0,
        renderState = RenderState.RENDERING,
        observedDownloadBps = 42_000_000,
        selection = ClientSelection(
            quality = QualitySelection.Auto,
            audioTrack = 0,
            subtitle = SubtitleSelection(SubtitleMode.OFF),
            audioOffsetMs = 0,
            codec = CodecPolicy.AUTO,
            dynamicRange = DynamicRangePolicy.AUTO,
        ),
        capabilities = capabilities(),
        acknowledgement = acknowledgement,
        supportedActions = PlaybackControl.SUPPORTED_ACTIONS,
    )

    @Test
    fun `the encoded request declares the prepared vocabulary`() {
        // Asserted against the serialized body rather than the in-memory list:
        // a server that never sees the vocabulary never sends the action, and
        // the object being right is not the same claim as the bytes being
        // right.
        val encoded = json.encodeToString(ControlRequest.serializer(), request())
        assertTrue(
            encoded.contains(
                "\"supported_actions\":" +
                    "[\"hold\",\"retry_resource\",\"terminal\",\"prepare_replacement\"]",
            ),
            encoded,
        )
    }

    @Test
    fun `observed download bps reaches the wire`() {
        // The throughput floor needs both this number and the server's own, and
        // a client that reports null can never be offered a preparation
        // whatever its capability says.
        val encoded = json.encodeToString(ControlRequest.serializer(), request())
        assertTrue(encoded.contains("\"observed_download_bps\":42000000"), encoded)
    }

    @Test
    fun `the three terminal acknowledgements encode the server's field names`() {
        val metadata = json.encodeToString(
            ControlRequest.serializer(),
            request(
                acknowledgement = ActionAcknowledgement(
                    actionId,
                    AcknowledgementState.METADATA_READY,
                ),
            ),
        )
        assertTrue(metadata.contains("\"acknowledgement\":{"), metadata)
        assertTrue(metadata.contains("\"action_id\":\"$actionId\""), metadata)
        assertTrue(metadata.contains("\"state\":\"metadata_ready\""), metadata)
        // `explicitNulls` is off, so the two optional numbers are absent rather
        // than null — which is what the server's `Option` fields want.
        assertFalse(metadata.contains("\"buffered_through_ms\":null"), metadata)

        val buffered = json.encodeToString(
            ControlRequest.serializer(),
            request(
                acknowledgement = ActionAcknowledgement(
                    actionId,
                    AcknowledgementState.BUFFER_READY,
                    bufferedThroughMs = 90_000,
                ),
            ),
        )
        assertTrue(buffered.contains("\"state\":\"buffer_ready\""), buffered)
        assertTrue(buffered.contains("\"buffered_through_ms\":90000"), buffered)

        val committed = json.encodeToString(
            ControlRequest.serializer(),
            request(
                acknowledgement = ActionAcknowledgement(
                    actionId,
                    AcknowledgementState.COMMITTED,
                    committedMediaOriginMs = 1_800_000,
                    firstFrameUnixMs = 1_788_000_000_000,
                ),
            ),
        )
        assertTrue(committed.contains("\"state\":\"committed\""), committed)
        // The evidence half of a commit, beside the measurement half. The
        // server requires both and compares this one against the offer.
        assertTrue(committed.contains("\"committed_media_origin_ms\":1800000"), committed)
        assertTrue(committed.contains("\"first_frame_unix_ms\":1788000000000"), committed)

        assertEquals(
            listOf("metadata_ready", "buffer_ready", "committed", "failed", "aborted"),
            AcknowledgementState.entries.map {
                json.encodeToString(AcknowledgementState.serializer(), it).trim('"')
            },
        )
    }

    @Test
    fun `an ordinary request carries no acknowledgement key at all`() {
        val encoded = json.encodeToString(ControlRequest.serializer(), request())
        assertFalse(encoded.contains("acknowledgement"), encoded)
    }

    @Test
    fun `a committed acknowledgement is settled before the ending exchange`() {
        // `demand: "end"` may not carry `state: "committed"`. The reporter
        // rewrites this one settlement to active; after it is accepted, the
        // unchanged player snapshot sends end as the following exchange.
        val ending = snapshot(demand = PlaybackDemand.END).copy(
            acknowledgement = ActionAcknowledgement(
                actionId,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = 1_800_000,
                firstFrameUnixMs = 1_788_000_000_000,
            ),
        )
        val settlement = settlingSnapshot(ending)
        assertEquals(PlaybackDemand.ACTIVE, settlement.demand)
        assertNotNull(settlement.sendableAcknowledgement)
        val encoded = json.encodeToString(
            ControlRequest.serializer(),
            request(
                demand = settlement.demand,
                acknowledgement = settlement.sendableAcknowledgement,
            ),
        )
        assertTrue(encoded.contains("\"state\":\"committed\""), encoded)
        assertTrue(encoded.contains("\"demand\":\"active\""), encoded)
        val terminal = assertNotNull(endingSnapshotAfterSettlement(ending))
        assertEquals(PlaybackDemand.END, terminal.demand)
        assertNull(terminal.acknowledgement)
        assertNull(endingSnapshotAfterSettlement(settlement), "only the original end intent queues it")

        // Every other state still rides an ending exchange: an abandoned
        // preparation must be settled, and the last exchange is often the only
        // one left to settle it on.
        val aborting = ending.copy(
            acknowledgement = ActionAcknowledgement(actionId, AcknowledgementState.ABORTED),
        )
        assertEquals(
            AcknowledgementState.ABORTED,
            assertNotNull(aborting.sendableAcknowledgement).state,
        )
        assertNull(endingSnapshotAfterSettlement(aborting))
    }

    @Test
    fun `a valid prepare is accepted and does not stop the reporter`() = runTest {
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(PlaybackControl.PROTOCOL, GENERATION, 7, 1, preparedAction()),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertFalse(subject.isStopped())
        assertNull(harness.exchanges.first().failure)
        subject.stop()
    }

    @Test
    fun `a prepare pointing anywhere but this node is fatal before any request`() = runTest {
        // Mirrors the server's own
        // `a_relayed_preparation_cannot_point_a_client_anywhere`: the client
        // refuses it too, and refuses it as a protocol error rather than
        // ignoring it, because a server sending one is not a server this client
        // still understands.
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    preparedAction(playlistUrl = "https://elsewhere.example/x/index.m3u8"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals("protocol:action", harness.exchanges.last().failure)
        assertEquals(1, harness.requests.size, "nothing was fetched from that URL")
    }

    @Test
    fun `a prepare missing the fields this client acts on is fatal`() = runTest {
        // An action inside the declared vocabulary but missing what the client
        // consumes is worse than one it has never heard of, because it would be
        // acted on. Nothing in the server tree asserts these four fields on a
        // serialized body, so this client validates all of them itself.
        val harness = Harness(this)
        harness.enqueue(
            Result.success(
                ControlResponse(
                    PlaybackControl.PROTOCOL,
                    GENERATION,
                    7,
                    1,
                    ControlAction(type = "prepare", actionId = "33333333-3333-4333-8333-333333333333"),
                ),
            ),
        )
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(30_001)
        assertTrue(subject.isStopped())
        assertEquals("protocol:action", harness.exchanges.last().failure)
    }

    @Test
    fun `the response's own effective selection is no longer dropped`() {
        // The server has always sent it; `ignoreUnknownKeys` meant this client
        // threw it away. Comparing it with a `prepare`'s own selection is how a
        // client knows what is about to change.
        val decoded = json.decodeFromString(
            ControlResponse.serializer(),
            """
            {"protocol":"${PlaybackControl.PROTOCOL}","generation":"$GENERATION",
             "control_epoch":7,"accepted_sequence":1,"action":{"type":"none"},
             "effective_selection":{"quality_auto":false,"height":720,
               "audio_track":1,"subtitle_burn":null,"audio_offset_ms":-40,
               "codec":"source","dynamic_range":"hdr10"}}
            """.trimIndent(),
        )
        val effective = assertNotNull(decoded.effectiveSelection)
        assertFalse(effective.qualityAuto)
        assertEquals(720L, effective.height)
        assertEquals(1L, effective.audioTrack)
        assertEquals(-40L, effective.audioOffsetMs)
        assertEquals("source", effective.codec)
        assertEquals("hdr10", effective.dynamicRange)
        assertTrue(effective.isValid)
    }
}

/**
 * The teardown hand-off: one last exchange, on a scope that outlives the
 * screen.
 *
 * Twice now the acknowledgement a preparation is owed has been lost on this
 * path — once because `end()`'s queued `stop()` cancelled the pump the urgent
 * notify had just launched, and once because cancelling a pump *inside* an
 * exchange ran the whole failure tail and armed a five-second retry of the
 * request it was replacing. Both were invisible: the code looked right and the
 * staging just quietly ran to its 330 s deadline. So the assertions here are
 * about what went out, not about what was intended.
 */
class PlaybackControlSettleTest {
    private val actionId = "33333333-3333-4333-8333-333333333333"

    /**
     * The teardown hands a capture in rather than letting the reporter read
     * one, because by then the caller has closed its observation. The identity
     * matches [Harness]'s so these read like every other exchange; the source
     * revision is high because a settled capture is by definition the last
     * word about this player.
     */
    private fun capture(value: PlaybackControlSnapshot) = PlaybackControlCapture(
        value,
        0L,
        PlaybackControlCaptureOwner("test-player", 1),
        Long.MAX_VALUE,
    )

    private fun settling(state: AcknowledgementState) = capture(
        snapshot().copy(acknowledgement = ActionAcknowledgement(actionId, state)),
    )

    @Test
    fun `a teardown across an in-flight exchange sends the snapshot it was handed`() = runTest {
        val harness = Harness(this)
        // The first exchange never comes back — the only case in which the
        // teardown path differs from an ordinary urgent report, and the case
        // both regressions lived in.
        harness.holds = true
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()
        assertEquals(1, harness.requests.size, "one exchange is in flight")
        assertNull(harness.requests.first().acknowledgement)

        harness.holds = false
        assertTrue(subject.settle(backgroundScope, settling(AcknowledgementState.ABORTED)))
        advanceTimeBy(2_000)
        runCurrent()

        assertEquals(2, harness.requests.size)
        val last = harness.requests.last()
        assertEquals(AcknowledgementState.ABORTED, last.acknowledgement?.state)
        assertEquals(actionId, last.acknowledgement?.actionId)
        // Not a replay of the request it inherited. A cancelled exchange used
        // to arm `retryRequest`, and `nextRequestLocked` prefers that over the
        // snapshot — so the exchange went out carrying no acknowledgement at
        // all, which is the one thing it existed to carry.
        assertEquals(2L, last.sequence)
        assertFalse(subject.status().retrying)
        subject.stop()
    }

    @Test
    fun `a teardown outlives the source that handed it over`() = runTest {
        // The third way this acknowledgement has been lost, and the one the
        // port onto main introduced: the session empties its capture slot in
        // the same synchronous pass that hands the reporter its last word —
        // `clearVerdict` on the disposal path, `end`'s invalidation on a
        // reopen. Every ordinary path reconciles what it is given against that
        // slot, and a reconciliation against an empty slot answers null, so the
        // exchange was built from nothing and never sent. A settled reporter
        // uses what it was handed.
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1)
        runCurrent()
        assertTrue(subject.settle(backgroundScope, settling(AcknowledgementState.ABORTED)))
        harness.sourceIsGone = true
        advanceTimeBy(2_000)
        runCurrent()

        val last = harness.requests.last()
        assertEquals(
            AcknowledgementState.ABORTED,
            last.acknowledgement?.state,
            "the exchange the teardown exists to send",
        )
        assertEquals(actionId, last.acknowledgement?.actionId)
        subject.stop()
    }

    @Test
    fun `a teardown with nothing in flight still sends it`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(1)
        runCurrent()
        assertTrue(subject.settle(backgroundScope, settling(AcknowledgementState.FAILED)))
        advanceTimeBy(2_000)
        runCurrent()
        assertEquals(
            AcknowledgementState.FAILED,
            harness.requests.last().acknowledgement?.state,
        )
        subject.stop()
    }

    @Test
    fun `an ending commit is accepted before its terminal exchange`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()

        val ending = snapshot(demand = PlaybackDemand.END).copy(
            acknowledgement = ActionAcknowledgement(
                actionId,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = 1_800_000,
                firstFrameUnixMs = 1_788_000_000_000,
            ),
        )
        val terminal = assertNotNull(endingSnapshotAfterSettlement(ending))
        assertTrue(subject.settle(backgroundScope, capture(ending), capture(terminal)))
        advanceTimeBy(5_000)
        runCurrent()

        val tail = harness.requests.takeLast(2)
        assertEquals(2, tail.size)
        assertEquals(PlaybackDemand.ACTIVE, tail[0].demand)
        assertEquals(AcknowledgementState.COMMITTED, tail[0].acknowledgement?.state)
        assertEquals(PlaybackDemand.END, tail[1].demand)
        assertNull(tail[1].acknowledgement)
        assertTrue(subject.isStopped(), "an accepted end closes the handed-off reporter")
    }

    @Test
    fun `a stopped reporter refuses the hand-off rather than pretending`() = runTest {
        // The caller waits on the return value: an exchange that will never be
        // built must not cost the teardown its whole deadline.
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()
        subject.stop()
        assertFalse(subject.settle(backgroundScope, settling(AcknowledgementState.ABORTED)))
    }

    @Test
    fun `a snapshot the server would refuse is not handed off`() = runTest {
        val harness = Harness(this)
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        runCurrent()
        val impossible = snapshot().copy(
            acknowledgement = ActionAcknowledgement(actionId, AcknowledgementState.COMMITTED),
        )
        assertFalse(impossible.isValid, "a commit with no rendered frame")
        assertFalse(subject.settle(backgroundScope, capture(impossible)))
        subject.stop()
    }

    @Test
    fun `a body this client cannot read stops the reporter rather than looping`() = runTest {
        // A protocol failure is not a transport failure. Folding it into the
        // null-status arm made it retryable, which replays the same request at
        // the cadence forever and logs `transport:none:-` — a diagnosable fatal
        // turned into an undiagnosable loop.
        val harness = Harness(this)
        harness.enqueue(Result.failure(ControlProtocolException("body")))
        val subject = assertNotNull(reporter(harness))
        subject.start(backgroundScope)
        advanceTimeBy(60_000)
        runCurrent()
        assertTrue(subject.isStopped())
        assertEquals(1, harness.requests.size)
        assertEquals("protocol:body", harness.exchanges.last().failure)
    }
}

class DisplayAwareAutoEvidenceTest {
    @Test fun a05EvidenceAcknowledgementRequiresTheExactSingleEvent() {
        val event = "12345678-1234-1234-1234-123456789abc"
        assertTrue(autoNegativeLinkAcknowledgement(event, listOf(event), 204, true, 250))
        assertFalse(autoNegativeLinkAcknowledgement(event, emptyList(), 204, true, 250))
        assertFalse(autoNegativeLinkAcknowledgement(event, listOf(event, event), 204, true, 250))
    }
    @Test fun a05NegativeAcknowledgementKeepsExactNonceAttachmentAndOriginalDeadline() {
        val nonce = "12345678-1234-1234-1234-123456789abc"
        assertTrue(autoNegativeLinkAcknowledgement(nonce, listOf(nonce), 204, true, 1))
        for (values in listOf(emptyList(), listOf(nonce, nonce), listOf("$nonce,$nonce"), listOf("other"), listOf(nonce.uppercase()))) {
            assertFalse(autoNegativeLinkAcknowledgement(nonce, values, 204, true, 1))
        }
        assertFalse(autoNegativeLinkAcknowledgement("malformed", listOf("malformed"), 204, true, 1))
        assertFalse(autoNegativeLinkAcknowledgement(nonce, listOf(nonce), 200, true, 1))
        assertFalse(autoNegativeLinkAcknowledgement(nonce, listOf(nonce), 204, false, 1))
        assertFalse(autoNegativeLinkAcknowledgement(nonce, listOf(nonce), 204, true, 0))
        val item = Any()
        val ticket = AutoRecoveryCauseTicket("incumbent", item, "a", "b", tv.plurx.app.data.ReopenReason.Link, 100, nonce)
        assertEquals(nonce, ticket.receiptForRequest("incumbent", "b", tv.plurx.app.data.ReopenReason.Link))
        assertNull(ticket.receiptForRequest("other", "b", tv.plurx.app.data.ReopenReason.Link))
        assertNull(ticket.receiptForRequest("incumbent", "c", tv.plurx.app.data.ReopenReason.Link))
        assertNull(ticket.receiptForRequest("incumbent", "b", tv.plurx.app.data.ReopenReason.Decode))
        assertFalse(ticket.isCurrent("incumbent", Any(), "a", "b", 200))
        assertFalse(ticket.isCurrent("incumbent", item, "a", "b", 15_101))
        assertFalse(ticket.isCurrent("incumbent", item, "a", "b", 99))
    }
    @Test fun a05TypedRecoveryKeepsCandidateAttachmentAndSuppliedDecodeIntervals() {
        val player = Any()
        val replacement = Any()
        val window = AutoDecodePressureWindow()
        fun sample(now: Long, position: Long, drops: Long, eligible: Boolean = true) =
            window.observe(player, "full-recipe-a", now, position, drops, eligible)
        assertFalse(sample(0, 0, 0))
        assertFalse(sample(2_000, 2_000, 3))
        assertTrue(sample(4_000, 4_000, 6))
        assertEquals(AutoDecodePressureEvidence(4_000, 4_000, 6), window.evidence)
        assertFalse(sample(6_000, 4_000, 10))
        assertFalse(sample(8_000, 6_000, 13, false))
        assertFalse(sample(10_000, 8_000, 16))
        assertFalse(window.observe(replacement, "full-recipe-a", 12_000, 10_000, 20, true))
        assertFalse(window.observe(replacement, "full-recipe-a", 11_000, 11_000, 23, true))
        val ticket = AutoRecoveryCauseTicket("incumbent", player, "full-recipe-a", "full-recipe-b",
            tv.plurx.app.data.ReopenReason.Decode, 100)
        assertTrue(ticket.isCurrent("incumbent", player, "full-recipe-a", "full-recipe-b", 15_100))
        assertFalse(ticket.isCurrent("incumbent", player, "full-recipe-a", "full-recipe-b", 15_101))
        assertFalse(ticket.isCurrent("incumbent", replacement, "full-recipe-a", "full-recipe-b", 200))
        assertFalse(ticket.isCurrent("incumbent", player, "other-recipe", "full-recipe-b", 200))
        assertFalse(ticket.isCurrent("incumbent", player, "full-recipe-a", "full-recipe-b", 99))
        val json = kotlinx.serialization.json.Json
        val causes = tv.plurx.app.data.ReopenReason.entries.map {
            json.encodeToString(tv.plurx.app.data.ReopenReason.serializer(), it)
        }
        assertEquals(listOf("\"stall\"", "\"link\"", "\"encode\"", "\"decode\"", "\"hold\"", "\"authority\""), causes)
    }
    @Test fun a05ViewerTransportRefusesStaleWrappers() {
        val forwarded = mutableListOf<String>()
        val delegate = java.lang.reflect.Proxy.newProxyInstance(
            androidx.media3.common.Player::class.java.classLoader,
            arrayOf(androidx.media3.common.Player::class.java),
        ) { _, method, _ ->
            forwarded.add(method.name)
            when (method.returnType) {
                java.lang.Boolean.TYPE -> false
                java.lang.Integer.TYPE -> 0
                java.lang.Long.TYPE -> 0L
                java.lang.Float.TYPE -> 0f
                else -> null
            }
        } as androidx.media3.common.Player
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        var current = true
        var optionalOwner: Any? = null
        val commands = mutableListOf<Boolean>()
        val wrapper = autoViewerTransport(delegate, { current }) { requested ->
            commands.add(requested)
            intent.setPlaybackRequested(requested)
            optionalOwner = if (requested) Any() else null
            // A second explicit Pause must still reach this writer without an
            // SDK edge, so it can revoke any optional boundary preparation.
        }
        wrapper.play()
        assertTrue(intent.playbackRequested)
        assertNotNull(optionalOwner)
        wrapper.pause()
        wrapper.pause()
        assertFalse(intent.playbackRequested)
        assertNull(optionalOwner)
        assertEquals(listOf(true, false, false), commands)
        wrapper.setPlayWhenReady(true)
        assertTrue(intent.playbackRequested)
        wrapper.seekTo(123L)
        assertEquals(listOf("seekTo"), forwarded, "non-transport SDK forwarding is unchanged")
        current = false
        wrapper.play()
        wrapper.pause()
        wrapper.setPlayWhenReady(false)
        assertEquals(listOf(true, false, false, true), commands)
        assertTrue(intent.playbackRequested, "stale wrapper cannot pause the successor")

        val manifest = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:1.000000,\nseg00000.m4s\n#EXTINF:1.000000,\nseg00001.m4s\n#EXT-X-ENDLIST\n"
        assertTrue(autoImmutableVodPlaylist(manifest.toByteArray()))
        assertFalse(autoImmutableVodPlaylist(manifest.replace("seg00001", "seg00000").toByteArray()))
        assertFalse(autoImmutableVodPlaylist(manifest.replace("#EXT-X-ENDLIST", "#EXT-X-DISCONTINUITY").toByteArray()))
    }

    @Test fun playAfterALongPauseReachesThePlayerImmediately() {
        // The delegate write is decided on the press. A long pause only arms
        // the original-first re-plan; it never defers or withholds the Play.
        val long = viewerTransportEdge(requested = true, wasRequested = false, pausedAtMs = 1_000L,
            pauseOwnerCurrent = true, nowMs = 1_000L + LONG_VIEWER_PAUSE_MS)
        assertTrue(long.delegatePlayWhenReady, "Play after a long pause is written at once")
        assertTrue(long.armsAutoBoundaryReplan)
        val far = viewerTransportEdge(true, false, 0L, true, 3_600_000L)
        assertTrue(far.delegatePlayWhenReady)
        assertTrue(far.armsAutoBoundaryReplan)

        val short = viewerTransportEdge(true, false, 1_000L, true, LONG_VIEWER_PAUSE_MS)
        assertTrue(short.delegatePlayWhenReady)
        assertFalse(short.armsAutoBoundaryReplan, "59.999 s is not a long pause")
        // A pause on another attachment, a clock that went backwards, or no
        // explicit pause at all still plays at once and owes nothing.
        for (edge in listOf(
            viewerTransportEdge(true, false, 1_000L, false, 120_000L),
            viewerTransportEdge(true, false, 120_000L, true, 1_000L),
            viewerTransportEdge(true, false, null, true, 120_000L),
            viewerTransportEdge(true, true, 1_000L, true, 120_000L),
        )) {
            assertTrue(edge.delegatePlayWhenReady)
            assertFalse(edge.armsAutoBoundaryReplan)
        }
        val pause = viewerTransportEdge(false, true, null, false, 120_000L)
        assertFalse(pause.delegatePlayWhenReady)
        assertFalse(pause.armsAutoBoundaryReplan)
    }

    @Test fun optionalOriginalReplanNeverBlocksAndIsServedOnceBesideThePlayingIncumbent() {
        val replan = AutoBoundaryReplan()
        val lifetime = Any()
        val original = "original"
        var asked = 0
        val produce = { asked += 1; original }
        assertNull(replan.take(lifetime, 60_000L, produce), "nothing armed, nothing owed")
        replan.arm(lifetime)
        // The incumbent is already playing; the re-plan waits for runway on
        // the ordinary Auto evaluation, not on the viewer.
        assertNull(replan.take(lifetime, AutoBoundaryReplan.AUTO_BOUNDARY_RUNWAY_MS - 1, produce))
        assertTrue(replan.isArmed, "short runway keeps the boundary owed")
        assertEquals(0, asked, "no candidate is asked for without runway")
        assertEquals(original, replan.take(lifetime, AutoBoundaryReplan.AUTO_BOUNDARY_RUNWAY_MS, produce))
        assertFalse(replan.isArmed)
        assertNull(replan.take(lifetime, 60_000L, produce), "served exactly once")
        assertEquals(1, asked)

        // A newer viewer edge renews the lifetime: the old boundary is stale
        // and is dropped, with no timer involved.
        replan.arm(lifetime)
        assertNull(replan.take(Any(), 60_000L, produce))
        assertFalse(replan.isArmed)
        replan.arm(lifetime)
        replan.clear()
        assertNull(replan.take(lifetime, 60_000L, produce))
    }

    @Test fun boundaryReplanIsSpentOnlyOnACandidate() {
        // Runway alone does not consume the boundary: with no fresh transfer
        // sample (or every original blocked) there is no candidate, and the
        // re-plan stays owed for the next evaluation.
        val replan = AutoBoundaryReplan()
        val lifetime = Any()
        replan.arm(lifetime)
        repeat(3) {
            assertNull(replan.take<String>(lifetime, 60_000L) { null })
            assertTrue(replan.isArmed, "no candidate, nothing spent")
        }
        assertEquals("original", replan.take(lifetime, 60_000L) { "original" })
        assertFalse(replan.isArmed)
        // What ends an unserved boundary is an event, not a clock.
        replan.arm(lifetime)
        assertNull(replan.take<String>(lifetime, 60_000L) { null })
        replan.clear()
        assertNull(replan.take(lifetime, 60_000L) { "original" })
    }

    @Test fun boundaryOfferThatBuildsNothingIsWithdrawn() {
        // Refuse, Same for a settled staging, a closed controller and an
        // action without a playlist all leave the boundary owning Auto with
        // no pipeline: that is the one case that must withdraw.
        assertTrue(autoBoundaryOfferBuiltNothing(boundaryStillOwns = true, built = false, switched = false))
        assertFalse(autoBoundaryOfferBuiltNothing(true, built = true, switched = false), "a primed successor settles itself")
        assertFalse(autoBoundaryOfferBuiltNothing(true, built = false, switched = true), "a switched successor is on screen")
        assertFalse(autoBoundaryOfferBuiltNothing(false, built = false, switched = false), "a newer owner already settled it")

        val source = controllerSource()
        val begin = source.substringAfter("private fun beginAutoBoundaryPreparation(")
            .substringBefore("/** Put Auto back where the boundary found it")
        val prepare = begin.indexOf("onPrepareAction(step.action)")
        val check = begin.indexOf("autoBoundaryOfferBuiltNothing(autoBoundaryAttempt === boundary")
        assertTrue(prepare in 0 until check, "the build is checked after the offer was handed over")
        assertTrue(begin.substring(check).contains("withdrawAutoBoundary(boundary)"))
        // A Start for the boundary's own candidate keeps the boundary across
        // the release of a superseded pipeline, so the check above and the
        // poll's ownership test still see it.
        val start = source.substringAfter("is PreparationOffer.Start -> {").substringBefore("private fun startSuccessor(")
        assertTrue(start.contains("if (boundary != null) autoBoundaryAttempt = boundary"))
    }

    @Test fun armedBoundaryReplanEndsWithStallRecoveryAndQualityChangeAndSkipsBlockedCandidates() {
        val source = controllerSource()
        val stall = source.substringAfter("private suspend fun onStall(").substringBefore("selectAutoStallRecoveryCandidate(observation,")
        val cleared = stall.indexOf("autoBoundaryReplan.clear()")
        assertTrue(cleared >= 0 && cleared < stall.indexOf("applyStallVerdict(verdict, event)"),
            "every stall recovery (hold, downgrade, reopen) ends the armed re-plan")
        val manual = source.substringAfter("fun prepareReplacement(").substringBefore("planReplacement.retain(onPrepared)")
        assertTrue(manual.contains("autoBoundaryReplan.clear()"), "a viewer quality change ends the armed re-plan")
        val seek = source.substringAfter("private fun enqueueSeek(").substringBefore("stallGuard.viewerSeek {")
        assertTrue(seek.contains("autoBoundaryReplan.arm(viewerTransportLifetime)"), "a seek still arms it")
        val candidate = source.substringAfter("private fun autoOriginalBoundaryCandidate(")
            .substringBefore("private fun autoBoundaryOwnerIsCurrent(")
        assertTrue(candidate.contains("(autoBlockedUntil[candidate.id] ?: 0L) <= now"),
            "a blocked original is not offered by a boundary either")
    }

    private fun controllerSource(): String = listOf(
        java.io.File("app/src/main/java/tv/plurx/app/player/Controller.kt"),
        java.io.File("src/main/java/tv/plurx/app/player/Controller.kt"),
        java.io.File("clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"),
    ).firstOrNull(java.io.File::isFile)?.readText() ?: error("Controller.kt source not found")

    @Test fun viewerResumeAndSeekNeverWaitBehindTheOptionalOriginalStage() {
        val source = listOf(
            java.io.File("app/src/main/java/tv/plurx/app/player/Controller.kt"),
            java.io.File("src/main/java/tv/plurx/app/player/Controller.kt"),
            java.io.File("clients/android/app/src/main/java/tv/plurx/app/player/Controller.kt"),
        ).firstOrNull(java.io.File::isFile)?.readText() ?: error("Controller.kt source not found")
        val resume = source.substringAfter("private fun setViewerPlaybackRequested(requested: Boolean) {")
            .substringBefore("private fun reopenAfterPausedRetirement(")
        assertTrue(resume.contains("writeViewerDelegate(edge.delegatePlayWhenReady)"))
        assertFalse(resume.contains("scope.launch"), "the viewer writer starts no optional work of its own")
        val seek = source.substringAfter("private fun enqueueSeek(")
            .substringBefore("private suspend fun publishIntent(")
        assertTrue(seek.contains("executeSeek(pending.targetMs, pending.sequence)"))
        assertFalse(seek.contains("Boundary(pending"), "the seek is not deferred behind an optional stage")
        assertFalse(source.contains("attemptAutoOriginalBoundary"))
        assertFalse(source.contains("autoBoundaryResumeJob"))
        // The boundary is served by the ordinary Auto evaluation and handed
        // off by the ordinary rendezvous, not by an exact-target hold.
        val tick = source.substringAfter("private fun tickDisplayAwareAuto() {")
            .substringBefore("private fun autoTransferOriginCurrent(")
        assertTrue(tick.contains("autoBoundaryReplan.take(viewerTransportLifetime"))
        assertFalse(source.contains("seekIssued"))
    }

    @Test fun a05StagedMediaIntervalsRequireCapturedPipelineAndOriginalDeadline() {
        val pipeline = Any()
        fun sample(index: Int) = AutoCompletedTransfer(100_000, 100, 1_000,
            "https://node", true, false, false, "https://node/hls/staged/seg0000$index.m4s",
            receipt = "00000000-0000-0000-0000-00000000000$index", etag = "object$index", statusCode = 200,
            pipelineIdentity = pipeline, observedMediaDurationMs = 1_000,
            mediaStartTimeMs = index * 1_000L, mediaEndTimeMs = (index + 1) * 1_000L, fullObject = true)
        val first = sample(0)
        val second = sample(1)
        fun margin(samples: List<AutoCompletedTransfer>, now: Long = 2_000, owner: Any = pipeline) =
            autoStagedEmpiricalMargin(samples, owner, "staged", now, 15_000)
        assertTrue(margin(listOf(first, second)))
        assertFalse(margin(listOf(first, first)))
        assertFalse(margin(listOf(first, second), owner = Any()))
        assertFalse(margin(listOf(first, second), now = 15_000))
        assertFalse(margin(listOf(first, second), now = 999))
        assertFalse(margin(listOf(first, second.copy(mediaStartTimeMs = 500, mediaEndTimeMs = 1_500))))
        assertFalse(margin(listOf(first, second.copy(mediaStartTimeMs = null))))
        assertFalse(margin(listOf(first, second.copy(observedMediaDurationMs = 1_003))))
        assertFalse(margin(listOf(first, second.copy(fullObject = false))))
        assertFalse(margin(listOf(first, second.copy(segmentId = "https://node/hls/staged/seg1.m4s"))))
        assertFalse(margin(listOf(first, second.copy(receipt = first.receipt))))
        assertFalse(margin(listOf(first, second.copy(etag = first.etag))))
    }

    private fun candidate(height: Int, route: String = "encode", peak: Long? = 3_000_000L) =
        tv.plurx.app.data.QualityCandidate("0a7ba9bab6fbdd31bab5e5e362a3fac7", List(32) { 0 },
            route, height * 16 / 9, height, height, average_bps = 8_000_000L, peak_bps = peak,
            grade = "sdr", decoder_compatible = true, complete_cache = false, sustainable = false)

    @Test fun runtimeDecoderSnapshotUsesStrictServerVocabulary() {
        val caps = tv.plurx.app.data.VideoEntry("hevc", listOf("main10"), 1_440, 2_560,
            tv.plurx.app.data.DecoderFrameRate(30, 1), listOf("pq", "sdr"), listOf(8))
        val snapshot = DecoderCapabilitySnapshot.fromVideo(1L, listOf(caps))
        val wire = Json.encodeToString(DecoderCapabilitySnapshot.serializer(), snapshot)
        val expected = """{"revision":1,"video":[{"codec":"hevc","profiles":["main10"],"available":true,"dynamic_ranges":["hdr10","sdr","dolby_vision"],"dv_profiles":[8],"max_width":2560,"max_height":1440,"max_frame_rate":{"numerator":30,"denominator":1}}]}"""
        assertEquals(Json.parseToJsonElement(expected), Json.parseToJsonElement(wire))
        val empty = DecoderCapabilitySnapshot.fromVideo(2L, listOf(tv.plurx.app.data.VideoEntry("h264", present = listOf("sdr"))))
        val emptyWire = Json.encodeToString(DecoderCapabilitySnapshot.serializer(), empty)
        assertTrue(emptyWire.contains("\"profiles\":[]"))
        assertTrue(emptyWire.contains("\"dv_profiles\":[]"))
        assertFalse(emptyWire.contains("\"present\":"))
        val many = (1..20).map { caps.copy(max_height = 100 + it) }
        assertEquals(16, DecoderCapabilitySnapshot.fromVideo(3L, many).video.size)
    }

    @Test fun currentDisplayOptimumDoesNotUpgradeAndBothAxesLimitEnlargement() {
        val current = candidate(1_080)
        assertEquals(current, autoPreferredDisplayCandidate(listOf(current, candidate(1_440)), 2_000.0, 1_125.0))
        assertEquals(1_440, autoPreferredDisplayCandidate(listOf(current, candidate(1_440)), 2_400.0, 1_350.0)?.height)
        assertFalse(autoDisplayFits(1_920, 1_000, 2_000.0, 1_125.0))
        assertFalse(autoDisplayFits(1_800, 1_080, 2_000.0, 1_125.0))
    }

    @Test fun coldOriginalCanRecoverToUnprovedCompatibleEncodeWithinLinkBudget() {
        val original = candidate(2_160, "remux", null)
        val lower = candidate(1_080)
        assertEquals(lower, autoRecoveryCandidate(listOf(original, lower), original, emptySet(), 4_000_000.0))
        assertNull(autoRecoveryCandidate(listOf(lower), original, emptySet(), 2_000_000.0))
        assertNull(autoRecoveryCandidate(listOf(lower), original, setOf(lower.id)))
        assertFalse(lower.sustainable)
        val hdrOriginal = original.copy(grade = "hdr10")
        assertNull(autoRecoveryCandidate(listOf(lower), hdrOriginal, emptySet()))
        assertEquals(lower, autoRecoveryCandidate(listOf(lower), hdrOriginal, emptySet(), decoderRecovery = true))
    }

    @Test fun originalAverageIsDownsideOnlyAndRequiresFreshUnpacedCurrentSessionEvidence() {
        val original = candidate(2_160, "remux", null)
        assertEquals(8_000_000.0, autoDownsideCostBps(original, sample(), "staged", 2_000L))
        assertNull(original.peak_bps)
        assertNull(autoDownsideCostBps(original, sample(), "other", 2_000L))
        assertNull(autoDownsideCostBps(original, sample(paced = true), "staged", 2_000L))
        assertNull(autoDownsideCostBps(original, sample(), "staged", 12_000L))
    }

    private fun sample(segment: String = "a", bodyMs: Long = 500L, mediaMs: Long? = 1_000L,
                       completedAt: Long = 1_000L, cached: Boolean? = false, paced: Boolean? = false) =
        AutoCompletedTransfer(100_000L, bodyMs, completedAt, "https://example.invalid", true,
            cached, paced, "/api/v1/hls/staged/seg$segment.m4s", mediaMs)

    @Test fun freshLinkRejectsStaleCachedPacedAndUnknownProvenance() {
        assertNotNull(autoCompletedTransferBps(sample(), 2_000L))
        assertNull(autoCompletedTransferBps(sample(completedAt = 0L), 10_001L))
        assertNull(autoCompletedTransferBps(sample(cached = true), 2_000L))
        assertNull(autoCompletedTransferBps(sample(paced = true), 2_000L))
        assertNull(autoCompletedTransferBps(sample(paced = null), 2_000L))
        assertNull(autoCompletedTransferBps(sample(cached = null), 2_000L))
    }

    @Test fun unknownPeakOriginalNeedsDistinctSegmentsAndEmpiricalMargin() {
        assertFalse(autoOriginalTransferMarginProven(listOf(sample(), sample()), "staged", 2_000L))
        assertTrue(autoOriginalTransferMarginProven(listOf(sample("a"), sample("b")), "staged", 2_000L))
        assertFalse(autoOriginalTransferMarginProven(listOf(sample("a", bodyMs = 600L), sample("b")), "staged", 2_000L))
        assertFalse(autoOriginalTransferMarginProven(listOf(sample("a"), sample("b", mediaMs = null)), "staged", 2_000L))
        assertFalse(autoOriginalTransferMarginProven(listOf(sample("a"), sample("b")), "another", 2_000L))
    }

    @Test fun a05PacingAndAttachmentUpgradeWindowsUseActualEvidence() {
        val status = tv.plurx.app.data.PlaybackSessionStatus(id = "incumbent", producer_state = "held",
            active_encode_candidate_id = "candidate", active_encode_milli_realtime = 2_000,
            active_encode_age_ms = 1_000L, active_encode_active_ms = 2_000L, active_encode_segments = 2)
        fun pressure(snapshot: tv.plurx.app.data.PlaybackSessionStatus?, session: String? = "incumbent", at: Long = 2_000L) =
            autoActiveProductionPressure(snapshot, 1_000L, at, session, "candidate", 3_000L)
        assertFalse(pressure(status), "paced wall delivery with actual 2x work is not pressure")
        assertTrue(pressure(status.copy(active_encode_milli_realtime = 800)), "fresh saturation still counts while currently held")
        assertFalse(pressure(status.copy(active_encode_milli_realtime = 800), at = 16_001L))
        assertFalse(pressure(status.copy(active_encode_milli_realtime = 800), session = "replacement"))
        assertFalse(pressure(null))
        assertFalse(pressure(status.copy(active_encode_milli_realtime = 800, active_encode_active_ms = null)))
        val item = Any(); val successor = Any()
        val window = AutoUpgradeEvidenceWindow()
        window.bind(item, 1, 0)
        window.stalled(1_000L)
        window.cliff(2_000L, 3_000L)
        window.cliff(2_000L, 10_000L)
        assertEquals(2_000L, window.lastCliffMs)
        assertFalse(window.allowsUpgrade(60_999L))
        assertFalse(window.allowsUpgrade(91_999L))
        assertTrue(window.allowsUpgrade(92_000L))
        assertFalse(window.allowsUpgrade(999L))
        window.bind(successor, 1, 92_000)
        assertNull(window.lastStallMs)
        assertNull(window.lastCliffMs)
        window.stalled(93_000L)
        window.bind(successor, 2, 93_000)
        assertNull(window.lastStallMs)
        window.cliff(1, 93_000L)
        assertNull(window.lastCliffMs)
        window.bind(null, 2, 93_000)
        assertFalse(window.allowsUpgrade(200_000L))
    }

    @Test fun a05FreshAttachmentRequiresObservedQuietIntervalBeforeHeadroomUpgrade() {
        val item = Any(); val replacement = Any()
        val window = AutoUpgradeEvidenceWindow()
        assertFalse(window.allowsUpgrade(100_000), "unobserved attachment is Unknown")
        window.bind(item, 1, 1_000)
        val headroomStartedAt = 1_000L
        assertTrue(46_000 - headroomStartedAt >= 45_000, "independent headroom is ready")
        assertFalse(window.allowsUpgrade(46_000), "45s headroom cannot bypass 60s observation")
        assertFalse(window.allowsProposal(46_000, headroomStartedAt))
        window.bind(item, 1, 50_000)
        assertFalse(window.allowsUpgrade(60_999))
        assertTrue(window.allowsUpgrade(61_000), "same binding does not renew observation")
        assertTrue(window.allowsProposal(61_000, headroomStartedAt), "concurrent windows first allow max(45s,60s), not 105s")
        assertFalse(window.allowsProposal(61_000, null))
        assertFalse(window.allowsProposal(61_000, 61_001))
        assertFalse(window.allowsUpgrade(999), "clock rollback is Unknown")
        window.stalled(62_000)
        assertFalse(window.allowsUpgrade(121_999))
        assertTrue(window.allowsUpgrade(122_000))
        window.reset()
        assertFalse(window.allowsUpgrade(200_000))
        window.bind(item, 2, 123_000)
        assertFalse(window.allowsUpgrade(182_999))
        assertTrue(window.allowsUpgrade(183_000))
        window.bind(replacement, 2, 184_000)
        assertFalse(window.allowsUpgrade(243_999))
        assertTrue(window.allowsUpgrade(244_000))
        window.cliff(244_000, 245_000)
        window.cliff(244_000, 250_000)
        assertFalse(window.allowsUpgrade(333_999))
        assertTrue(window.allowsUpgrade(334_000), "original EOF age is not renewed")
        window.bind(null, 2, 335_000)
        assertFalse(window.allowsUpgrade(500_000))
    }

    @Test fun stagedProductionProofRequiresExactCandidateAndCombinedAge() {
        val status = tv.plurx.app.data.PlaybackSessionStatus(id = "staged", active_encode_candidate_id = "candidate",
            active_encode_milli_realtime = 1_150, active_encode_age_ms = 1_000L,
            active_encode_active_ms = 2_000L, active_encode_segments = 2)
        assertTrue(autoStagedEncodeProof(status, 1_000L, 2_000L, "staged", "candidate"))
        assertFalse(autoStagedEncodeProof(status, 1_000L, 2_000L, "staged", "other"))
        assertFalse(autoStagedEncodeProof(status.copy(active_encode_age_ms = 14_500L), 1_000L, 2_000L, "staged", "candidate"))
        assertFalse(autoStagedEncodeProof(status.copy(active_encode_segments = 1), 1_000L, 2_000L, "staged", "candidate"))
    }

    @Test fun supersededAutoTrialCannotReplaceIncumbent() {
        assertTrue(autoTrialMayCommit("a", "a", 2L, 2L, 4L, 4L, true, true, false))
        assertFalse(autoTrialMayCommit("a", "a", 2L, 3L, 4L, 4L, true, true, false))
        assertFalse(autoTrialMayCommit("a", "a", 2L, 2L, 4L, 5L, true, true, false))
    }

    @Test fun manual1440RemainsSelectableBeforeProductionProof() {
        val candidate = tv.plurx.app.data.QualityCandidate("0a7ba9bab6fbdd31bab5e5e362a3fac7", List(32) { 0 },
            "encode", 2_560, 1_440, 1_440, grade = "sdr", decoder_compatible = true,
            complete_cache = false, sustainable = false)
        assertEquals(listOf(1_440), tv.plurx.app.data.manualCatalogHeights(listOf(candidate)))
    }
}

class DisplayAwareAutoIntentTest {
    @Test fun automaticCandidateRecoveryPreservesIntentLifetimeAndIndependentRevisions() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val id = "0a7ba9bab6fbdd31bab5e5e362a3fac7"
        intent.requestAutomaticCandidate(id, 1_440)
        val selection = ClientSelection(QualitySelection.AutoCandidate(1_440, id),
            audioTrack = 2, subtitle = SubtitleSelection(SubtitleMode.OFF), audioOffsetMs = 250,
            codec = CodecPolicy.AUTO, dynamicRange = DynamicRangePolicy.AUTO)
        val initial = intent.mediaIntent(selection)
        assertEquals(initial, intent.mediaIntent(selection))
        assertEquals(PlaybackQuality.Auto, intent.desiredQuality)
        intent.setPlaybackRequested(false)
        val paused = intent.mediaIntent(selection)
        assertEquals(initial.lifetime_id, paused.lifetime_id)
        assertEquals(initial.recipe_revision, paused.recipe_revision)
        assertEquals(initial.destination_revision, paused.destination_revision)
        assertEquals(initial.transport_revision + 1, paused.transport_revision)
        intent.noteViewerDestination()
        val sought = intent.mediaIntent(selection)
        assertEquals(paused.destination_revision + 1, sought.destination_revision)
        assertEquals(paused.recipe_revision, sought.recipe_revision)
        val changed = intent.mediaIntent(selection.copy(audioOffsetMs = 500))
        assertEquals(sought.recipe_revision + 1, changed.recipe_revision)
        assertEquals(sought.destination_revision, changed.destination_revision)
        assertEquals(sought.transport_revision, changed.transport_revision)
        val wire = Json.encodeToString(tv.plurx.app.data.MediaIntentEnvelope.serializer(), changed)
        assertTrue(wire.contains("\"mode\":\"auto\""))
        assertTrue(wire.contains("\"candidate_id\":\"$id\""))
    }
}
