package tv.plurx.app.player

import kotlinx.coroutines.delay
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.SharedDecisionClient
import tv.plurx.app.data.SharedFixture
import tv.plurx.app.data.SharedSelection
import java.io.IOException

/**
 * The Shared prepared successor (P1/P2) through the actual authenticated
 * client, a synthetic B and a recording two-pipeline renderer. Protocol and
 * ordering evidence only: no Source, relay or device playback.
 */
class SharedPreparedHandoffTest {
    /** An incumbent whose film clock runs in real time while playing, and a parked successor. */
    private class Renderer(private val log: MutableList<String>) : SharedRenderer {
        private var base = 0L; private var since = System.nanoTime(); var active = false
        var successorAt: Long? = null; var switched = false; var successorReady = true; var frame: Long? = 1_800_000_000_000
        fun position() = if (active) base + (System.nanoTime() - since) / 1_000_000 else base
        private fun moveTo(value: Long) { base = value; since = System.nanoTime() }
        override fun attachHls(url: String, positionMs: Long, playWhenReady: Boolean) { log += "attach hls $url @$positionMs"; moveTo(positionMs); active = playWhenReady }
        override fun attachDirect(url: String, positionMs: Long, playWhenReady: Boolean) { log += "attach direct $url" }
        override fun seekTo(positionMs: Long) { log += "seek $positionMs"; moveTo(positionMs) }
        override fun setPlaying(playing: Boolean) { moveTo(position()); active = playing }
        override fun snapshot() = SharedRendererSnapshot(position(), position() + 30_000, 900_000, active, RenderState.RENDERING)
        override fun release() { log += "release" }
        override fun prepareSuccessor(url: String, positionMs: Long, textEnabled: Boolean) { log += "prepare successor $url"; successorAt = positionMs }
        override fun successorSnapshot() = successorAt?.let { at ->
            // A parked successor holds still on its rendezvous with runway through it.
            SharedSuccessorSnapshot(successorReady, if (switched) position() else at, at + 10_000, seekLanded = true, failed = false,
                firstFrameUnixMs = if (switched) frame else null)
        }
        override fun parkSuccessor(positionMs: Long) { log += "park"; successorAt = positionMs }
        override fun switchToSuccessor() { log += "switch"; switched = true }
        override fun restorePredecessor() { log += "restore predecessor"; switched = false; successorAt = null }
        override fun releasePredecessor() { log += "release predecessor"; successorAt = null }
        override fun releaseSuccessor() { log += "release successor"; successorAt = null }
    }

    private val actionId = "a1a1a1a1-a1a1-41a1-81a1-a1a1a1a1a1a1"
    private fun offer(f: SharedFixture, body: JsonObject, preparation: String = "offered") = buildJsonObject {
        val session = f.session(9)
        put("protocol", "plurx-playback-control-v1"); put("generation", body.getValue("generation")); put("control_epoch", body.getValue("control_epoch"))
        put("accepted_sequence", body.getValue("sequence"))
        put("action", buildJsonObject {
            put("type", "prepare"); put("action_id", actionId); put("session_id", session); put("playlist_url", "/api/v1/hls/$session/index.m3u8")
            put("control", buildJsonObject { put("protocol", "plurx-playback-control-v1"); put("url", "/api/v1/hls/$session/control"); put("generation", f.generation(9))
                put("control_epoch", 1); put("next_exchange_ms", 5_000); put("lease_timeout_ms", 300_000) })
            put("media_origin_ms", 0)
            put("effective_selection", buildJsonObject { put("quality_auto", false); put("height", 720); put("audio_offset_ms", 0); put("codec", "server_selected") })
        })
        put("delivery", buildJsonObject { put("preparation", preparation) })
    }.toString()
    private fun ack(body: JsonObject) = body["acknowledgement"]?.jsonObject
    private val manual720 = SharedSelection(PlaybackQuality.Q720)

    private fun owner(scope: kotlinx.coroutines.CoroutineScope, f: SharedFixture, renderer: Renderer) =
        SharedPlaybackOwner(scope, { SharedDecisionClient.forTest(f.transport()) }, renderer, preparedHandoff = true)

    private suspend fun until(what: String, condition: () -> Boolean) {
        withTimeout(15_000) { while (!condition()) delay(20) }
        assertTrue(what, condition())
    }

    @Test fun offeredSuccessorPrimesCommitsOnThePredecessorAndMovesControlAndProgress(): Unit = runBlocking {
        val f = SharedFixture("401"); f.login()
        val renderer = Renderer(f.log)
        val owner = owner(this, f, renderer)
        owner.start(f.plan(f.context()))
        f.decisionWire = { query -> assertEquals("force=transcode", query); f.decision("transcode") }
        var lostCommit = true
        f.control = { _, body ->
            val ack = ack(body)
            when {
                ack == null && body.getValue("sequence").jsonPrimitive.long == 1L -> 200 to f.accepted(body, "staging")
                ack?.get("state")?.jsonPrimitive?.content == "committed" -> {
                    // The first commit's answer is lost; the same bytes go again.
                    if (lostCommit) { lostCommit = false; throw IOException("lost answer") }
                    200 to f.accepted(body, "none")
                }
                else -> 200 to offer(f, body)
            }
        }
        owner.change(manual720)
        until("the handoff settles") { owner.currentSession?.sessionId == f.session(9) }
        val predecessor = f.bodies.filter { it.first == "control ${f.session(1)}" }.map { Json.parseToJsonElement(it.second).jsonObject }
        // Sequence 1 declared both names and dual-player preparation.
        val first = predecessor.first()
        assertEquals(listOf("prepare_replacement", "shared_prepare_replacement"), first.getValue("supported_actions").jsonArray.map { it.jsonPrimitive.content })
        assertTrue(first.getValue("capabilities").jsonObject.getValue("dual_player_preparation").jsonPrimitive.boolean)
        // Every exchange on the predecessor carried the new ask, so B kept the successor.
        predecessor.forEach { assertEquals(720, it.getValue("selection").jsonObject.getValue("quality").jsonObject.getValue("height").jsonPrimitive.int) }
        val states = predecessor.mapNotNull { ack(it)?.getValue("state")?.jsonPrimitive?.content }
        assertEquals(listOf("metadata_ready", "buffer_ready", "committed", "committed"), states)
        val commits = f.bodies.filter { it.first == "control ${f.session(1)}" && it.second.contains("\"committed\"") }.map { it.second }
        assertEquals("a lost commit answer is asked again byte for byte", 1, commits.toSet().size)
        val commit = ack(Json.parseToJsonElement(commits.first()).jsonObject)!!
        assertEquals(actionId, commit.getValue("action_id").jsonPrimitive.content)
        assertEquals(0L, commit.getValue("committed_media_origin_ms").jsonPrimitive.long)
        assertEquals(1_800_000_000_000L, commit.getValue("first_frame_unix_ms").jsonPrimitive.long)
        assertTrue(f.log.contains("prepare successor https://b.test/api/v1/hls/${f.session(9)}/index.m3u8"))
        assertEquals(listOf("switch", "release predecessor"), f.log.filter { it == "switch" || it.startsWith("release") || it.startsWith("restore") })
        // No reopen, and B (not this client) retires the predecessor.
        assertEquals(1, f.starts); assertFalse(f.log.contains("delete ${f.session(1)}"))
        assertEquals(manual720, owner.selection.value)

        // Control and progress follow the successor's B tuple, from sequence 1.
        owner.tickNow(progress = true)
        val renewal = f.body("control ${f.session(9)}")
        assertEquals(1L, renewal.getValue("sequence").jsonPrimitive.long); assertEquals(f.generation(9), renewal.getValue("generation").jsonPrimitive.content)
        assertNotNull(renewal["capabilities"])
        assertEquals(720, renewal.getValue("selection").jsonObject.getValue("quality").jsonObject.getValue("height").jsonPrimitive.int)
        val beat = f.body("progress")
        assertEquals(f.session(9), beat.getValue("session_id").jsonPrimitive.content); assertEquals(8L, beat.getValue("sequence").jsonPrimitive.long)
        owner.stop()
        assertEquals("delete ${f.session(9)}", f.log.last())
        assertFalse(f.log.contains("delete ${f.session(1)}"))
    }

    @Test fun noneOnTheAskDeclinesAtOnceAndReopens(): Unit = runBlocking {
        val f = SharedFixture("402"); f.login()
        val renderer = Renderer(f.log)
        val owner = owner(this, f, renderer)
        owner.start(f.plan(f.context()))
        f.decisionWire = { f.decision("transcode") }
        f.control = { _, body -> 200 to f.accepted(body, "none") }
        val before = f.log.size
        owner.change(manual720)
        assertEquals(listOf("control ${f.session(1)}", "decision?force=transcode", "start"), f.log.drop(before).take(3))
        assertEquals("delete ${f.session(1)}", f.log.last())
        assertFalse(f.log.any { it.startsWith("prepare successor") })
        owner.stop()
    }

    @Test fun aWithdrawnSuccessorIsAcknowledgedFailedAndTheChangeReopens(): Unit = runBlocking {
        val f = SharedFixture("403"); f.login()
        val renderer = Renderer(f.log).apply { successorReady = false }
        val owner = owner(this, f, renderer)
        owner.start(f.plan(f.context()))
        f.decisionWire = { f.decision("transcode") }
        var withdrawn = false
        f.control = { _, body -> when {
            body.getValue("sequence").jsonPrimitive.long == 1L -> 200 to f.accepted(body, "staging")
            withdrawn -> 200 to f.accepted(body, "none")
            else -> 200 to offer(f, body)
        } }
        owner.change(manual720)
        until("the successor primes") { f.log.any { it.startsWith("prepare successor") } }
        // B withdraws it (its deadline, the Source's refusal); the next exchange says so.
        withdrawn = true
        owner.tickNow(progress = false)
        until("the change reopens") { f.starts == 2 }
        until("the predecessor is released") { f.log.contains("delete ${f.session(1)}") }
        val states = f.bodies.filter { it.first == "control ${f.session(1)}" }.mapNotNull { ack(Json.parseToJsonElement(it.second).jsonObject)?.getValue("state")?.jsonPrimitive?.content }
        assertEquals(listOf("failed"), states)
        assertTrue(f.log.indexOf("release successor") < f.log.lastIndexOf("start"))
        assertEquals(720, f.body("start", 1).getValue("height").jsonPrimitive.int)
        assertEquals(manual720, owner.selection.value)
        owner.stop()
    }

    @Test fun aRefusedCommitPutsThePredecessorBackAndTakesTheReopen(): Unit = runBlocking {
        val f = SharedFixture("404"); f.login()
        val renderer = Renderer(f.log)
        val owner = owner(this, f, renderer)
        owner.start(f.plan(f.context()))
        f.decisionWire = { f.decision("transcode") }
        f.control = { _, body -> when {
            body.getValue("sequence").jsonPrimitive.long == 1L -> 200 to f.accepted(body, "staging")
            ack(body)?.get("state")?.jsonPrimitive?.content == "committed" -> 409 to "{\"code\":\"stale_control\"}"
            else -> 200 to offer(f, body)
        } }
        owner.change(manual720)
        until("the change reopens") { f.starts == 2 }
        until("the predecessor is released") { f.log.contains("delete ${f.session(1)}") }
        val order = f.log.drop(2).filter { it in setOf("switch", "restore predecessor", "start") }
        assertEquals(listOf("switch", "restore predecessor", "start"), order)
        assertEquals(f.session(2), owner.currentSession?.sessionId)
        owner.stop()
    }

    @Test fun aSeekWhileStagingAbandonsTheHandoffAndReopensAtTheTarget(): Unit = runBlocking {
        val f = SharedFixture("405"); f.login()
        val renderer = Renderer(f.log)
        val owner = owner(this, f, renderer)
        owner.start(f.plan(f.context()))
        f.decisionWire = { f.decision("transcode") }
        f.control = { _, body -> 200 to f.accepted(body, "staging") }
        owner.change(manual720)
        val before = f.log.size
        owner.seek(60_000)
        assertEquals(listOf("decision?force=transcode", "start", "attach hls https://b.test/api/v1/hls/${f.session(2)}/master.m3u8 @60000", "delete ${f.session(1)}"), f.log.drop(before))
        assertEquals(720, f.body("start", 1).getValue("height").jsonPrimitive.int)
        owner.stop()
    }
}
