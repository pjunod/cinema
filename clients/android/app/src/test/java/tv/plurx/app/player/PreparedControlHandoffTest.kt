package tv.plurx.app.player

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer
import java.util.concurrent.CopyOnWriteArrayList
import java.util.concurrent.CountDownLatch
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicReference
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class PreparedControlHandoffTest {
    private val predecessor = "11111111-1111-4111-8111-111111111111"
    private val successor = "22222222-2222-4222-8222-222222222222"
    private val actionId = "33333333-3333-4333-8333-333333333333"
    private val bootstrap = ControlBootstrap(PlaybackControl.PROTOCOL,
        "/api/v1/hls/$successor/control", successor, 1L, 1000L, 30000L)
    private val standing = ClientSelection(quality = QualitySelection.AutoCandidate(2160, "copy-original"),
        codec = CodecPolicy.AUTO, dynamicRange = DynamicRangePolicy.AUTO, audioTrack = 0,
        audioOffsetMs = 0, subtitle = SubtitleSelection(SubtitleMode.OFF))
    private fun handoff() = PreparedControlHandoff(ControlAction(type = "prepare", actionId = actionId,
        sessionId = successor, playlistUrl = "/api/v1/hls/$successor/index.m3u8", mediaOriginMs = 0,
        control = bootstrap, effectiveSelection = EffectiveSelection(candidateId = "processed", qualityAuto = true,
            height = 2160, audioOffsetMs = 0, codec = "server_selected")), standing, 1L)
    private fun committed() = ActionAcknowledgement(actionId, AcknowledgementState.COMMITTED,
        committedMediaOriginMs = 0, firstFrameUnixMs = 1234)

    @Test fun serverPrepareCarriesItsOwnControlBootstrap() {
        val action = Json { ignoreUnknownKeys = true }.decodeFromString(ControlAction.serializer(), """
            {"type":"prepare","action_id":"$actionId","session_id":"$successor",
             "control":{"protocol":"${PlaybackControl.PROTOCOL}","url":"/api/v1/hls/$successor/control",
               "generation":"$successor","control_epoch":1,"next_exchange_ms":1000,"lease_timeout_ms":30000}}
            """)
        assertEquals(bootstrap, action.control)
        assertEquals(bootstrap, handoff().copy(action = action).bootstrap)
    }

    @Test fun onlyTheAcceptedExactCommitTransfersReporting() {
        val handoff = handoff()
        assertTrue(handoff.accepted(committed(), "none", "none", successor))
        assertTrue(handoff.accepted(committed(), "none", "hold", successor))
        assertFalse(handoff.accepted(committed(), "none", "terminal", successor))
        assertFalse(handoff.accepted(committed(), "none", "retry_resource", successor))
        assertFalse(handoff.accepted(committed(), "offered", "prepare", successor))
        assertFalse(handoff.accepted(committed(), null, "none", successor))
        assertFalse(handoff.accepted(committed().copy(actionId = successor), "none", "none", successor))
        assertFalse(handoff.accepted(committed().copy(committedMediaOriginMs = 1000), "none", "none", successor))
        assertFalse(handoff.accepted(committed().copy(firstFrameUnixMs = null), "none", "none", successor))
        assertFalse(handoff.accepted(committed(), "none", "none", predecessor))
        for (state in listOf(AcknowledgementState.FAILED, AcknowledgementState.ABORTED,
            AcknowledgementState.BUFFER_READY)) {
            assertFalse(handoff.accepted(committed().copy(state = state), "none", "none", successor))
        }
    }

    @Test fun anAcceptedCommitCannotRedirectReportingToAnotherSession() {
        val handoff = handoff()
        val legacy = handoff.copy(action = handoff.action.copy(control = null))
        assertNull(legacy.bootstrap)
        assertTrue(legacy.receiptAccepted(committed(), "none", "none", successor))
        assertFalse(legacy.accepted(committed(), "none", "none", successor))
        assertNull(handoff.copy(action = handoff.action.copy(control = bootstrap.copy(
            url = "/api/v1/hls/$predecessor/control"))).bootstrap)
        assertNull(handoff.copy(action = handoff.action.copy(control = bootstrap.copy(controlEpoch = 0))).bootstrap)
        assertNull(handoff.copy(action = handoff.action.copy(control = bootstrap.copy(
            url = "https://untrusted.invalid/api/v1/hls/$successor/control"))).bootstrap)
    }

    @Test fun localPictureAndRollbackLeaveCandidateUntouchedUntilCommitAcceptance() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        intent.requestAutomaticCandidate("copy-original", 2160)
        val pending = handoff().copy(candidateOwner = intent.automaticCandidateOwner())
        assertEquals("copy-original", intent.automaticCandidateId)
        assertEquals(standing, pending.standingSelection)
        assertFalse(pending.accepted(committed(), "offered", "prepare", successor))
        assertEquals("copy-original", intent.automaticCandidateId)
        if (pending.accepted(committed(), "none", "none", successor)) {
            val effective = pending.action.effectiveSelection!!
            assertTrue(intent.adoptCommittedAutomaticCandidate(effective.candidateId, effective.height,
                effective.qualityAuto, pending.candidateOwner))
        }
        assertEquals("processed", intent.automaticCandidateId)
        assertEquals("copy-original", (pending.standingSelection.quality as QualitySelection.AutoCandidate).candidateId)
    }

    @Test fun acceptedWireCommitTransfersTheRealSessionCallbackAfterItsResponse() = runBlocking {
        wireHandoff("none", true)
    }

    @Test fun terminalWireResponseDoesNotDispatchSuccessorReporting() = runBlocking {
        wireHandoff("terminal", false)
    }

    private fun wireHandoff(responseAction: String, transfer: Boolean) {
        val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val session = PlaybackControlSession(scope)
        val json = Json { ignoreUnknownKeys = true; explicitNulls = false }
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        intent.requestAutomaticCandidate("copy-original", 2160)
        val pending = handoff().copy(candidateOwner = intent.automaticCandidateOwner())
        val oldBootstrap = bootstrap.copy(url = "/api/v1/hls/$predecessor/control", generation = predecessor)
        val initial = PlayerControlObservation(positionMs = 1_860_000, durationMs = 7_200_000,
            bufferedFromMs = 1_860_000, bufferedThroughMs = 1_890_000, rate = 1.0,
            isPaused = false, isEnded = false, isSeeking = false, hasStarted = true,
            isLikelyToKeepUp = true, selection = pending.standingSelection,
            capabilities = DynamicCapabilities(platform = "android", maxHeight = 2160,
                codecs = listOf(CodecPolicy.H264), dynamicRanges = listOf(DynamicRangePolicy.SDR),
                dualPlayerPreparation = true))
        val observed = AtomicReference(initial)
        val requests = CopyOnWriteArrayList<Pair<String, ControlRequest>>()
        val firstRequest = LinkedBlockingQueue<ControlRequest>()
        val successorRequest = LinkedBlockingQueue<ControlRequest>()
        val commitReceived = CountDownLatch(1)
        val allowCommitAnswer = CountDownLatch(1)
        val callbackFinished = CountDownLatch(1)
        val client = OkHttpClient.Builder().addInterceptor { chain ->
            val buffer = Buffer()
            chain.request().body!!.writeTo(buffer)
            val request = json.decodeFromString(ControlRequest.serializer(), buffer.readUtf8())
            val path = chain.request().url.encodedPath
            requests.add(path to request)
            val isCommit = request.acknowledgement?.state == AcknowledgementState.COMMITTED
            if (path == bootstrap.url) successorRequest.add(request)
            else if (!isCommit) firstRequest.add(request)
            if (isCommit) {
                commitReceived.countDown()
                check(allowCommitAnswer.await(5, TimeUnit.SECONDS))
            }
            val type = if (isCommit) responseAction else "none"
            val extra = if (type == "terminal") ",\"code\":\"source_unavailable\",\"message\":\"Source unavailable\"" else ""
            val body = """{"protocol":"${PlaybackControl.PROTOCOL}","generation":"${request.generation}",
                "control_epoch":${request.controlEpoch},"accepted_sequence":${request.sequence},
                "action":{"type":"$type"$extra},"delivery":{"preparation":"none"}}"""
            Response.Builder().request(chain.request()).protocol(Protocol.HTTP_1_1).code(200).message("OK")
                .body(body.toResponseBody("application/json".toMediaType())).build()
        }.build()
        val transport = PlaybackControlTransport("https://cinema.example", client, json)
        try {
            session.begin(oldBootstrap, observed::get, transport = transport,
                onAcknowledged = { acknowledgement, response ->
                    if (pending.accepted(acknowledgement, response.delivery?.preparation,
                            response.action.type, successor)) {
                        val effective = pending.action.effectiveSelection!!
                        check(intent.adoptCommittedAutomaticCandidate(effective.candidateId, effective.height,
                            effective.qualityAuto, pending.candidateOwner))
                        session.end()
                        observed.set(initial.copy(selection = standing.copy(quality =
                            QualitySelection.AutoCandidate(intent.automaticCandidateHeight, intent.automaticCandidateId!!))))
                        session.begin(pending.bootstrap!!, observed::get, transport = transport)
                    }
                    callbackFinished.countDown()
                })
            assertNotNull(firstRequest.poll(5, TimeUnit.SECONDS))
            observed.set(initial.copy(acknowledgement = committed()))
            session.reportEvidence()
            assertTrue(commitReceived.await(5, TimeUnit.SECONDS))
            assertEquals("copy-original", intent.automaticCandidateId)
            assertEquals(0, requests.count { it.first == bootstrap.url })
            val commit = requests.single { it.second.acknowledgement?.state == AcknowledgementState.COMMITTED }.second
            assertEquals(pending.standingSelection, commit.selection)
            allowCommitAnswer.countDown()
            assertTrue(callbackFinished.await(5, TimeUnit.SECONDS))
            if (transfer) {
                val next = successorRequest.poll(5, TimeUnit.SECONDS)
                assertNotNull(next)
                assertEquals(successor, next!!.generation)
                assertEquals(QualitySelection.AutoCandidate(2160, "processed"), next.selection.quality)
                assertNull(next.acknowledgement)
            } else {
                assertEquals("copy-original", intent.automaticCandidateId)
                assertEquals(0, requests.count { it.first == bootstrap.url })
            }
            assertFalse(requests.any { it.second.demand == PlaybackDemand.END })
        } finally {
            allowCommitAnswer.countDown()
            session.end()
            scope.cancel()
        }
    }

    @Test fun exitDiagnosticsUseFiniteOriginsAndBoundedKeyRepeats() {
        for (origin in PlaybackExitOrigin.entries) {
            val detail = playbackExitDetail(origin, PlayerContractInput.Back, Int.MAX_VALUE)
            assertTrue(detail.contains("origin=${origin.wire}"))
            assertTrue(detail.contains("input=back repeat=9999"))
            assertTrue(detail.length < 100)
        }
        assertEquals("origin=remote_stop input=none repeat=0", playbackExitDetail(PlaybackExitOrigin.RemoteStop))
    }
}
