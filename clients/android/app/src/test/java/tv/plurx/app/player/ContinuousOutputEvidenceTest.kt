@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import org.junit.Assert.*
import org.junit.Test
import kotlinx.serialization.json.*

class ContinuousOutputEvidenceTest {
    @Test fun hardwareCallbacksUseTheirQueuedFormatAndRejectEarlierEpochs() {
        val owner = Any()
        val low = Format.Builder().setWidth(1280).setHeight(720).build()
        val high = Format.Builder().setWidth(1920).setHeight(1080).build()
        val frames = ContinuousCodecFrames()
        val epoch = frames.epoch
        frames.queued(1_000_000_000, 999_000_000, low, owner)
        frames.queued(1_000_041_667, 999_000_000, high, owner)
        assertEquals(low, frames.rendered(1_000_000_000, epoch)?.format)
        assertEquals(1_041_667L, frames.rendered(1_000_041_667, epoch)?.positionUs)
        assertNull(frames.rendered(1_000_041_667, epoch))
        frames.reset()
        frames.queued(1_000_000_000, 999_000_000, high, owner)
        assertNull(frames.rendered(1_000_000_000, epoch))
        assertEquals(high, frames.rendered(1_000_000_000, frames.epoch)?.format)
        frames.queued(2_000_000, 1_000_000, low, owner, 20_000_000)
        assertEquals(1_000_000L, frames.rendered(22_000_000, frames.epoch)?.positionUs)
        frames.queued(C.TIME_UNSET, 0, low, owner)
        assertNull(frames.rendered(C.TIME_UNSET, frames.epoch))
        repeat(513) { frames.queued(it.toLong(), 0, low, owner) }
        assertNull(frames.rendered(0, frames.epoch))
        assertNotNull(frames.rendered(512, frames.epoch))
    }
    @Test fun aFailedCodecFlushCannotCreditDecoderRelease() {
        val evidence = ContinuousOutputEvidence()
        val owner = Any()
        val events = mutableListOf<ContinuousOutputEvidence.Event>()
        evidence.subscribe(owner, events::add)
        var fail = true
        val delegate = java.lang.reflect.Proxy.newProxyInstance(
            androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java.classLoader,
            arrayOf(androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java),
        ) { _, method, _ ->
            if (method.name == "flush" && fail) throw IllegalStateException("fixture codec flush failure")
            null
        } as androidx.media3.exoplayer.mediacodec.MediaCodecAdapter
        val adapter = ContinuousCodecAdapterFactory.observe(delegate, video = true, audio = false, evidence)
        adapter.queueInputBuffer(0, 0, 1, 0, 0)
        assertThrows(IllegalStateException::class.java) { adapter.flush() }
        assertEquals(listOf(ContinuousOutputEvidence.Event.VideoOwned), events)
        fail = false
        adapter.flush()
        assertEquals(ContinuousOutputEvidence.Event.VideoFreed, events.last())
    }
    @Test fun aSuccessfulEmptyCodecFlushRetainsItsCreatingAttachmentOwner() {
        val evidence = ContinuousOutputEvidence()
        val owner = Any()
        val events = mutableListOf<ContinuousOutputEvidence.Event>()
        evidence.subscribe(owner, events::add)
        val delegate = java.lang.reflect.Proxy.newProxyInstance(
            androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java.classLoader,
            arrayOf(androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java),
        ) { _, _, _ -> null } as androidx.media3.exoplayer.mediacodec.MediaCodecAdapter
        val adapter = ContinuousCodecAdapterFactory.observe(delegate, video = true, audio = false, evidence)
        adapter.flush()
        assertEquals(listOf(ContinuousOutputEvidence.Event.VideoFreed), events)
        evidence.unsubscribe(owner)
        val next = Any()
        evidence.subscribe(next, events::add)
        adapter.release()
        assertEquals(1, events.size)
    }

    @Test fun callbacksCannotBorrowANewAttachmentSubscription() {
        val evidence = ContinuousOutputEvidence()
        val old = Any(); val next = Any()
        val events = mutableListOf<ContinuousOutputEvidence.Event>()
        evidence.subscribe(old, events::add)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(100), old)
        evidence.unsubscribe(old)
        evidence.subscribe(next, events::add)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(200), old)
        evidence.unsubscribe(old)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(300), next)
        assertEquals(listOf(ContinuousOutputEvidence.Event.AudioHead(100), ContinuousOutputEvidence.Event.AudioHead(300)), events)
    }
    private fun acceptedJournalFixture(): Triple<JsonObject, JsonObject, JsonObject> {
        val row = buildJsonObject {
            put("candidate_id", "a".repeat(32)); put("rendition_id", "b".repeat(64))
            put("width", 1280); put("height", 720); put("timescale", 24)
        }
        val family = buildJsonObject { put("family_id", "c".repeat(64)) }
        val transaction = buildJsonObject {
            put("transaction_id", "12345678-1234-1234-1234-123456789abc")
            put("intent_revision", 2); put("intent_superseded", false); put("state", "presented")
            put("target_rendition_id", "b".repeat(64)); put("first_presented_tick", 96)
            put("appended", buildJsonArray { add(buildJsonObject { put("artifact_id", "d".repeat(64)) }) })
        }
        return Triple(row, family, transaction)
    }

    @Test fun automaticJournalRejectsStaleUnacceptedAndSupersededFrames() {
        val (row, family, accepted) = acceptedJournalFixture()
        fun ledger(revision: Long = 2) = buildJsonObject {
            put("latest_intent_revision", revision)
            put("attachment", buildJsonObject { put("family_id", "c".repeat(64)) })
        }
        val frame = ContinuousOutputEvidence.Event.Frame(4_000_000,
            Format.Builder().setWidth(1280).setHeight(720).build(), 5_000)
        fun snapshot(tx: JsonObject = accepted, current: JsonObject? = ledger(), delivered: Long = 1,
                     observed: ContinuousOutputEvidence.Event.Frame = frame) =
            continuousAcceptedPresentation(row, family, current, tx, observed, "d".repeat(64), 96, delivered)
        val before = accepted.toString()
        assertNotNull(snapshot())
        assertNull(snapshot(current = null))
        assertNull(snapshot(current = ledger(3)))
        assertNull(snapshot(delivered = 2))
        assertNull(snapshot(tx = JsonObject(accepted + ("intent_superseded" to JsonPrimitive(true)))))
        assertNull(snapshot(tx = JsonObject(accepted - "first_presented_tick")))
        assertNull(snapshot(tx = JsonObject(accepted + ("first_presented_tick" to JsonPrimitive(95)))))
        assertNull(snapshot(tx = JsonObject(accepted + ("state" to JsonPrimitive("appended")))))
        assertNull(snapshot(tx = JsonObject(accepted + ("appended" to buildJsonArray {}))))
        assertNull(snapshot(observed = frame.copy(format = Format.Builder().setWidth(1920).setHeight(1080).build())))
        assertEquals(before, accepted.toString())
    }

    @Test fun automaticJournalKeepsOnlyExactAcceptedBindingAndDecoderGeometry() {
        val (row, family, tx) = acceptedJournalFixture()
        val ledger = buildJsonObject {
            put("latest_intent_revision", 2)
            put("attachment", buildJsonObject { put("family_id", "c".repeat(64)) })
        }
        val frame = ContinuousOutputEvidence.Event.Frame(4_000_000,
            Format.Builder().setWidth(1280).setHeight(720).build(), 5_000)
        val journal = requireNotNull(continuousAcceptedPresentation(row, family, ledger, tx, frame,
            "d".repeat(64), 96, 1))
        assertEquals(96L, journal.filmTick)
        assertEquals(24L, journal.timescale)
        assertEquals(1280, journal.width)
        assertEquals(720, journal.height)
        assertEquals(2L, journal.revision)
        assertEquals("d".repeat(64), journal.artifactId)
        assertTrue(journal.automaticDetail().contains("mode=auto route=continuous"))
        assertTrue(journal.automaticDetail().contains("intent_revision=2 latest_intent_revision=2"))
        assertNull(continuousAcceptedPresentation(row, family,
            JsonObject(ledger + ("attachment" to buildJsonObject { put("family_id", "e".repeat(64)) })),
            tx, frame, "d".repeat(64), 96, 1))
    }

}
