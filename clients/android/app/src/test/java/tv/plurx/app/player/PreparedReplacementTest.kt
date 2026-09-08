package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.SerializationException
import kotlinx.serialization.json.Json
import okhttp3.Interceptor
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.assertFailsWith

private const val PREPARED_ACTION_ID = "33333333-3333-4333-8333-333333333333"
private const val PREPARED_SESSION_ID = "44444444-4444-4444-8444-444444444444"

private val preparedJson = Json { ignoreUnknownKeys = true; explicitNulls = false }

private fun effectiveSelection() = EffectiveSelection(
    qualityAuto = true,
    height = 1_080,
    audioTrack = 1,
    subtitleBurn = null,
    audioOffsetMs = 0,
    codec = "server_selected",
    dynamicRange = "sdr",
)

private fun requestedSelection() = ClientSelection(
    quality = QualitySelection.Auto,
    audioTrack = 1,
    subtitle = SubtitleSelection(SubtitleMode.OFF),
    audioOffsetMs = 0,
    codec = CodecPolicy.AUTO,
    dynamicRange = DynamicRangePolicy.AUTO,
)

private fun prepareAction(mediaOriginMs: Long = 600_000) = ControlAction(
    type = "prepare",
    actionId = PREPARED_ACTION_ID,
    sessionId = PREPARED_SESSION_ID,
    playlistUrl = "/api/v1/hls/$PREPARED_SESSION_ID/index.m3u8",
    mediaOriginMs = mediaOriginMs,
    effectiveSelection = effectiveSelection(),
)

private fun prepareJsonWithSelection(selection: String): String =
    """{"type":"prepare",
        "action_id":"$PREPARED_ACTION_ID",
        "session_id":"$PREPARED_SESSION_ID",
        "playlist_url":"/api/v1/hls/$PREPARED_SESSION_ID/index.m3u8",
        "media_origin_ms":600000,
        "effective_selection":{$selection}}"""

private val completeSelectionFields = linkedMapOf(
    "quality_auto" to "true",
    "height" to "1080",
    "audio_track" to "null",
    "subtitle_burn" to "null",
    "audio_offset_ms" to "0",
    "codec" to "\"server_selected\"",
    "dynamic_range" to "null",
)

private fun selectionJsonWithout(missing: String): String = completeSelectionFields
    .filterKeys { it != missing }
    .entries
    .joinToString(",") { (name, value) -> "\"$name\":$value" }

class PreparedActionDecodingTest {
    @Test
    fun `the exact prepare action decodes every field including nulls`() {
        val decoded = preparedJson.decodeFromString(
            ControlAction.serializer(),
            """
            {"type":"prepare",
             "action_id":"$PREPARED_ACTION_ID",
             "session_id":"$PREPARED_SESSION_ID",
             "playlist_url":"/api/v1/hls/$PREPARED_SESSION_ID/index.m3u8",
             "media_origin_ms":600000,
             "effective_selection":{"quality_auto":true,"height":1080,
               "audio_track":1,"subtitle_burn":null,"audio_offset_ms":0,
               "codec":"server_selected","dynamic_range":"sdr"},
             "future_response_field":"ignored"}
            """.trimIndent(),
        )

        assertEquals("prepare", decoded.type)
        assertEquals(PREPARED_ACTION_ID, decoded.actionId)
        assertEquals(PREPARED_SESSION_ID, decoded.sessionId)
        assertEquals(
            "/api/v1/hls/$PREPARED_SESSION_ID/index.m3u8",
            decoded.playlistUrl,
        )
        assertEquals(600_000L, decoded.mediaOriginMs)
        val selection = assertNotNull(decoded.effectiveSelection)
        assertTrue(selection.qualityAuto)
        assertEquals(1_080L, selection.height)
        assertEquals(1L, selection.audioTrack)
        assertNull(selection.subtitleBurn)
        assertEquals(0L, selection.audioOffsetMs)
        assertEquals("server_selected", selection.codec)
        assertEquals("sdr", selection.dynamicRange)
        assertTrue(decoded.preparedPayloadIsValid)
    }

    @Test
    fun `the prepared vocabulary is declared while the measured capability stays false`() {
        assertTrue(PlaybackControl.SUPPORTED_ACTIONS.contains("prepare_replacement"))
        assertFalse(PlaybackControl.SUPPORTED_ACTIONS.contains("prepare"))
        assertFalse(
            controlCapabilities(mapOf("vcodec" to "h264,hevc", "hdr" to "1"))
                .dualPlayerPreparation,
        )
    }

    private fun assertMissingSelectionFieldRejected(field: String) {
        assertFailsWith<SerializationException> {
            preparedJson.decodeFromString(
                ControlAction.serializer(),
                prepareJsonWithSelection(selectionJsonWithout(field)),
            )
        }
    }

    @Test
    fun `a missing nullable audio track is not accepted as explicit null`() {
        assertMissingSelectionFieldRejected("audio_track")
    }

    @Test
    fun `a missing nullable subtitle burn is not accepted as explicit null`() {
        assertMissingSelectionFieldRejected("subtitle_burn")
    }

    @Test
    fun `a missing nullable dynamic range is not accepted as explicit null`() {
        assertMissingSelectionFieldRejected("dynamic_range")
    }
}

class PreparedReplacementCoordinatorTest {
    @Test
    fun `the ladder is ordered and commit echoes the offered origin`() {
        val coordinator = PreparedReplacementCoordinator()
        coordinator.receive(prepareAction(), null, requestedSelection(), 1)

        assertTrue(coordinator.metadataReady(PREPARED_ACTION_ID))
        val metadata = assertNotNull(coordinator.nextAcknowledgement)
        assertEquals(AcknowledgementState.METADATA_READY, metadata.state)
        coordinator.receive(prepareAction(), metadata, requestedSelection(), 2)

        assertTrue(coordinator.bufferReady(PREPARED_ACTION_ID, 612_000))
        val buffered = assertNotNull(coordinator.nextAcknowledgement)
        assertEquals(AcknowledgementState.BUFFER_READY, buffered.state)
        assertEquals(612_000L, buffered.bufferedThroughMs)
        coordinator.receive(prepareAction(), buffered, requestedSelection(), 3)

        assertTrue(
            coordinator.committed(
                PREPARED_ACTION_ID,
                1_788_000_000_000,
                requestedSelection(),
                PlaybackDemand.ACTIVE,
            ).queued,
        )
        val committed = assertNotNull(coordinator.nextAcknowledgement)
        assertEquals(AcknowledgementState.COMMITTED, committed.state)
        assertEquals(600_000L, committed.committedMediaOriginMs)
        assertEquals(1_788_000_000_000L, committed.firstFrameUnixMs)
    }

    @Test
    fun `a wrong-origin commit is detected by the offer persisting`() {
        val coordinator = PreparedReplacementCoordinator()
        coordinator.receive(prepareAction(), null, requestedSelection(), 1)
        coordinator.metadataReady(PREPARED_ACTION_ID)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 2,
        )
        coordinator.bufferReady(PREPARED_ACTION_ID, 612_000)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 3,
        )
        coordinator.committed(
            PREPARED_ACTION_ID,
            1_788_000_000_000,
            requestedSelection(),
            PlaybackDemand.ACTIVE,
        )

        val wrongOrigin = assertNotNull(coordinator.nextAcknowledgement).copy(
            committedMediaOriginMs = 599_999,
        )
        val events = coordinator.receive(
            prepareAction(), wrongOrigin, requestedSelection(), 4,
        )

        val released = events.single() as PreparedReplacementEvent.Released
        assertEquals(PreparedReplacementReleaseReason.COMMIT_DISCARDED, released.reason)
        assertEquals(
            AcknowledgementState.ABORTED,
            assertNotNull(coordinator.nextAcknowledgement).state,
            "the client answers the persisted offer instead of waiting for an error",
        )
        val aborted = assertNotNull(coordinator.nextAcknowledgement)
        assertTrue(
            coordinator.receive(
                ControlAction("none"), aborted, requestedSelection(), 5,
            ).isEmpty(),
        )
        assertTrue(
            coordinator.reset(PreparedReplacementReleaseReason.SESSION_ENDED).isEmpty(),
            "settling the abort and ending the session must not release the offer twice",
        )
    }

    @Test
    fun `failed and aborted preparations each release exactly once`() {
        for ((state, release) in listOf(
            AcknowledgementState.FAILED to PreparedReplacementReleaseReason.FAILED,
            AcknowledgementState.ABORTED to PreparedReplacementReleaseReason.ABORTED,
        )) {
            val coordinator = PreparedReplacementCoordinator()
            coordinator.receive(prepareAction(), null, requestedSelection(), 1)
            val first = if (state == AcknowledgementState.FAILED) {
                coordinator.failed(PREPARED_ACTION_ID)
            } else {
                coordinator.aborted(PREPARED_ACTION_ID)
            }
            assertEquals(release, assertNotNull(first).reason)
            val acknowledgement = assertNotNull(coordinator.nextAcknowledgement)
            assertTrue(
                coordinator.receive(
                    ControlAction("none"), acknowledgement, requestedSelection(), 2,
                ).isEmpty(),
            )
            assertTrue(
                coordinator.reset(PreparedReplacementReleaseReason.SESSION_ENDED).isEmpty(),
            )
        }
    }

    @Test
    fun `a selection change aborts instead of queuing an old commit`() {
        val coordinator = PreparedReplacementCoordinator()
        coordinator.receive(prepareAction(), null, requestedSelection(), 1)
        coordinator.metadataReady(PREPARED_ACTION_ID)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 2,
        )
        coordinator.bufferReady(PREPARED_ACTION_ID, 612_000)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 3,
        )

        val result = coordinator.committed(
            PREPARED_ACTION_ID,
            1_788_000_000_000,
            requestedSelection().copy(audioOffsetMs = 250),
            PlaybackDemand.ACTIVE,
        )

        assertFalse(result.queued)
        assertEquals(
            PreparedReplacementReleaseReason.ABORTED,
            assertNotNull(result.released).reason,
        )
        assertEquals(
            AcknowledgementState.ABORTED,
            assertNotNull(coordinator.nextAcknowledgement).state,
        )
    }

    @Test
    fun `terminal demand aborts instead of silently dropping a commit`() {
        val coordinator = PreparedReplacementCoordinator()
        coordinator.receive(prepareAction(), null, requestedSelection(), 1)
        coordinator.metadataReady(PREPARED_ACTION_ID)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 2,
        )
        coordinator.bufferReady(PREPARED_ACTION_ID, 612_000)
        coordinator.receive(
            prepareAction(), coordinator.nextAcknowledgement, requestedSelection(), 3,
        )

        val result = coordinator.committed(
            PREPARED_ACTION_ID,
            1_788_000_000_000,
            requestedSelection(),
            PlaybackDemand.END,
        )

        assertFalse(result.queued)
        assertEquals(
            PreparedReplacementReleaseReason.SESSION_ENDED,
            assertNotNull(result.released).reason,
        )
        assertEquals(
            AcknowledgementState.ABORTED,
            assertNotNull(coordinator.nextAcknowledgement).state,
        )
    }

    @Test
    fun `repeat identity does not produce a second offer`() {
        val coordinator = PreparedReplacementCoordinator()
        assertTrue(
            coordinator.receive(prepareAction(), null, requestedSelection(), 1).single()
                is PreparedReplacementEvent.Offered,
        )
        repeat(3) {
            assertTrue(
                coordinator.receive(
                    prepareAction(), null, requestedSelection(), 2L + it,
                ).isEmpty(),
            )
        }
    }

    @Test
    fun `expiry and withdrawal release the local preparation`() {
        val expired = PreparedReplacementCoordinator()
        expired.receive(prepareAction(), null, requestedSelection(), 10)
        val expiry = expired.expire(10 + PREPARED_OFFER_TTL_MS).single()
            as PreparedReplacementEvent.Released
        assertEquals(PreparedReplacementReleaseReason.EXPIRED, expiry.reason)
        assertNull(expired.activeOffer)

        val withdrawn = PreparedReplacementCoordinator()
        withdrawn.receive(prepareAction(), null, requestedSelection(), 20)
        val withdrawal = withdrawn.receive(
            ControlAction("none"), null, requestedSelection(), 21,
        ).single()
            as PreparedReplacementEvent.Released
        assertEquals(PreparedReplacementReleaseReason.WITHDRAWN, withdrawal.reason)
        assertNull(withdrawn.activeOffer)

        val ended = PreparedReplacementCoordinator()
        ended.receive(prepareAction(), null, requestedSelection(), 30)
        assertEquals(
            PreparedReplacementReleaseReason.SESSION_ENDED,
            (ended.reset(PreparedReplacementReleaseReason.SESSION_ENDED).single()
                as PreparedReplacementEvent.Released).reason,
        )
        assertTrue(
            ended.reset(PreparedReplacementReleaseReason.SESSION_ENDED).isEmpty(),
            "a concurrent second teardown must not release the offer twice",
        )
    }
}

private data class PreparedWireRequest(val request: ControlRequest, val body: String)

class PreparedReplacementTransportTest {
    private fun observation() = PlayerControlObservation(
        positionMs = 600_000,
        durationMs = 7_200_000,
        bufferedFromMs = 600_000,
        bufferedThroughMs = 620_000,
        rate = 1.0,
        isPaused = false,
        isEnded = false,
        isSeeking = false,
        hasStarted = true,
        isLikelyToKeepUp = true,
        observedDownloadBps = 40_000_000,
        selection = requestedSelection(),
        capabilities = DynamicCapabilities(
            platform = "android",
            maxHeight = 2_160,
            codecs = listOf(CodecPolicy.H264, CodecPolicy.HEVC),
            dynamicRanges = listOf(DynamicRangePolicy.SDR),
            dualPlayerPreparation = false,
        ),
    )

    private fun bootstrap() = ControlBootstrap(
        protocol = PlaybackControl.PROTOCOL,
        url = "/api/v1/hls/session-1/control",
        generation = "11111111-1111-4111-8111-111111111111",
        controlEpoch = 7,
        nextExchangeMs = PlaybackControl.MIN_EXCHANGE_MS,
        leaseTimeoutMs = 300_000,
    )

    private fun transport(
        requests: LinkedBlockingQueue<PreparedWireRequest>,
        responseAction: (ControlRequest) -> ControlAction,
    ): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor(
            Interceptor { chain ->
                val buffer = Buffer()
                chain.request().body!!.writeTo(buffer)
                val body = buffer.readUtf8()
                val request = preparedJson.decodeFromString(ControlRequest.serializer(), body)
                requests.add(PreparedWireRequest(request, body))
                val action = responseAction(request)
                val responseBody = preparedJson.encodeToString(
                    ControlResponse.serializer(),
                    ControlResponse(
                        protocol = PlaybackControl.PROTOCOL,
                        generation = request.generation,
                        controlEpoch = request.controlEpoch,
                        acceptedSequence = request.sequence,
                        action = action,
                    ),
                )
                Response.Builder()
                    .request(chain.request())
                    .protocol(Protocol.HTTP_1_1)
                    .code(200)
                    .message("OK")
                    .body(responseBody.toResponseBody("application/json".toMediaType()))
                    .build()
            },
        ).build()
        return PlaybackControlTransport("https://cinema.example", client, preparedJson)
    }

    private fun acknowledgementRejectingTransport(
        requests: LinkedBlockingQueue<PreparedWireRequest>,
        commitArrived: CountDownLatch? = null,
        releaseCommit: CountDownLatch? = null,
    ): PlaybackControlTransport {
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            val buffer = Buffer()
            chain.request().body!!.writeTo(buffer)
            val body = buffer.readUtf8()
            val request = preparedJson.decodeFromString(ControlRequest.serializer(), body)
            requests.add(PreparedWireRequest(request, body))
            val committed = request.acknowledgement?.state == AcknowledgementState.COMMITTED
            if (committed) {
                commitArrived?.countDown()
                if (releaseCommit != null && !releaseCommit.await(5, TimeUnit.SECONDS)) {
                    error("the delayed commit response was never released")
                }
            }
            val responseBody = if (committed) {
                """{"code":"stale_control",
                    "message":"the preparation acknowledgement lost its commit or deadline fence",
                    "generation":"${request.generation}","control_epoch":7}"""
            } else {
                preparedJson.encodeToString(
                    ControlResponse.serializer(),
                    ControlResponse(
                        protocol = PlaybackControl.PROTOCOL,
                        generation = request.generation,
                        controlEpoch = request.controlEpoch,
                        acceptedSequence = request.sequence,
                        action = if (request.sequence <= 3) prepareAction() else ControlAction("none"),
                    ),
                )
            }
            Response.Builder()
                .request(chain.request())
                .protocol(Protocol.HTTP_1_1)
                .code(if (committed) 409 else 200)
                .message(if (committed) "Conflict" else "OK")
                .body(responseBody.toResponseBody("application/json".toMediaType()))
                .build()
        }.build()
        return PlaybackControlTransport("https://cinema.example", client, preparedJson)
    }

    private fun awaitRequest(
        requests: LinkedBlockingQueue<PreparedWireRequest>,
        state: AcknowledgementState? = null,
    ): PreparedWireRequest {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (System.nanoTime() < deadline) {
            val value = requests.poll(100, TimeUnit.MILLISECONDS) ?: continue
            if (value.request.acknowledgement?.state == state) return value
        }
        error("no request carrying $state")
    }

    private fun awaitSettled(session: PlaybackControlSession) {
        val deadline = System.nanoTime() + TimeUnit.SECONDS.toNanos(5)
        while (System.nanoTime() < deadline) {
            if (session.pendingPreparedAcknowledgementForTest() == null) return
            Thread.sleep(10)
        }
        error("the acknowledgement never settled")
    }

    @Test
    fun `the fake transport receives metadata buffer and committed bodies in order`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                ::observation,
                transport(requests) { request ->
                    if (request.acknowledgement?.state == AcknowledgementState.COMMITTED) {
                        ControlAction("none")
                    } else {
                        prepareAction()
                    }
                },
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Offered
                ?: error("the offer was not dispatched")
            val initial = awaitRequest(requests)
            assertTrue(initial.body.contains("\"prepare_replacement\""))
            assertTrue(initial.body.contains("\"dual_player_preparation\":false"))

            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            val metadata = awaitRequest(requests, AcknowledgementState.METADATA_READY)
            assertTrue(metadata.body.contains("\"state\":\"metadata_ready\""))
            awaitSettled(session)

            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            val buffered = awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            assertTrue(buffered.body.contains("\"buffered_through_ms\":612000"))
            awaitSettled(session)

            assertTrue(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            val committed = awaitRequest(requests, AcknowledgementState.COMMITTED)
            assertTrue(committed.body.contains("\"committed_media_origin_ms\":600000"))
            assertTrue(committed.body.contains("\"first_frame_unix_ms\":1788000000000"))
            val released = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("the accepted commit did not settle")
            assertEquals(PreparedReplacementReleaseReason.COMMITTED, released.reason)
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `a returned offer after commit is noticed and explicitly aborted`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                ::observation,
                transport(requests) { request ->
                    if (request.acknowledgement?.state == AcknowledgementState.ABORTED) {
                        ControlAction("none")
                    } else {
                        // The fake server models the silent-discard contract:
                        // a bad origin is a 200 carrying the same offer.
                        prepareAction()
                    }
                },
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as PreparedReplacementEvent.Offered
            awaitRequest(requests)
            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            awaitRequest(requests, AcknowledgementState.METADATA_READY)
            awaitSettled(session)
            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            awaitSettled(session)
            assertTrue(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            awaitRequest(requests, AcknowledgementState.COMMITTED)

            val discarded = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("the persisted offer was not noticed")
            assertEquals(PreparedReplacementReleaseReason.COMMIT_DISCARDED, discarded.reason)
            val aborted = awaitRequest(requests, AcknowledgementState.ABORTED)
            assertEquals(PREPARED_ACTION_ID, aborted.request.acknowledgement?.actionId)
            assertTrue(session.isReporting, "the control loop continues after the silent discard")
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `a rejected preparation acknowledgement releases it and keeps reporting`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                ::observation,
                acknowledgementRejectingTransport(requests),
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as PreparedReplacementEvent.Offered
            awaitRequest(requests)
            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            awaitRequest(requests, AcknowledgementState.METADATA_READY)
            awaitSettled(session)
            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            awaitSettled(session)
            assertTrue(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            val commit = awaitRequest(requests, AcknowledgementState.COMMITTED)

            val released = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("the rejected acknowledgement did not release the preparation")
            assertEquals(
                PreparedReplacementReleaseReason.ACKNOWLEDGEMENT_REJECTED,
                released.reason,
            )
            val next = awaitRequest(requests)
            assertTrue(next.request.sequence > commit.request.sequence)
            assertNull(next.request.acknowledgement)
            assertTrue(session.isReporting)
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `a changed selection aborts and never sends the stale commit`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val observed = AtomicReference(observation())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                observed::get,
                transport(requests) { request ->
                    if (request.acknowledgement?.state == AcknowledgementState.ABORTED) {
                        ControlAction("none")
                    } else {
                        prepareAction()
                    }
                },
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as PreparedReplacementEvent.Offered
            awaitRequest(requests)
            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            awaitRequest(requests, AcknowledgementState.METADATA_READY)
            awaitSettled(session)
            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            awaitSettled(session)

            observed.set(
                observation().copy(
                    selection = requestedSelection().copy(audioOffsetMs = 250),
                ),
            )
            session.playerChanged()

            val released = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("the changed selection did not release its old preparation")
            assertEquals(PreparedReplacementReleaseReason.ABORTED, released.reason)
            assertFalse(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            val aborted = awaitRequest(requests, AcknowledgementState.ABORTED)
            assertEquals(
                requestedSelection().copy(audioOffsetMs = 250),
                aborted.request.selection,
            )
            assertTrue(session.isReporting)
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `terminal demand aborts the preparation and never carries committed`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val observed = AtomicReference(observation())
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                observed::get,
                transport(requests) { request ->
                    if (request.acknowledgement?.state == AcknowledgementState.ABORTED) {
                        ControlAction("none")
                    } else {
                        prepareAction()
                    }
                },
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as PreparedReplacementEvent.Offered
            awaitRequest(requests)
            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            awaitRequest(requests, AcknowledgementState.METADATA_READY)
            awaitSettled(session)
            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            awaitSettled(session)

            observed.set(observation().copy(positionMs = 7_200_000, isEnded = true))
            session.playerChanged()

            val released = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("terminal demand did not release the preparation")
            assertEquals(PreparedReplacementReleaseReason.SESSION_ENDED, released.reason)
            assertFalse(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            val aborted = awaitRequest(requests, AcknowledgementState.ABORTED)
            assertEquals(PlaybackDemand.END, aborted.request.demand)
            assertFalse(aborted.body.contains("\"state\":\"committed\""))
            session.end()
            assertNull(events.poll(200, TimeUnit.MILLISECONDS), "teardown released it twice")
        } finally {
            session.end()
            scope.cancel()
        }
    }

    @Test
    fun `commit rejection settles while the current capture is cleared`() = runBlocking {
        val requests = LinkedBlockingQueue<PreparedWireRequest>()
        val events = LinkedBlockingQueue<PreparedReplacementEvent>()
        val commitArrived = CountDownLatch(1)
        val releaseCommit = CountDownLatch(1)
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        try {
            session.begin(
                bootstrap(),
                ::observation,
                acknowledgementRejectingTransport(requests, commitArrived, releaseCommit),
                onPreparedEvent = events::add,
            )
            val offered = events.poll(5, TimeUnit.SECONDS) as PreparedReplacementEvent.Offered
            awaitRequest(requests)
            assertTrue(session.preparedMetadataReady(offered.offer.actionId))
            awaitRequest(requests, AcknowledgementState.METADATA_READY)
            awaitSettled(session)
            assertTrue(session.preparedBufferReady(offered.offer.actionId, 612_000))
            awaitRequest(requests, AcknowledgementState.BUFFER_READY)
            awaitSettled(session)
            assertTrue(session.preparedCommitted(offered.offer.actionId, 1_788_000_000_000))
            assertTrue(commitArrived.await(5, TimeUnit.SECONDS))
            val commit = awaitRequest(requests, AcknowledgementState.COMMITTED)

            session.clearVerdict()
            releaseCommit.countDown()

            val released = events.poll(5, TimeUnit.SECONDS) as? PreparedReplacementEvent.Released
                ?: error("the capture gap discarded the commit rejection")
            assertEquals(
                PreparedReplacementReleaseReason.ACKNOWLEDGEMENT_REJECTED,
                released.reason,
            )
            session.playerChanged()
            val next = awaitRequest(requests)
            assertTrue(next.request.sequence > commit.request.sequence)
            assertNull(next.request.acknowledgement)
            assertTrue(session.isReporting)
        } finally {
            releaseCommit.countDown()
            session.end()
            scope.cancel()
        }
    }
}
