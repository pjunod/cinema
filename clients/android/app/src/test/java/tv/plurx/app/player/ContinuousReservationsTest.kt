package tv.plurx.app.player

import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.player.ContinuousQualityFixtures.identity

class ContinuousReservationsTest {
    @Test fun unsupportedTargetsDoNotPrepareAndColdVideoSchedulesBeforeSelection() = runBlocking {
        fun row(height: Int, id: String, candidate: String) = buildJsonObject {
            put("candidate_id", candidate.repeat(32)); put("rendition_id", id.repeat(64)); put("init_id", "d".repeat(64))
            put("codec", "avc1.640032"); put("width", if (height == 720) 1280 else 1920); put("height", height)
            put("timescale", 24); put("frame_ticks", 1); put("segment_ticks", 48); put("peak_bps", 1_000_000)
            put("playlist", "video/${id.repeat(64)}/index.m3u8")
        }
        val low = row(720, "b", "1"); val high = row(1080, "c", "2")
        val family = buildJsonObject {
            put("version", 1); put("mode", "controlled"); put("family_id", "a".repeat(64)); put("master", "master.m3u8")
            put("video", JsonArray(listOf(low, high)))
        }
        val requests = mutableListOf<JsonObject>()
        var sequence = 0L
        var revision = 0L
        var ledgerRevision = 0L
        var tx: JsonObject? = null
        var failedTargetCalls = 0
        var failedRestoreCalls = 0
        var blockOptional = false
        val optionalEntered = CompletableDeferred<Unit>()
        val optionalBlocked = CompletableDeferred<Unit>()
        val protocol = ContinuousQualityProtocol(identity, { request ->
            requests += request
            val transition = request.obj("transition")
            val operation = transition?.obj("operation")
            if (failedRestoreCalls > 0 && operation?.text("kind") == "prepare" && operation.text("target_rendition_id") == "c".repeat(64)) {
                failedRestoreCalls--
                throw java.io.IOException("lost restorative acknowledgement")
            }
            if (blockOptional && operation?.text("kind") == "prepare" && operation.text("target_rendition_id") == "b".repeat(64)) {
                blockOptional = false
                optionalEntered.complete(Unit)
                optionalBlocked.await()
            }
            if (failedTargetCalls > 0 && operation?.text("kind") == "prepare" && operation.text("target_rendition_id") == "b".repeat(64)) {
                failedTargetCalls--
                throw java.io.IOException("lost optional target acknowledgement")
            }
            if (operation?.text("kind") == "prepare") {
                revision = requireNotNull(operation.number("intent_revision"))
                val start = requireNotNull(request.obj("frontier")?.number("through_tick"))
                val pin = buildJsonObject {
                    put("artifact_id", "e".repeat(64)); put("rendition_id", operation.getValue("target_rendition_id"))
                    put("timescale", 24); put("from_tick", start); put("through_tick", start + 48); put("byte_length", 1000)
                }
                tx = buildJsonObject {
                    put("transaction_id", transition.getValue("transaction_id")); put("intent_revision", revision)
                    put("target_rendition_id", operation.getValue("target_rendition_id")); put("state", "ready")
                    put("intent_superseded", false); put("cancel_requested", false); put("ever_appended", false)
                    put("ready", JsonArray(listOf(pin))); put("reserved", JsonArray(emptyList()))
                    put("appended", JsonArray(emptyList())); put("disposed", JsonArray(emptyList()))
                }
            }
            if (operation?.text("kind") == "cancel_unappended") tx = JsonObject(requireNotNull(tx) + mapOf(
                "cancel_requested" to JsonPrimitive(true), "state" to JsonPrimitive("cancelling")))
            if (operation?.text("kind") == "scheduled") tx = JsonObject(requireNotNull(tx) + mapOf(
                "state" to JsonPrimitive("scheduled"), "reserved" to operation.getValue("intervals")))
            if (transition != null) sequence = requireNotNull(transition.number("sequence"))
            val ledger = JsonObject(identity + mapOf("accepted_sequence" to JsonPrimitive(sequence),
                "latest_intent_revision" to JsonPrimitive(revision), "transactions" to JsonArray(listOfNotNull(tx))))
            buildJsonObject {
                identity.forEach { (key, value) -> put(key, value) }; put("revision", ++ledgerRevision); put("ledger", ledger)
                if (transition != null) put("receipt", JsonObject(identity + mapOf(
                    "accepted_sequence" to JsonPrimitive(sequence), "transaction" to requireNotNull(tx))))
            }
        })
        val selection = ContinuousVideoSelection(family, protocol)
        val reservations = ContinuousReservations(family, protocol, selection, "b".repeat(64)) { _, _ -> true }
        assertFalse(reservations.change("c".repeat(64), 0, false, setOf("b".repeat(64))))
        assertTrue(requests.isEmpty())
        reservations.initial(0, false)
        assertEquals(listOf(null, "prepare", "scheduled"), requests.map { it.obj("transition")?.obj("operation")?.text("kind") })
        val before = requests.size
        reservations.reserve(ContinuousQualityMedia.Resource("video", low, false, 0), null)
        assertEquals(before, requests.size)
        // Only an actually presented incumbent can be restored on a cold
        // optional load failure. Refresh its durable presentation receipt.
        tx = JsonObject(requireNotNull(tx) + mapOf("first_presented_tick" to JsonPrimitive(0),
            "first_presented_at_ms" to JsonPrimitive(1), "ever_appended" to JsonPrimitive(true),
            "appended" to requireNotNull(tx).getValue("reserved"), "state" to JsonPrimitive("presented")))
        protocol.snapshot()
        assertTrue(reservations.change("c".repeat(64), 48, true, setOf("b".repeat(64), "c".repeat(64))))
        assertEquals("c".repeat(64), protocol.ledger?.get("transactions")?.jsonArray?.single()?.jsonObject?.text("target_rendition_id"))
        assertEquals(2L, protocol.ledger?.number("latest_intent_revision"))
        val retentionStart = requests.size
        val retained = reservations.retainUnexposed(ContinuousQualityMedia.Resource("video", high, false, 1))
        assertEquals(high, retained?.failedRow)
        assertEquals("b".repeat(64), protocol.ledger?.get("transactions")?.jsonArray?.single()?.jsonObject?.text("target_rendition_id"))
        assertEquals(listOf("cancel_unappended", "prepare", "scheduled"), requests.drop(retentionStart).mapNotNull {
            it.obj("transition")?.obj("operation")?.text("kind")
        })
        assertTrue(reservations.change("c".repeat(64), 48, true, setOf("b".repeat(64), "c".repeat(64))))
        failedTargetCalls = 2
        assertTrue(runCatching { reservations.change("b".repeat(64), 96, false, setOf("b".repeat(64), "c".repeat(64))) }.isFailure)
        failedRestoreCalls = 2
        assertTrue(runCatching { reservations.recoverFailedChange() }.isFailure)
        reservations.recoverFailedChange()
        // Replay the uncertain low target first, then restore high using a
        // newer intent; its actual scheduled choice remains usable.
        assertEquals(6L, protocol.ledger?.number("latest_intent_revision"))
        assertEquals("c".repeat(64), protocol.ledger?.get("transactions")?.jsonArray?.single()?.jsonObject?.text("target_rendition_id"))
        assertTrue(selection.publishReserved(96, true))
        assertTrue(runCatching { reservations.reserve(ContinuousQualityMedia.Resource("video", low, false, 2), null) }.isFailure)
        tx = JsonObject(requireNotNull(tx) + mapOf("first_presented_tick" to JsonPrimitive(96),
            "first_presented_at_ms" to JsonPrimitive(1), "ever_appended" to JsonPrimitive(true),
            "appended" to requireNotNull(tx).getValue("reserved"), "state" to JsonPrimitive("presented")))
        protocol.snapshot()
        blockOptional = true
        val cancelled = launch(start = CoroutineStart.UNDISPATCHED) {
            reservations.change("b".repeat(64), 144, false, setOf("b".repeat(64), "c".repeat(64)), 42)
        }
        optionalEntered.await()
        cancelled.cancelAndJoin()
        val cancellationRecovery = requests.size
        val recovered = reservations.recoverFailedChange()
        assertEquals(low, recovered?.failedRow)
        assertEquals(42L, recovered?.request)
        assertEquals(listOf("prepare", "cancel_unappended", "prepare", "scheduled"), requests.drop(cancellationRecovery).mapNotNull {
            it.obj("transition")?.obj("operation")?.text("kind")
        })
        assertEquals(8L, protocol.ledger?.number("latest_intent_revision"))
        assertEquals("c".repeat(64), protocol.ledger?.get("transactions")?.jsonArray?.single()?.jsonObject?.text("target_rendition_id"))

    }
}
