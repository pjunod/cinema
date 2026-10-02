package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.player.ContinuousQualityFixtures.identity
import tv.plurx.app.player.ContinuousQualityFixtures.transaction
import tv.plurx.app.player.ContinuousQualityFixtures.nextTransaction
import tv.plurx.app.player.ContinuousQualityFixtures.operation
import tv.plurx.app.player.ContinuousQualityFixtures.reply

internal object ContinuousQualityFixtures {
    private val generation = "11111111-1111-4111-8111-111111111111"
    val transaction = "22222222-2222-4222-8222-222222222222"
    val nextTransaction = "33333333-3333-4333-8333-333333333333"
    val identity = buildJsonObject {
        put("version", 1); put("generation", generation); put("control_epoch", 1)
        put("attachment", buildJsonObject {
            put("client_instance_id", generation); put("attachment_id", transaction)
            put("lifetime_id", "test-owned-playback"); put("family_id", "a".repeat(64))
        })
    }
    fun operation(revision: Long = 1) = buildJsonObject {
        put("kind", "prepare"); put("intent_revision", revision); put("target_rendition_id", "b".repeat(64))
    }
    fun reply(request: JsonObject): JsonObject {
        val transition = request.obj("transition")
        val sequence = transition?.number("sequence") ?: 0
        val op = transition?.obj("operation")
        val tx = buildJsonObject {
            put("transaction_id", transition?.text("transaction_id") ?: transaction)
            put("intent_revision", op?.number("intent_revision") ?: 1)
            put("target_rendition_id", op?.get("target_rendition_id") ?: JsonPrimitive("b".repeat(64)))
            put("state", "ready"); put("intent_superseded", false); put("cancel_requested", false); put("ever_appended", false)
            for (key in listOf("ready", "reserved", "appended", "disposed")) put(key, JsonArray(emptyList()))
        }
        return buildJsonObject {
            identity.forEach { (key, value) -> put(key, value) }; put("revision", sequence + 1)
            put("ledger", buildJsonObject {
                identity.forEach { (key, value) -> put(key, value) }
                put("accepted_sequence", sequence); put("latest_intent_revision", op?.number("intent_revision") ?: 1)
                put("transactions", JsonArray(listOf(tx)))
            })
            if (transition != null) put("receipt", buildJsonObject {
                identity.forEach { (key, value) -> put(key, value) }
                put("accepted_sequence", sequence); put("transaction", tx)
            })
        }
    }

}

class ContinuousQualityProtocolTest {
    @Test fun lostAcknowledgementReplaysTheSameRequestBeforeNewIntent() = runBlocking {
        val requests = mutableListOf<JsonObject>()
        val protocol = ContinuousQualityProtocol(identity, { request ->
            requests += request
            if (requests.size <= 2) throw IOException("lost acknowledgement")
            reply(request)
        })
        assertTrue(runCatching { protocol.transition(transaction, operation()) }.isFailure)
        protocol.transition(nextTransaction, operation(2))
        assertEquals(listOf(1L, 1L, 1L, 2L), requests.map { it.obj("transition")?.number("sequence") })
        assertEquals(requests[0], requests[1]); assertEquals(requests[0], requests[2])
        assertEquals(nextTransaction, requests.last().obj("transition")?.text("transaction_id"))
    }

    @Test fun cancellationRetainsUncertainOrderingAndRateLimitRetriesAreDelayed() = runBlocking {
        val entered = CompletableDeferred<Unit>()
        val blocked = CompletableDeferred<Unit>()
        val requests = mutableListOf<JsonObject>()
        val waits = mutableListOf<Long>()
        var first = true
        val protocol = ContinuousQualityProtocol(identity, { request ->
            requests += request
            if (first) { first = false; entered.complete(Unit); blocked.await() }
            if (requests.size == 2) throw ContinuousQualityHttpFailure(429, 5000)
            reply(request)
        }, { waits += it })
        val cancelled = async(start = CoroutineStart.UNDISPATCHED) { protocol.transition(transaction, operation()) }
        entered.await(); cancelled.cancelAndJoin()
        protocol.transition(nextTransaction, operation(2))
        assertEquals(listOf(1L, 1L, 1L, 2L), requests.map { it.obj("transition")?.number("sequence") })
        assertEquals(listOf(1000L), waits)
        assertEquals(requests[0], requests[2])
    }

    @Test fun malformedReceiptsAndForeignAttachmentsCannotAdvanceTheLedger() = runBlocking {
        val requests = mutableListOf<JsonObject>()
        val protocol = ContinuousQualityProtocol(identity, { request ->
            requests += request
            val valid = reply(request)
            if (requests.size <= 2) JsonObject(valid + ("attachment" to JsonObject(requireNotNull(valid.obj("attachment")) +
                ("attachment_id" to JsonPrimitive(nextTransaction))))) else valid
        })
        assertTrue(runCatching { protocol.transition(transaction, operation()) }.isFailure)
        assertNull(protocol.ledger)
        protocol.transition(nextTransaction, operation(2))
        assertEquals(listOf(1L, 1L, 1L, 2L), requests.map { it.obj("transition")?.number("sequence") })
        val request = requests.last(); val valid = reply(request)
        val tx = requireNotNull(valid.obj("receipt")?.obj("transaction"))
        val badReceipt = JsonObject(requireNotNull(valid.obj("receipt")) + ("transaction" to JsonObject(tx +
            ("ever_appended" to JsonPrimitive("false")))))
        assertFalse(ContinuousQualityWire.response(JsonObject(valid + ("receipt" to badReceipt)), request))
        assertFalse(ContinuousQualityWire.response(JsonObject(valid + ("revision" to JsonPrimitive("2"))), request))
        assertFalse(ContinuousQualityWire.response(JsonObject(valid + ("revision" to JsonPrimitive(Long.MAX_VALUE))), request))
    }

    @Test fun terminalReconciliationRequiresDurableEndProof() = runBlocking {
        var terminal = false
        val protocol = ContinuousQualityProtocol(identity, { request ->
            JsonObject(reply(request) + ("terminal" to JsonPrimitive(terminal)))
        })
        assertTrue(runCatching { protocol.reconcileTerminal() }.isFailure)
        assertNull(protocol.ledger)
        terminal = true
        assertTrue(protocol.reconcileTerminal()["terminal"]?.wireBoolean() == true)
    }

    @Test fun familyBindingRejectsDuplicateRastersClocksAndForeignPlaylists() {
        fun video(height: Int, index: Int) = buildJsonObject {
            put("candidate_id", index.toString().repeat(32)); put("rendition_id", index.toString().repeat(64))
            put("init_id", "c".repeat(64)); put("codec", "avc1.640032"); put("width", if (height == 720) 1280 else 1920)
            put("height", height); put("timescale", 24); put("frame_ticks", 1); put("segment_ticks", 48)
            put("peak_bps", 1_000_000); put("playlist", "video/${index.toString().repeat(64)}/index.m3u8")
        }
        val first = video(720, 1); val second = video(1080, 2)
        fun family(rows: List<JsonObject>) = buildJsonObject {
            put("version", 1); put("mode", "controlled"); put("family_id", "a".repeat(64)); put("master", "master.m3u8")
            put("video", JsonArray(rows)); put("audio", JsonNull)
        }
        assertTrue(ContinuousQualityWire.family(family(listOf(first, second))))
        assertFalse(ContinuousQualityWire.family(family(listOf(first, first))))
        assertFalse(ContinuousQualityWire.family(family(listOf(first, JsonObject(second + ("timescale" to JsonPrimitive(48)))))))
        assertFalse(ContinuousQualityWire.family(family(listOf(first, JsonObject(second + ("playlist" to JsonPrimitive("https://foreign.invalid/video.m3u8")))))))
    }
    @Test fun reservationCapsApplyAcrossTransactionsAndAppendsNeedTheirExactPin() {
        val request = JsonObject(identity + ("transition" to buildJsonObject {
            identity.forEach { (key, value) -> put(key, value) }
            put("sequence", 1); put("transaction_id", transaction); put("operation", operation())
        }))
        val valid = reply(request)
        val original = requireNotNull(valid.obj("ledger")?.get("transactions") as? JsonArray).first().jsonObject
        fun pins(count: Int, start: Int) = JsonArray((start until start + count).map { index -> buildJsonObject {
            put("artifact_id", index.toString(16).padStart(64, '0')); put("rendition_id", "b".repeat(64))
            put("timescale", 24); put("from_tick", index * 48); put("through_tick", (index + 1) * 48); put("byte_length", 1)
        } })
        fun response(total: Int): JsonObject {
            val first = JsonObject(original + ("reserved" to pins(64, 1)))
            val second = JsonObject(original + mapOf("transaction_id" to JsonPrimitive(nextTransaction),
                "reserved" to pins(total - 64, 65)))
            return JsonObject(valid + ("ledger" to JsonObject(requireNotNull(valid.obj("ledger")) +
                ("transactions" to JsonArray(listOf(first, second))))))
        }
        assertTrue(ContinuousQualityWire.response(response(128), request))
        assertFalse(ContinuousQualityWire.response(response(129), request))
        val unreservedAppend = JsonObject(original + mapOf("appended" to pins(1, 1), "ever_appended" to JsonPrimitive(true)))
        val badLedger = JsonObject(requireNotNull(valid.obj("ledger")) + ("transactions" to JsonArray(listOf(unreservedAppend))))
        assertFalse(ContinuousQualityWire.response(JsonObject(valid + ("ledger" to badLedger)), request))
    }

    @Test fun receiptIdentityAloneCannotAcknowledgeUnperformedOperations() {
        val pin = buildJsonObject {
            put("artifact_id", "c".repeat(64)); put("rendition_id", "b".repeat(64)); put("timescale", 24)
            put("from_tick", 0); put("through_tick", 48); put("byte_length", 1000)
        }
        fun request(kind: String) = JsonObject(identity + ("transition" to buildJsonObject {
            identity.forEach { (key, value) -> put(key, value) }
            put("sequence", 1); put("transaction_id", transaction)
            put("operation", buildJsonObject {
                put("kind", kind)
                when (kind) {
                    "scheduled", "appended" -> put("intervals", JsonArray(listOf(pin)))
                    "cancel_unappended" -> put("completed", JsonArray(emptyList()))
                    "disposed" -> put("artifacts", JsonArray(listOf(pin.getValue("artifact_id"))))
                    "presented" -> { put("artifact_id", pin.getValue("artifact_id")); put("film_tick", 0); put("observed_at_ms", 1) }
                }
            })
        }))
        fun receipt(request: JsonObject, fields: Map<String, JsonElement>): JsonObject {
            val original = reply(request)
            val tx = JsonObject(requireNotNull(original.obj("receipt")?.obj("transaction")) + fields)
            return JsonObject(original + mapOf(
                "receipt" to JsonObject(requireNotNull(original.obj("receipt")) + ("transaction" to tx)),
                "ledger" to JsonObject(requireNotNull(original.obj("ledger")) + ("transactions" to JsonArray(listOf(tx))))))
        }
        for (kind in listOf("scheduled", "appended", "presented", "cancel_unappended", "disposed")) {
            val request = request(kind)
            assertFalse(kind, ContinuousQualityWire.response(reply(request), request))
        }
        val scheduled = request("scheduled")
        assertTrue(ContinuousQualityWire.response(receipt(scheduled, mapOf("reserved" to JsonArray(listOf(pin)))), scheduled))
        val appended = request("appended")
        val committed = mapOf("reserved" to JsonArray(listOf(pin)), "appended" to JsonArray(listOf(pin)), "ever_appended" to JsonPrimitive(true))
        assertTrue(ContinuousQualityWire.response(receipt(appended, committed), appended))
        val presented = request("presented")
        assertTrue(ContinuousQualityWire.response(receipt(presented, committed + mapOf(
            "first_presented_tick" to JsonPrimitive(0), "first_presented_at_ms" to JsonPrimitive(1))), presented))
        val disposed = request("disposed")
        assertFalse(ContinuousQualityWire.response(receipt(disposed, mapOf("disposed" to JsonArray(listOf(pin.getValue("artifact_id"))),
            "reserved" to JsonArray(listOf(pin)))), disposed))
        assertTrue(ContinuousQualityWire.response(receipt(disposed, mapOf("disposed" to JsonArray(listOf(pin.getValue("artifact_id"))))), disposed))
    }

}
