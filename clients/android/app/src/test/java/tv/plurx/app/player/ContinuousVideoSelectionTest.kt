@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.TrackGroup
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.atomic.AtomicReference
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import tv.plurx.app.player.ContinuousQualityFixtures.identity
import tv.plurx.app.player.ContinuousQualityFixtures.transaction
import tv.plurx.app.player.ContinuousQualityFixtures.operation
import tv.plurx.app.player.ContinuousQualityFixtures.reply


class ContinuousVideoSelectionTest {
    private fun format(height: Int, bitrate: Int): Format = Format.Builder()
        .setSampleMimeType(MimeTypes.VIDEO_H264).setCodecs("avc1.640032")
        .setWidth(if (height == 720) 1280 else 1920).setHeight(height).setAverageBitrate(bitrate).build()
    private fun choice(rendition: String, revision: Long, automatic: Boolean = false) =
        ReservedVideoChoice(rendition, revision, "00000000-0000-4000-8000-${revision.toString().padStart(12, '0')}", automatic)

    @Test fun reservedFutureChoiceUsesTheSameSelectionAfterMedia3ReordersFormats() {
        val group = TrackGroup(format(720, 1_000_000), format(1080, 2_000_000))
        val current = AtomicReference<ReservedVideoChoice?>(choice("720", 1))
        val selection = ContinuousRenditionSelection(group, intArrayOf(0, 1), 0,
            mapOf(0 to "720", 1 to "1080"), requireNotNull(current.get()), current::get)
        assertEquals(0, selection.selectedIndexInTrackGroup)
        assertEquals(720, selection.selectedFormat.height)
        current.set(choice("1080", 2, automatic = true))
        selection.updateSelectedTrack(10_000_000, 90_000_000, C.TIME_UNSET, mutableListOf(), emptyArray())
        assertEquals(1, selection.selectedIndexInTrackGroup)
        assertEquals(1080, selection.selectedFormat.height)
        assertEquals(C.SELECTION_REASON_ADAPTIVE, selection.selectionReason)
        assertEquals(current.get()?.transactionId, selection.selectionData)
        assertTrue(selection.matches(intArrayOf(1, 0), 0))
        assertFalse(selection.matches(intArrayOf(0), 0))
        assertFalse(selection.matches(intArrayOf(0, 1), 1))
        assertFalse(selection.excludeTrack(selection.selectedIndex, 10_000))
    }

    @Test fun staleUnreservedAndUnsupportedChoicesKeepTheCurrentRendition() {
        val group = TrackGroup(format(720, 1_000_000), format(1080, 2_000_000))
        val current = AtomicReference<ReservedVideoChoice?>(choice("1080", 2))
        val selection = ContinuousRenditionSelection(group, intArrayOf(0, 1), 0,
            mapOf(0 to "720", 1 to "1080"), requireNotNull(current.get()), current::get)
        for (unusable in listOf(null, choice("720", 1), choice("foreign", 3))) {
            current.set(unusable)
            selection.updateSelectedTrack(10_000_000, 90_000_000, C.TIME_UNSET, mutableListOf(), emptyArray())
            assertEquals(1080, selection.selectedFormat.height)
        }
        current.set(choice("720", 3))
        selection.updateSelectedTrack(10_000_000, 90_000_000, C.TIME_UNSET, mutableListOf(), emptyArray())
        assertEquals(720, selection.selectedFormat.height)
        assertEquals(C.SELECTION_REASON_MANUAL, selection.selectionReason)
    }
    @Test fun staleColdLoadsYieldOnlyToTheBoundReservedChoiceWithoutBlacklisting() {
        val group = TrackGroup(format(720, 1_000_000), format(1080, 2_000_000))
        val current = AtomicReference<ReservedVideoChoice?>(choice("720", 1))
        val selection = ContinuousRenditionSelection(group, intArrayOf(0, 1), 0,
            mapOf(0 to "720", 1 to "1080"), requireNotNull(current.get()), current::get)
        val stale = selection.selectedIndex
        assertFalse(selection.excludeTrack(stale, 60_000))
        current.set(choice("1080", 2))
        assertTrue(selection.excludeTrack(stale, 60_000))
        assertEquals(1080, selection.selectedFormat.height)
        assertFalse(selection.excludeTrack(selection.selectedIndex, 60_000))
        assertFalse(selection.excludeTrack(-1, 60_000))
        for (unusable in listOf(null, choice("720", 1), choice("720", 2), choice("foreign", 3))) {
            current.set(unusable)
            assertFalse(selection.excludeTrack(stale, 60_000))
            assertEquals(1080, selection.selectedFormat.height)
        }
        current.set(choice("720", 3))
        selection.updateSelectedTrack(0, 0, C.TIME_UNSET, mutableListOf(), emptyArray())
        assertEquals(720, selection.selectedFormat.height)
        assertFalse(selection.isTrackExcluded(selection.selectedIndex, Long.MAX_VALUE))
    }

    @Test fun aReadyTargetCannotChangeFutureLoadsUntilItsIntervalIsReserved() = runBlocking {
        fun row(height: Int, id: String, candidate: String) = buildJsonObject {
            put("candidate_id", candidate.repeat(32)); put("rendition_id", id.repeat(64)); put("init_id", "d".repeat(64))
            put("codec", "avc1.640032"); put("width", if (height == 720) 1280 else 1920); put("height", height)
            put("timescale", 24); put("frame_ticks", 1); put("segment_ticks", 48); put("peak_bps", 1_000_000)
            put("playlist", "video/${id.repeat(64)}/index.m3u8")
        }
        val family = buildJsonObject {
            put("version", 1); put("mode", "controlled"); put("family_id", "a".repeat(64)); put("master", "master.m3u8")
            put("video", JsonArray(listOf(row(720, "b", "1"), row(1080, "c", "2"))))
        }
        val pin = buildJsonObject {
            put("artifact_id", "e".repeat(64)); put("rendition_id", "b".repeat(64)); put("timescale", 24)
            put("from_tick", 0); put("through_tick", 48); put("byte_length", 1000)
        }
        val protocol = ContinuousQualityProtocol(identity, { request ->
            val response = reply(request)
            if (request.obj("transition")?.obj("operation")?.text("kind") != "scheduled") response else {
                val receipt = requireNotNull(response.obj("receipt"))
                val target = JsonObject(requireNotNull(receipt.obj("transaction")) + mapOf(
                    "state" to JsonPrimitive("scheduled"), "ready" to JsonArray(listOf(pin)), "reserved" to JsonArray(listOf(pin))))
                JsonObject(response + mapOf("receipt" to JsonObject(receipt + ("transaction" to target)),
                    "ledger" to JsonObject(requireNotNull(response.obj("ledger")) + ("transactions" to JsonArray(listOf(target))))))
            }
        })
        val binding = ContinuousVideoSelection(family, protocol)
        protocol.transition(transaction, operation())
        assertFalse(binding.publishReserved(0, automatic = false))
        protocol.transition(transaction, buildJsonObject { put("kind", "scheduled"); put("intervals", JsonArray(listOf(pin))) })
        assertTrue(binding.publishReserved(0, automatic = false))
        assertFalse(binding.publishReserved(48, automatic = false))
        val group = TrackGroup(format(720, 1_000_000), format(1080, 2_000_000))
        val definition = androidx.media3.exoplayer.trackselection.ExoTrackSelection.Definition(group, 0, 1)
        assertTrue(binding.supportedRenditions().isEmpty())
        val selected = requireNotNull(binding.selection(definition))
        assertEquals(setOf("b".repeat(64), "c".repeat(64)), binding.supportedRenditions())
        assertEquals(720, selected.selectedFormat.height)
        assertSame(selected, binding.selection(definition))
    }

    @Test fun continuousAutoCostsIncludeSharedAudioWithoutChangingCandidateIdentity() {
        fun candidate(height: Int, raw: Long): tv.plurx.app.data.QualityCandidate {
            val recipe = List(32) { height % 256 }
            val hash = java.security.MessageDigest.getInstance("SHA-256")
            hash.update("plurx:auto-quality-candidate:v1\u0000".toByteArray(Charsets.UTF_8))
            val id = hash.digest(recipe.map { it.toByte() }.toByteArray()).take(16)
                .joinToString("") { "%02x".format(it.toInt() and 255) }
            return tv.plurx.app.data.QualityCandidate(id, recipe, "encode",
                if (height == 720) 1280 else 1920, height, height, peak_bps = raw,
                grade = "sdr", decoder_compatible = true, complete_cache = false, sustainable = true)
        }
        val raw = listOf(candidate(720, 6_160_000), candidate(1080, 12_160_000))
        val family = buildJsonObject {
            put("video", JsonArray(raw.mapIndexed { index, row -> buildJsonObject {
                put("candidate_id", row.id); put("peak_bps", if (index == 0) 17_600_000 else 33_600_000)
            } }))
            put("audio", buildJsonObject { put("peak_bps", 199_734) })
        }
        val bound = continuousQualityBoundCatalog(raw, family)
        assertEquals(17_799_734L, bound[0].peak_bps)
        assertEquals(33_799_734L, bound[1].peak_bps)
        assertTrue(bound.all { it.hasValidIdentity })
        assertEquals(raw[0], bound[0].copy(peak_bps = raw[0].peak_bps))
        assertEquals(6_160_000L, raw[0].peak_bps)
        assertEquals(raw[0].id, autoRecoveryCandidate(raw, raw[1], emptySet(), 10_000_000.0)?.id)
        assertNull(autoRecoveryCandidate(bound, bound[1], emptySet(), 10_000_000.0))
        assertEquals(raw[0].id, autoRecoveryCandidate(bound, bound[1], emptySet(), 20_000_000.0)?.id)
        assertSame(raw, continuousQualityBoundCatalog(raw, null))
        val unrelated = candidate(144, 1_000_000)
        assertSame(unrelated, continuousQualityBoundCatalog(listOf(unrelated), family).single())
    }

}
