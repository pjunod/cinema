package tv.plurx.app.data

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.player.PlaybackDemand
import tv.plurx.app.player.RenderState
import java.io.IOException

// Synthetic B through the actual authenticated client; protocol evidence only.
class SharedPlaybackSessionTest {
    private val instance = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"

    @Test fun statusIsATypedTokenSafeDtoBoundToItsPlayback(): Unit = runBlocking {
        val f = SharedFixture("101"); f.login()
        val client = SharedDecisionClient.forTest(f.transport())
        val playback = client.start(f.context(), f.plan(f.context()).request)
        val wire = Json.parseToJsonElement(f.status(1)).jsonObject
        fun variant(change: (JsonObject) -> JsonObject) = JsonObject(wire + ("status" to change(wire.getValue("status").jsonObject))).toString().toByteArray()
        val status = SharedPlaybackStatus.decode(variant { JsonObject(it + mapOf("ahead_seconds" to JsonPrimitive(12), "http_wait_count" to JsonPrimitive(2),
            "suspended" to JsonPrimitive(true), "render_state" to JsonPrimitive("rendering"))) }, playback)
        assertEquals(f.session(1), status.sessionId)
        assertEquals("Shared HLS · 720p · copy · ready · 12 s ahead · 2 waiting · suspended", status.summary)
        assertEquals(720L, status.metrics.targetHeight); assertEquals(12L, status.metrics.aheadSeconds); assertEquals("rendering", status.metrics.renderState)
        assertTrue(status.metrics.admitted && status.metrics.suspended && !status.metrics.final)
        assertEquals(f.session(1), client.status(playback).sessionId)
        // Words are the server's status tokens; prose, paths and overlong words never render.
        for (word in listOf("not ready", "/srv/a.mkv", "x".repeat(33), "")) {
            assertTrue(word, runCatching { SharedPlaybackStatus.decode(variant { JsonObject(it + ("producer_state" to JsonPrimitive(word))) }, playback) }.isFailure)
        }
        assertTrue(runCatching { SharedPlaybackStatus.decode(variant { JsonObject(it + ("producer_failed" to JsonPrimitive("boom"))) }, playback) }.isFailure)
        assertTrue(runCatching { SharedPlaybackStatus.decode(variant { JsonObject(it + ("file_id" to JsonPrimitive(7))) }, playback) }.isFailure)
        // Another started session's status cannot bind to this playback.
        val foreign = JsonObject(wire + ("session_id" to JsonPrimitive(f.session(2)))).toString().toByteArray()
        assertTrue(runCatching { SharedPlaybackStatus.decode(foreign, playback) }.isFailure)
    }

    @Test fun controlUsesTheExactBTupleOrderedSequencesAndTheFrozenRawSelection(): Unit = runBlocking {
        val f = SharedFixture("102"); f.login()
        val client = SharedDecisionClient.forTest(f.transport())
        // Manual 144 asked; the encoder may deliver 72. The control still says 144.
        val plan = f.plan(f.context(), "transcode", SharedSelection(PlaybackQuality.Q144))
        assertEquals(144, plan.request.height); assertEquals(false, plan.request.copy)
        val playback = client.start(plan.subject.context, plan.request)
        val channel = SharedControlChannel(client, playback, instance, sharedControlCapabilities(plan.caps))
        val state = SharedControlState(PlaybackDemand.ACTIVE, 40_000, 45_000, RenderState.RENDERING)
        assertEquals(SharedControlOutcome.Accepted("none"), channel.exchange(state, plan.frozenControlSelection()))
        val first = f.body("control ${f.session(1)}")
        assertEquals(f.generation(1), first.getValue("generation").jsonPrimitive.content)
        assertEquals(1L, first.getValue("control_epoch").jsonPrimitive.long)
        assertEquals(instance, first.getValue("client_instance_id").jsonPrimitive.content)
        assertEquals(1L, first.getValue("sequence").jsonPrimitive.long)
        assertEquals(buildJsonObject { put("mode", "manual"); put("height", 144) }, first.getValue("selection").jsonObject.getValue("quality"))
        assertEquals(JsonArray(emptyList()), first.getValue("supported_actions"))
        assertEquals(listOf("sdr"), first.getValue("capabilities").jsonObject.getValue("dynamic_ranges").jsonArray.map { it.jsonPrimitive.content })
        assertFalse(first.getValue("capabilities").jsonObject.getValue("dual_player_preparation").jsonPrimitive.boolean)
        assertNull(first["seek_target_ms"])

        // A seek: seeking with its target; later sequences carry no capabilities.
        channel.exchange(state.copy(renderState = RenderState.SEEKING, seekTargetMs = 70_000), plan.frozenControlSelection())
        val second = f.body("control ${f.session(1)}", 1)
        assertEquals(2L, second.getValue("sequence").jsonPrimitive.long); assertNull(second["capabilities"])
        assertEquals("seeking", second.getValue("render_state").jsonPrimitive.content); assertEquals(70_000L, second.getValue("seek_target_ms").jsonPrimitive.long)
        assertEquals(instance, second.getValue("client_instance_id").jsonPrimitive.content)

        // Deferred and uncertain exchanges resend the same bytes under the same sequence.
        f.control = { n, body -> if (n == 3) 429 to "{\"code\":\"control_rate_limited\",\"retry_after_ms\":250}" else 200 to f.accepted(body) }
        assertTrue(channel.exchange(state.copy(demand = PlaybackDemand.HOLD), plan.frozenControlSelection()) is SharedControlOutcome.Accepted)
        val rateLimited = f.bodies.filter { it.first.startsWith("control") }.drop(2).map { it.second }
        assertEquals(2, rateLimited.size); assertEquals(rateLimited[0], rateLimited[1])
        assertEquals(3L, Json.parseToJsonElement(rateLimited[0]).jsonObject.getValue("sequence").jsonPrimitive.long)
        var lost = true
        val flaky = SharedDecisionClient.forTest(okhttp3.OkHttpClient.Builder().addInterceptor { chain ->
            if (lost) { lost = false; val buffer = okio.Buffer(); chain.request().body!!.writeTo(buffer); f.bodies += "lost" to buffer.readUtf8(); throw IOException("lost answer") }
            f.transport().newCall(chain.request()).execute()
        }.build())
        val retrying = SharedControlChannel(flaky, playback, instance, sharedControlCapabilities(plan.caps))
        assertTrue(retrying.exchange(state, plan.frozenControlSelection()) is SharedControlOutcome.Accepted)
        assertEquals(f.bodies.last { it.first == "lost" }.second, f.bodies.last { it.first.startsWith("control") }.second)

        // Refusals are typed and never retried.
        val before = f.controls
        f.control = { _, _ -> 409 to "{\"code\":\"stale_control\"}" }
        assertEquals(SharedControlOutcome.Refused(409, "stale_control"), channel.exchange(state, plan.frozenControlSelection()))
        f.control = { _, _ -> 410 to "{\"code\":\"session_ended\"}" }
        assertEquals(SharedControlOutcome.Ended("session_ended"), channel.exchange(state, plan.frozenControlSelection()))
        assertEquals(before + 2, f.controls)
        // An answer that is not for this tuple and sequence is not an acceptance.
        f.control = { _, body -> 200 to f.accepted(JsonObject(body + ("generation" to JsonPrimitive(f.generation(9))))) }
        assertEquals(SharedControlOutcome.Refused(200, "protocol"), channel.exchange(state, plan.frozenControlSelection()))
        f.control = { _, body -> 200 to f.accepted(JsonObject(body + ("sequence" to JsonPrimitive(1)))) }
        assertEquals(SharedControlOutcome.Refused(200, "protocol"), channel.exchange(state, plan.frozenControlSelection()))
        assertEquals(7L, channel.sequence)
    }

    @Test fun directStartIsTheExactFiveFieldReplyBoundToThisFile(): Unit = runBlocking {
        val f = SharedFixture("103"); f.login()
        val client = SharedDecisionClient.forTest(f.transport())
        val context = f.context()
        val plan = f.plan(context, "direct_play", allowDirect = true)
        assertTrue(plan.direct)
        val direct = client.startDirect(context, plan.request)
        val sent = f.body("start")
        assertEquals("direct", sent.getValue("presentation").jsonPrimitive.content)
        listOf("height", "copy", "native_subtitles", "subtitle", "previous_session_id").forEach { assertNull(it, sent[it]) }
        assertEquals("shared-player", sent.getValue("playback_id").jsonPrimitive.content)
        assertEquals("https://b.test${f.base}/direct?session=${f.session(1)}", client.directUrl(direct))
        assertEquals(4_096L, direct.length); assertTrue(direct.playable)
        assertTrue(runCatching { direct.context.localId() }.isFailure)
        val reply = Json.parseToJsonElement(f.directReply(2)).jsonObject
        for (bad in listOf(
            JsonObject(reply + ("control_epoch" to JsonPrimitive(1))),
            JsonObject(reply - "mime"),
            JsonObject(reply + ("mime" to JsonPrimitive("text/html"))),
            JsonObject(reply + ("url" to JsonPrimitive("/api/v1/files/7/direct?session=${f.session(2)}"))),
            JsonObject(reply + ("url" to JsonPrimitive("${f.base}/direct?session=${f.session(3)}"))),
            JsonObject(reply + ("length" to JsonPrimitive(-1))),
            JsonObject(reply + ("length" to JsonPrimitive("4096"))),
            JsonObject(reply + ("presentation" to JsonPrimitive("vod"))),
        )) assertTrue(bad.toString(), runCatching { SharedStartedDirect.decode(bad.toString(), context, plan.request) }.isFailure)
        assertFalse(SharedStartedDirect.decode(f.directReply(4, "audio/x-ms-wma"), context, plan.request).playable)
        // HLS-only fields are refused before any request.
        val starts = f.starts
        assertTrue(runCatching { client.startDirect(context, plan.request.copy(copy = true)) }.isFailure)
        assertTrue(runCatching { client.startDirect(context, plan.request.copy(presentation = "vod")) }.isFailure)
        assertTrue(runCatching { client.start(context, plan.request) }.isFailure)
        assertEquals(starts, f.starts)
    }

    @Test fun directIsChosenOnlyForTheUntouchedFileAndSelectionsRoundTrip(): Unit = runBlocking {
        val f = SharedFixture("104"); f.login(); val context = f.context()
        assertTrue(f.plan(context, "direct_play", allowDirect = true).direct)
        assertFalse(f.plan(context, "direct_play", allowDirect = false).direct)
        assertFalse(f.plan(context, "direct_play", SharedSelection(PlaybackQuality.Auto, audio = 2), allowDirect = true).direct)
        assertFalse(f.plan(context, "direct_play", SharedSelection(PlaybackQuality.Auto, subtitle = 4), allowDirect = true).direct)
        assertFalse(f.plan(context, "direct_play", allowDirect = true, wire = f.decision("direct_play", offset = -500)).direct)
        assertFalse(f.plan(context, "direct_play", allowDirect = true, wire = f.decision("direct_play", transcodeAudio = true)).direct)
        assertEquals(true, f.plan(context, "direct_play").request.copy)
        assertFalse(f.plan(context, "remux", allowDirect = true).direct)
        for ((method, selection) in listOf("remux" to SharedSelection(PlaybackQuality.Auto), "remux" to SharedSelection(PlaybackQuality.Original, audio = 2),
                "transcode" to SharedSelection(PlaybackQuality.Q720, subtitle = 4), "transcode" to SharedSelection(PlaybackQuality.Q144))) {
            val plan = f.plan(context, method, selection)
            assertEquals(selection, plan.selection)
            assertEquals(selection.controlSelection(), plan.frozenControlSelection())
        }
        assertEquals(mapOf("force" to "transcode", "audio" to "2", "subtitle" to "4"), SharedSelection(PlaybackQuality.Q480, 2, 4).decisionQuery())
        assertEquals(emptyMap<String, String>(), SharedSelection(PlaybackQuality.Auto).decisionQuery())
    }
}
