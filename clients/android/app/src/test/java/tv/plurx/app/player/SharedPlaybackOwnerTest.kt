package tv.plurx.app.player

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.SharedDecisionClient
import tv.plurx.app.data.SharedFixture
import tv.plurx.app.data.SharedSelection
import tv.plurx.app.data.SharedStartedDirect

// Synthetic B through the actual authenticated client and a recording renderer.
// Protocol and ordering evidence only: no Source, relay or device playback.
class SharedPlaybackOwnerTest {
    private class Renderer(private val log: MutableList<String>, private val rejectAttach: Boolean = false) : SharedRenderer {
        var position = 0L; var active = false; var presented = true
        override fun attachHls(url: String, positionMs: Long, playWhenReady: Boolean) { if (rejectAttach) error("decoder allocation failed"); log += "attach hls $url @$positionMs"; position = positionMs; active = playWhenReady }
        override fun attachDirect(url: String, positionMs: Long, playWhenReady: Boolean) { if (rejectAttach) error("decoder allocation failed"); log += "attach direct $url @$positionMs"; position = positionMs; active = playWhenReady }
        override fun seekTo(positionMs: Long) { log += "seek $positionMs"; position = positionMs }
        override fun setPlaying(playing: Boolean) { log += "playing $playing"; active = playing }
        override fun snapshot() = SharedRendererSnapshot(position, position + 4_000, 90_000, active, RenderState.RENDERING, framePresented = presented)
        override fun release() { log += "release" }
        // These owners never declare a Shared successor, so nothing primes one.
        override fun prepareSuccessor(url: String, positionMs: Long, textEnabled: Boolean) = error("no successor without the declaration")
        override fun successorSnapshot(): SharedSuccessorSnapshot? = null
        override fun parkSuccessor(positionMs: Long) = error("no successor")
        override fun switchToSuccessor() = error("no successor")
        override fun restorePredecessor() = error("no successor")
        override fun releasePredecessor() = error("no successor")
        override fun releaseSuccessor() = error("no successor")
    }

    @Test fun rendererSetupFailureEndsTheStartedSessionForHlsAndDirect(): Unit = runBlocking {
        for (direct in listOf(false, true)) {
            val f = SharedFixture(if (direct) "206" else "205"); f.login()
            val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, Renderer(f.log, rejectAttach = true))
            val plan = f.plan(f.context(), if (direct) "direct_play" else "remux", allowDirect = direct)
            assertTrue(runCatching { owner.start(plan) }.isFailure)
            assertEquals(listOf("start", "delete ${f.session(1)}"), f.log)
            assertNull(owner.currentSession)
            owner.stop()
        }
    }

    @Test fun noRenderedFrameNeverWritesZeroHistoryOnTickOrClose(): Unit = runBlocking {
        val f = SharedFixture("207"); f.login()
        val renderer = Renderer(f.log).also { it.presented = false }
        val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, renderer)
        owner.start(f.plan(f.context()))
        renderer.position = 0
        owner.tickNow(progress = true)
        owner.stop()
        assertFalse(f.log.any { it.startsWith("progress") })
        assertEquals("delete ${f.session(1)}", f.log.last())
    }

    @Test fun seekPauseAndPlayReachTheRendererOnlyAfterBAccepts(): Unit = runBlocking {
        val f = SharedFixture("201"); f.login()
        val renderer = Renderer(f.log)
        val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, renderer)
        owner.start(f.plan(f.context()))
        assertEquals(listOf("start", "attach hls https://b.test/api/v1/hls/${f.session(1)}/master.m3u8 @12500"), f.log)
        val control = "control ${f.session(1)}"

        f.control = { _, body -> assertFalse(f.log.contains("seek 30000")); 200 to f.accepted(body) }
        owner.seek(30_000)
        assertEquals(listOf(control, "seek 30000"), f.log.drop(2))
        val seek = f.body(control)
        assertEquals("seeking", seek.getValue("render_state").jsonPrimitive.content); assertEquals(30_000L, seek.getValue("seek_target_ms").jsonPrimitive.long)
        assertEquals(1L, seek.getValue("sequence").jsonPrimitive.long); assertNotNull(seek["capabilities"])

        // A refused pause leaves the picture playing and says why.
        f.control = { _, _ -> 409 to "{\"code\":\"stale_control\"}" }
        owner.setPlaying(false)
        assertEquals(control, f.log.last()); assertTrue(renderer.active)
        assertTrue(owner.failure.value!!.contains("stale_control"))

        f.control = { _, body -> assertTrue(renderer.active); 200 to f.accepted(body) }
        owner.setPlaying(false)
        assertEquals("playing false", f.log.last()); assertFalse(renderer.active)
        assertEquals("hold", f.body(control, 2).getValue("demand").jsonPrimitive.content)
        f.control = { _, body -> assertFalse(renderer.active); 200 to f.accepted(body) }
        owner.setPlaying(true)
        assertEquals("playing true", f.log.last())
        assertEquals(listOf(1L, 2L, 3L, 4L), f.bodies.filter { it.first == control }.map { Json.parseToJsonElement(it.second).jsonObject.getValue("sequence").jsonPrimitive.long })
        assertEquals(1, f.bodies.filter { it.first == control }.map { Json.parseToJsonElement(it.second).jsonObject.getValue("client_instance_id") }.toSet().size)

        // B retired the session while paused: play is a fresh Start at the position, never the dead URL.
        f.control = { _, body -> 200 to f.accepted(body) }
        owner.setPlaying(false)
        f.control = { _, _ -> 410 to "{\"code\":\"session_ended\"}" }
        owner.tickNow(progress = false)
        val before = f.log.size
        owner.setPlaying(true)
        assertEquals(listOf("start", "attach hls https://b.test/api/v1/hls/${f.session(2)}/master.m3u8 @30000", "delete ${f.session(1)}"), f.log.drop(before))
        assertEquals(30.0, f.body("start", 1).getValue("start").jsonPrimitive.double, 0.0)
        owner.stop()
        assertEquals(listOf("playing false", "progress", "release", "delete ${f.session(2)}"), f.log.takeLast(4))
    }

    @Test fun directedChangeReopensOnNoneCarriesProgressAndReleasesThePredecessorAfterAttach(): Unit = runBlocking {
        val f = SharedFixture("202"); f.login()
        val renderer = Renderer(f.log)
        val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, renderer)
        owner.start(f.plan(f.context()))
        owner.tickNow(progress = true)
        assertEquals(8L, f.body("progress").getValue("sequence").jsonPrimitive.long)
        assertEquals("Shared HLS · 720p · copy · ready", owner.statusSummary.value)
        renderer.position = 40_000
        f.decisionWire = { query -> assertEquals("force=transcode&audio=2", query); f.decision("transcode") }
        val before = f.log.size
        owner.change(SharedSelection(PlaybackQuality.Q720, audio = 2))
        assertEquals(listOf("control ${f.session(1)}", "decision?force=transcode&audio=2", "start",
            "attach hls https://b.test/api/v1/hls/${f.session(2)}/master.m3u8 @40000", "delete ${f.session(1)}"), f.log.drop(before))
        // The ask went to the old session raw; the reopen is a plain initial Start.
        val ask = f.body("control ${f.session(1)}", 1).getValue("selection").jsonObject
        assertEquals(buildJsonObject { put("mode", "manual"); put("height", 720) }, ask.getValue("quality")); assertEquals(2, ask.getValue("audio_track").jsonPrimitive.int)
        val first = f.body("start"); val reopen = f.body("start", 1)
        assertEquals(first.getValue("playback_id"), reopen.getValue("playback_id"))
        assertNotEquals(first.getValue("request_id"), reopen.getValue("request_id"))
        listOf("previous_session_id", "control_sequence", "reopen_reason", "intent").forEach { assertNull(it, reopen[it]) }
        assertEquals(40.0, reopen.getValue("start").jsonPrimitive.double, 0.0); assertEquals(720, reopen.getValue("height").jsonPrimitive.int)
        assertEquals(false, reopen.getValue("copy").jsonPrimitive.boolean); assertEquals(2, reopen.getValue("audio").jsonPrimitive.int)
        assertEquals(SharedSelection(PlaybackQuality.Q720, audio = 2), owner.selection.value)

        // The new session controls from sequence 1 with the new frozen ask, and the
        // ordered progress sequence carries on rather than restarting.
        owner.tickNow(progress = true)
        val renewal = f.body("control ${f.session(2)}")
        assertEquals(1L, renewal.getValue("sequence").jsonPrimitive.long); assertEquals(f.generation(2), renewal.getValue("generation").jsonPrimitive.content)
        assertEquals(720, renewal.getValue("selection").jsonObject.getValue("quality").jsonObject.getValue("height").jsonPrimitive.int)
        val beat = f.body("progress", 1)
        assertEquals(f.session(2), beat.getValue("session_id").jsonPrimitive.content); assertEquals(9L, beat.getValue("sequence").jsonPrimitive.long)

        // A refused change keeps the current session and selection.
        f.control = { _, _ -> 409 to "{\"code\":\"owner_changed\"}" }
        val starts = f.starts
        owner.change(SharedSelection(PlaybackQuality.Original))
        assertEquals(starts, f.starts); assertEquals(SharedSelection(PlaybackQuality.Q720, audio = 2), owner.selection.value)
        assertFalse(f.log.contains("delete ${f.session(2)}"))
        owner.stop()
    }

    @Test fun directPlayHasNoControlAndRestartsOnceAfterExpiry(): Unit = runBlocking {
        val f = SharedFixture("203"); f.login()
        val renderer = Renderer(f.log)
        val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, renderer)
        owner.start(f.plan(f.context(), "direct_play", allowDirect = true))
        val url = { n: Int -> "https://b.test${f.base}/direct?session=${f.session(n)}" }
        assertEquals(listOf("start", "attach direct ${url(1)} @12500"), f.log)
        assertTrue(owner.direct.value && owner.currentSession is SharedStartedDirect)
        owner.seek(50_000); owner.setPlaying(false); owner.setPlaying(true)
        owner.tickNow(progress = true)
        assertEquals(listOf("seek 50000", "playing false", "playing true", "progress"), f.log.drop(2))
        assertEquals(f.session(1), f.body("progress").getValue("session_id").jsonPrimitive.content)

        // Before the attachment reached its timeline there is no allowance.
        assertNull(owner.rendererFailed(410, "gone"))
        owner.timelineReached()
        val before = f.log.size
        owner.rendererFailed(410, "gone")!!.join()
        assertEquals(listOf("start", "attach direct ${url(2)} @50000", "delete ${f.session(1)}"), f.log.drop(before))
        assertEquals("direct", f.body("start", 1).getValue("presentation").jsonPrimitive.content)
        assertEquals(50.0, f.body("start", 1).getValue("start").jsonPrimitive.double, 0.0)
        // The new attachment has not reached its timeline: the allowance is spent.
        assertNull(owner.rendererFailed(404, "gone"))
        assertEquals(2, f.starts)

        // A directed change from direct play goes to Copy HLS.
        f.decisionWire = { f.decision("direct_play") }
        owner.change(SharedSelection(PlaybackQuality.Auto, audio = 2))
        assertEquals(listOf("decision?audio=2", "start"), f.log.takeLast(4).take(2))
        assertEquals("vod", f.body("start", 2).getValue("presentation").jsonPrimitive.content)
        assertEquals(true, f.body("start", 2).getValue("copy").jsonPrimitive.boolean)
        assertEquals("delete ${f.session(2)}", f.log.last())
        assertFalse(owner.direct.value)
        owner.stop()
        assertEquals("delete ${f.session(3)}", f.log.last())
    }

    @Test fun aDirectTypeExoPlayerCannotReadIsReleasedAndPlayedAsCopyHls(): Unit = runBlocking {
        val f = SharedFixture("204"); f.login()
        f.startReply = { n, body -> if (body["presentation"]?.jsonPrimitive?.content == "direct") f.directReply(n, "audio/x-ms-wma") else f.hlsReply(n) }
        val owner = SharedPlaybackOwner(this, { SharedDecisionClient.forTest(f.transport()) }, Renderer(f.log))
        owner.start(f.plan(f.context(), "direct_play", allowDirect = true))
        assertEquals(listOf("start", "delete ${f.session(1)}", "start", "attach hls https://b.test/api/v1/hls/${f.session(2)}/master.m3u8 @12500"), f.log)
        assertEquals(true, f.body("start", 1).getValue("copy").jsonPrimitive.boolean)
        assertEquals(f.body("start").getValue("playback_id"), f.body("start", 1).getValue("playback_id"))
        owner.stop()
    }
}
