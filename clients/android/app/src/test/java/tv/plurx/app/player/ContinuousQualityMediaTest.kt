package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.player.ContinuousQualityFixtures.identity
import tv.plurx.app.player.ContinuousQualityFixtures.operation
import tv.plurx.app.player.ContinuousQualityFixtures.reply
import tv.plurx.app.player.ContinuousQualityFixtures.transaction

class ContinuousQualityMediaTest {
    private val init = byteArrayOf(1, 2, 3)
    private val segment = byteArrayOf(4, 5, 6, 7)
    private val parent = "/api/v1/hls/11111111-1111-4111-8111-111111111111/"
    private val rendition = "b".repeat(64)
    private fun row(id: String, height: Int) = buildJsonObject {
        put("candidate_id", (if (height == 720) "1" else "2").repeat(32)); put("rendition_id", id.repeat(64))
        put("init_id", ContinuousQualityMedia.digest(init)); put("codec", "avc1.640032")
        put("width", if (height == 720) 1280 else 1920); put("height", height); put("timescale", 24)
        put("frame_ticks", 1); put("segment_ticks", 48); put("peak_bps", 1_000_000)
        put("playlist", "video/${id.repeat(64)}/index.m3u8")
    }
    private val family get() = buildJsonObject {
        put("version", 1); put("mode", "controlled"); put("family_id", "a".repeat(64)); put("master", "master.m3u8")
        put("video", JsonArray(listOf(row("b", 720), row("c", 1080))))
    }
    private fun protocol(disposed: Boolean = false): ContinuousQualityProtocol = ContinuousQualityProtocol(identity, { request ->
        val response = reply(request); val receipt = requireNotNull(response.obj("receipt"))
        val pin = buildJsonObject {
            put("artifact_id", ContinuousQualityMedia.digest(segment)); put("rendition_id", rendition)
            put("timescale", 24); put("from_tick", 0); put("through_tick", 48); put("byte_length", segment.size)
        }
        val target = JsonObject(requireNotNull(receipt.obj("transaction")) + mapOf("state" to JsonPrimitive("scheduled"),
            "ready" to JsonArray(listOf(pin)), "reserved" to JsonArray(listOf(pin)),
            "disposed" to JsonArray(if (disposed) listOf(pin.getValue("artifact_id")) else emptyList())))
        JsonObject(response + mapOf("receipt" to JsonObject(receipt + ("transaction" to target)),
            "ledger" to JsonObject(requireNotNull(response.obj("ledger")) + ("transactions" to JsonArray(listOf(target))))))
    })

    @Test fun immutableInitializationAndExactReservedBytesAreRequiredBeforeExtraction() = runBlocking<Unit> {
        val protocol = protocol()
        val media = ContinuousQualityMedia("https://owned.invalid", parent + "quality-schedule", family, protocol)
        val initial = requireNotNull(media.resource("https://owned.invalid${parent}video/$rendition/init/${ContinuousQualityMedia.digest(init)}.mp4"))
        assertNull(media.authorize(initial, init))
        assertThrows(IOException::class.java) { media.authorize(initial, byteArrayOf(9)) }
        val fragment = requireNotNull(media.resource("https://owned.invalid${parent}video/$rendition/segment/0.m4s"))
        assertThrows(IOException::class.java) { media.authorize(fragment, segment) }
        protocol.transition(transaction, operation())
        val authorized = requireNotNull(media.authorize(fragment, segment))
        assertEquals(setOf(transaction), authorized.transactionIds)
        assertEquals(ContinuousQualityMedia.digest(segment), authorized.interval.text("artifact_id"))
        assertThrows(IOException::class.java) { media.authorize(fragment, byteArrayOf(4, 5, 6, 8)) }
        val wrongIndex = requireNotNull(media.resource("https://owned.invalid${parent}video/$rendition/segment/1.m4s"))
        assertThrows(IOException::class.java) { media.authorize(wrongIndex, segment) }
    }

    @Test fun foreignParentsRenditionsAndInitializationPathsCannotBorrowOwnership() {
        val media = ContinuousQualityMedia("https://owned.invalid", parent + "quality-schedule", family, protocol())
        assertNull(media.resource("https://foreign.invalid${parent}video/$rendition/segment/0.m4s"))
        assertNull(media.resource("https://owned.invalid/api/v1/hls/other/video/$rendition/segment/0.m4s"))
        assertThrows(IOException::class.java) { media.resource("https://owned.invalid${parent}video/${"f".repeat(64)}/segment/0.m4s") }
        assertThrows(IOException::class.java) { media.resource("https://owned.invalid${parent}video/$rendition/init/${"f".repeat(64)}.mp4") }
        assertThrows(IOException::class.java) { media.resource("https://owned.invalid${parent}video/$rendition/segment/99999999999999999999.m4s") }
    }

    @Test fun aDisposedPinAndAnOverflowedVideoFrontierCannotAuthorizeBytes() = runBlocking<Unit> {
        val protocol = protocol(disposed = true)
        protocol.transition(transaction, operation())
        val media = ContinuousQualityMedia("https://owned.invalid", parent + "quality-schedule", family, protocol)
        val fragment = requireNotNull(media.resource("https://owned.invalid${parent}video/$rendition/segment/0.m4s"))
        assertThrows(IOException::class.java) { media.authorize(fragment, segment) }
        val overflow = requireNotNull(media.resource("https://owned.invalid${parent}video/$rendition/segment/9007199254740991.m4s"))
        assertThrows(IOException::class.java) { overflow.videoFrontier() }
        assertThrows(IOException::class.java) { media.authorize(fragment, ByteArray(ContinuousQualityMedia.MAX_MEDIA_BYTES + 1)) }
    }
}
