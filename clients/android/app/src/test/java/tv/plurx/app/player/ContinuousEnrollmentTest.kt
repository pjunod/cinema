package tv.plurx.app.player

import com.sun.net.httpserver.HttpServer
import java.net.InetSocketAddress
import java.security.MessageDigest
import java.util.Collections
import java.util.concurrent.Executors
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonArray
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Test
import tv.plurx.app.data.ClientInfo
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Session
import tv.plurx.app.data.VideoDecoderLimit
import tv.plurx.app.data.capsDocument
import tv.plurx.app.data.videoCodecCaps

/**
 * Ruling R5 (2026-10-04): continuous enrollment does not require the
 * display-aware Auto switch. With it off, the create body carries no intent
 * envelope (`Controller.applyAutoIntent`), and enrollment used to decline as
 * `no_intent`; the continuous start now builds its own, as web does.
 */
class ContinuousEnrollmentTest {
    private val caps = capsDocument(
        video = videoCodecCaps(listOf(VideoDecoderLimit("h264", 1080))),
        audio = listOf("aac"),
        hdrTypes = emptySet(),
        decoderDolbyVisionProfiles = emptyList(),
        client = ClientInfo("android", "146", "test device"),
    )

    /** What `Controller.sessionBody` sends with display-aware Auto off: no intent. */
    private fun displayAwareOffBody() = CreateSessionReq(
        playback_id = "presentation",
        request_id = "11111111-1111-4111-8111-111111111111",
        height = 720,
        start = 0.0,
        caps = caps,
        quality_auto = true,
    )

    private fun autoSelection() = ClientSelection(
        quality = QualitySelection.Auto,
        audioTrack = 0,
        subtitle = SubtitleSelection(SubtitleMode.OFF),
        audioOffsetMs = 0,
        codec = CodecPolicy.AUTO,
        dynamicRange = DynamicRangePolicy.AUTO,
    )

    @Test fun withoutAnyIntentEnrollmentStillDeclines() {
        ContinuousEnrollment("http://127.0.0.1:9", "token").use { enrollment ->
            assertEquals("no_intent", enrollment.declineReason(7, displayAwareOffBody(), null))
        }
    }

    @Test fun displayAwareOffBuildsTheContinuousIntentIndependently() {
        val previous = Session.displayAwareAuto
        Session.displayAwareAuto = false
        try {
            val body = displayAwareOffBody()
            assertNull(body.intent)
            val playbackIntent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val intent = continuousEnrollmentIntent(body, playbackIntent, autoSelection())
            assertEquals(playbackIntent.playbackId, intent.lifetime_id)
            assertEquals(QualitySelection.Auto, intent.selection.quality)
            assertEquals(SubtitleMode.OFF, intent.selection.subtitles.mode)
            ContinuousEnrollment("http://127.0.0.1:9", "token").use { enrollment ->
                assertNull(enrollment.declineReason(7, body, intent))
            }
        } finally {
            Session.displayAwareAuto = previous
        }
    }

    @Test fun aDisplayAwareEnvelopeIsKept() {
        val playbackIntent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val displayAware = playbackIntent.mediaIntent(autoSelection())
        val body = displayAwareOffBody().copy(intent = displayAware)
        assertSame(displayAware, continuousEnrollmentIntent(body, PlaybackIntent(initialQuality = PlaybackQuality.Auto), autoSelection()))
    }

    @Test fun theIndependentIntentStillDeclinesWhatContinuousCannotCarry() {
        val playbackIntent = PlaybackIntent(initialQuality = PlaybackQuality.Original)
        val original = continuousEnrollmentIntent(displayAwareOffBody(), playbackIntent,
            autoSelection().copy(quality = QualitySelection.Original))
        val native = continuousEnrollmentIntent(displayAwareOffBody(), PlaybackIntent(initialQuality = PlaybackQuality.Auto),
            autoSelection().copy(subtitle = SubtitleSelection(SubtitleMode.NATIVE, 2)))
        ContinuousEnrollment("http://127.0.0.1:9", "token").use { enrollment ->
            assertEquals("original", enrollment.declineReason(7, displayAwareOffBody(), original))
            assertEquals("native_subtitles", enrollment.declineReason(7, displayAwareOffBody(), native))
        }
    }

    private fun candidate(height: Int, seed: Int): JsonObject {
        val digest = (0 until 32).map { (it * 7 + seed) and 255 }
        val hash = MessageDigest.getInstance("SHA-256")
        hash.update("plurx:auto-quality-candidate:v1\u0000".toByteArray(Charsets.UTF_8))
        val id = hash.digest(digest.map { it.toByte() }.toByteArray()).take(16)
            .joinToString("") { "%02x".format(it.toInt() and 255) }
        return buildJsonObject {
            put("id", id)
            put("recipe_digest", buildJsonArray { digest.forEach { add(kotlinx.serialization.json.JsonPrimitive(it)) } })
            put("route", "encode"); put("width", height * 16 / 9); put("height", height); put("target_height", height)
            put("peak_bps", height * 5_000L); put("grade", "sdr"); put("decoder_compatible", true)
            put("complete_cache", false); put("sustainable", true)
        }
    }

    @Test fun displayAwareOffEnrollmentReachesTheContinuousStartWithItsOwnIntent() = runBlocking {
        val primary = candidate(720, 1)
        val companion = candidate(480, 2)
        val primaryId = primary["id"]!!.jsonPrimitive.content
        val companionId = companion["id"]!!.jsonPrimitive.content
        val posted = Collections.synchronizedMap(mutableMapOf<String, JsonObject>())
        val server = HttpServer.create(InetSocketAddress("127.0.0.1", 0), 0)
        val executor = Executors.newSingleThreadExecutor { Thread(it, "continuous-enrollment-fixture").apply { isDaemon = true } }
        server.executor = executor
        server.createContext("/api/v1/") { exchange ->
            val path = exchange.requestURI.path
            posted[path] = Json.parseToJsonElement(exchange.requestBody.readBytes().decodeToString()).jsonObject
            if (path.endsWith("/continuous-candidates")) {
                val body = buildJsonObject {
                    put("version", 1)
                    put("candidates", buildJsonArray { add(primary); add(companion) })
                    put("pairs", buildJsonArray { add(buildJsonObject {
                        put("primary_candidate_id", primaryId); put("companion_candidate_id", companionId)
                    }) })
                }.toString().toByteArray()
                exchange.sendResponseHeaders(200, body.size.toLong())
                exchange.responseBody.use { it.write(body) }
            } else {
                // Stop at the start: what it carried is the whole question.
                exchange.sendResponseHeaders(404, -1)
            }
            exchange.close()
        }
        server.start()
        val previous = Session.displayAwareAuto
        Session.displayAwareAuto = false
        try {
            val body = displayAwareOffBody()
            val playbackIntent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val intent = continuousEnrollmentIntent(body, playbackIntent, autoSelection())
            val started = ContinuousEnrollment("http://127.0.0.1:${server.address.port}", "token").use {
                it.open(7, body, intent)
            }
            assertNull(started)
            // The catalog sees the create body unchanged, as on web.
            val catalog = posted.getValue("/api/v1/files/7/hls/continuous-candidates")
            assertNull(catalog.getValue("start").jsonObject["intent"])
            // The start carries the independently built envelope, bound to the primary candidate.
            val start = posted.getValue("/api/v1/files/7/hls/continuous-sessions")
            assertEquals(primaryId, start.getValue("primary_candidate_id").jsonPrimitive.content)
            assertEquals(companionId, start.getValue("companion_candidate_id").jsonPrimitive.content)
            val wire = start.getValue("start").jsonObject
            assertEquals("true", wire.getValue("quality_auto").jsonPrimitive.content)
            val envelope = wire.getValue("intent").jsonObject
            assertEquals(playbackIntent.playbackId, envelope.getValue("lifetime_id").jsonPrimitive.content)
            val quality = envelope.getValue("selection").jsonObject.getValue("quality").jsonObject
            assertEquals("auto", quality.getValue("mode").jsonPrimitive.content)
            assertEquals(primaryId, quality.getValue("candidate_id").jsonPrimitive.content)
            assertEquals("720", quality.getValue("height").jsonPrimitive.content)
        } finally {
            Session.displayAwareAuto = previous
            server.stop(0)
            executor.shutdownNow()
        }
    }
}
