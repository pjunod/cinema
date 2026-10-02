package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*

/** One serialized demand owner for video and the shared AAC projection. It
 * schedules accepted immutable intervals before publishing future selection.
 * Queue and renderer acknowledgements are deliberately supplied separately. */
internal class ContinuousReservations(
    family: JsonObject,
    private val protocol: ContinuousQualityProtocol,
    private val selection: ContinuousVideoSelection,
    initialRendition: String,
    private val cancelUnexposed: (String, JsonObject) -> Boolean = { _, _ -> false },
) {
    private val family = Json.parseToJsonElement(family.toString()).jsonObject.also { require(ContinuousQualityWire.family(it)) }
    private val video = this.family.getValue("video").jsonArray.map { it.jsonObject }
    private val lock = Mutex()
    private var wanted = video.single { it.text("rendition_id") == initialRendition }
    private var transaction: String? = null
    private var automatic = false
    data class Retained(val failedRow: JsonObject, val request: Long)
    private data class Previous(val row: JsonObject, val automatic: Boolean, val request: Long)
    private var optionalPrevious: Previous? = null
    private data class FailedChange(val row: JsonObject, val previous: Previous, val through: Long,
        val transaction: String?, val wasPresented: Boolean)
    private var failedChange: FailedChange? = null

    suspend fun initial(through: Long, automatic: Boolean) = lock.withLock {
        this.automatic = automatic
        reserveWindow(through)
    }

    /** The caller must establish actual supported formats before a new target
     * is prepared. This retains the previous choice when that proof is absent. */
    suspend fun change(rendition: String, through: Long, automatic: Boolean, supported: Set<String>, request: Long = 0): Boolean = lock.withLock {
        val next = video.singleOrNull { it.text("rendition_id") == rendition } ?: return@withLock false
        if (rendition !in supported) return@withLock false
        val previous = wanted
        val previousAutomatic = this.automatic
        val wasPresented = protocol.ledger?.get("transactions")?.jsonArray.orEmpty().any {
            it.jsonObject.text("target_rendition_id") == previous.text("rendition_id") &&
                it.jsonObject.number("first_presented_tick") != null
        }
        optionalPrevious = null
        failedChange = null
        transaction = null
        try {
            prepare(next, through)
            wanted = next
            this.automatic = automatic
            reserveWindow(through)
            if (next != previous && wasPresented) optionalPrevious = Previous(previous, previousAutomatic, request)
            true
        } catch (error: Exception) {
            failedChange = FailedChange(next, Previous(previous, previousAutomatic, request), through, transaction, wasPresented)
            wanted = previous
            this.automatic = previousAutomatic
            transaction = null
            // The attachment pump owns restoration after caller cancellation;
            // it must replay the uncertain request before a higher intent.
            throw error
        }
    }

    suspend fun recoverFailedChange(): Retained? = lock.withLock {
        val failed = failedChange ?: return@withLock null
        protocol.settlePending()
        protocol.snapshot()
        val tx = protocol.ledger?.get("transactions")?.jsonArray.orEmpty().map { it.jsonObject }
            .singleOrNull { it.text("transaction_id") == failed.transaction }
        val proof = tx ?: buildJsonObject {
            put("target_rendition_id", failed.row.getValue("rendition_id")); put("reserved", JsonArray(emptyList()))
        }
        val unexposed = failed.wasPresented && (tx == null || tx["ever_appended"]?.wireBoolean() == false &&
            tx.getValue("appended").jsonArray.isEmpty()) && cancelUnexposed(failed.transaction.orEmpty(), proof)
        if (unexposed && tx != null && tx["cancel_requested"]?.wireBoolean() != true) protocol.transition(requireNotNull(failed.transaction),
            buildJsonObject { put("kind", "cancel_unappended"); put("completed", JsonArray(emptyList())) })
        // A restorative prepare may have been acknowledged only on replay.
        // Reuse that live owner rather than allocating another intent on each
        // retry, or when the incumbent loader already restored the same choice.
        val existing = protocol.ledger?.get("transactions")?.jsonArray.orEmpty().map { it.jsonObject }
            .singleOrNull { it.text("transaction_id") == transaction &&
                it["intent_revision"] == protocol.ledger?.get("latest_intent_revision") &&
                it["target_rendition_id"] == failed.previous.row["rendition_id"] &&
                it["intent_superseded"]?.wireBoolean() == false && it["cancel_requested"]?.wireBoolean() == false &&
                it.getValue("ready").jsonArray.isNotEmpty() }
        if (existing == null) prepare(failed.previous.row, failed.through)
        reserveWindow(failed.through)
        failedChange = null
        if (unexposed) Retained(failed.row, failed.previous.request) else null
    }

    suspend fun reserve(resource: ContinuousQualityMedia.Resource, bytes: ByteArray?) = lock.withLock {
        if (resource.initialization) return@withLock
        if (resource.role == "video") {
            val through = resource.videoFrontier()
            if (hasVideoPin(resource.rendition, through)) return@withLock
            if (resource.rendition != wanted.text("rendition_id")) throw ContinuousStaleVideoLoad()
            reserveWindow(through)
        } else {
            val payload = bytes ?: throw IOException("Continuous audio clock bytes missing")
            val artifact = ContinuousQualityMedia.digest(payload)
            val audio = (protocol.ledger?.get("shared_audio_reserved") as? JsonArray).orEmpty()
            if (audio.any { it.jsonObject.text("artifact_id") == artifact && it.jsonObject.text("rendition_id") == resource.rendition }) return@withLock
            val entry = ContinuousFragmentClock.videoEntry(ContinuousFragmentClock.firstDecodeTick(payload),
                requireNotNull(resource.row.number("timescale")), requireNotNull(wanted.number("timescale")),
                requireNotNull(wanted.number("segment_ticks")))
            // Video ready may already cover this entry while its AAC pin was
            // disposed. Renew the shared projection even in that case.
            if (transaction == null) prepare(wanted, entry)
            protocol.window(requireNotNull(transaction), frontier(wanted, entry))
            reserveWindow(entry)
        }
    }

    /** Restores only a previously presented choice with zero target exposure.
     * A partial extraction or uncertain committed append cannot use this path. */
    suspend fun retainUnexposed(resource: ContinuousQualityMedia.Resource): Retained? = lock.withLock {
        val previous = optionalPrevious ?: return@withLock null
        if (resource.initialization || resource.role != "video" || resource.rendition != wanted.text("rendition_id")) return@withLock null
        protocol.settlePending()
        val tx = current()
        if (tx["ever_appended"]?.wireBoolean() != false || tx.getValue("appended").jsonArray.isNotEmpty()) return@withLock null
        val id = requireNotNull(transaction)
        if (!cancelUnexposed(id, tx)) return@withLock null
        val failedRow = wanted
        protocol.transition(id, buildJsonObject { put("kind", "cancel_unappended"); put("completed", JsonArray(emptyList())) })
        // Once cancelled, future loads must never republish that target, even
        // if the restorative control request loses its acknowledgment.
        optionalPrevious = null
        wanted = previous.row
        automatic = previous.automatic
        transaction = null
        prepare(wanted, resource.videoFrontier())
        reserveWindow(resource.videoFrontier())
        Retained(failedRow, previous.request)
    }

    private suspend fun prepare(row: JsonObject, through: Long): JsonObject {
        require(through in 0..ContinuousQualityWire.MAX_SAFE_INTEGER)
        // Refresh server ordering before allocating intent after a restored or
        // uncertain request; the protocol serializes and replays pending work.
        protocol.snapshot()
        val revision = requireNotNull(protocol.ledger?.number("latest_intent_revision"))
        if (revision == ContinuousQualityWire.MAX_SAFE_INTEGER) throw IOException("Continuous intent revision bound")
        val id = ContinuousQualityWire.newId()
        transaction = id
        protocol.transition(id, buildJsonObject {
            put("kind", "prepare"); put("intent_revision", revision + 1); put("target_rendition_id", requireNotNull(row.text("rendition_id")))
        }, frontier(row, through))
        return current().also { requireLive(it) }
    }

    private suspend fun reserveWindow(through: Long) {
        if (transaction == null) prepare(wanted, through)
        var ready = current().also { requireLive(it) }
        if (ready.getValue("ready").jsonArray.none { contains(it.jsonObject, through) }) {
            protocol.window(requireNotNull(transaction), frontier(wanted, through))
            ready = current().also { requireLive(it) }
        }
        if (ready.getValue("ready").jsonArray.any { it.jsonObject["artifact_id"] in ready.getValue("disposed").jsonArray }) {
            ready = prepare(wanted, through)
        }
        val reservations = ready.getValue("reserved").jsonArray
        val missing = ready.getValue("ready").jsonArray.map { it.jsonObject }.filter { pin ->
            reservations.none { sameInterval(it.jsonObject, pin) }
        }.map { pin -> JsonObject(pin.filterKeys { it in INTERVAL_FIELDS }) }
        if (missing.isNotEmpty()) protocol.transition(requireNotNull(transaction), buildJsonObject {
            put("kind", "scheduled"); put("intervals", JsonArray(missing))
        })
        if (!hasVideoPin(requireNotNull(wanted.text("rendition_id")), through) || !selection.publishReserved(through, automatic)) {
            throw IOException("Continuous target has no reserved selection")
        }
    }

    private fun current(): JsonObject = (protocol.ledger?.get("transactions") as? JsonArray)?.map { it.jsonObject }
        ?.singleOrNull { it.text("transaction_id") == transaction } ?: throw IOException("Continuous transaction missing")
    private fun requireLive(tx: JsonObject) {
        if (tx["intent_superseded"]?.wireBoolean() != false || tx["cancel_requested"]?.wireBoolean() != false ||
            tx.getValue("ready").jsonArray.isEmpty()) throw IOException("Continuous target retained current")
    }
    private fun hasVideoPin(rendition: String, through: Long): Boolean = (protocol.ledger?.get("transactions") as? JsonArray).orEmpty().any { item ->
        val tx = item.jsonObject
        tx.getValue("reserved").jsonArray.any { pin -> pin.jsonObject.text("rendition_id") == rendition &&
            pin.jsonObject["artifact_id"] !in tx.getValue("disposed").jsonArray && contains(pin.jsonObject, through) }
    }
    private fun contains(pin: JsonObject, tick: Long) = requireNotNull(pin.number("from_tick")) <= tick && tick < requireNotNull(pin.number("through_tick"))
    private fun frontier(row: JsonObject, through: Long) = buildJsonObject { put("timescale", requireNotNull(row.number("timescale"))); put("through_tick", through) }
    private fun sameInterval(a: JsonObject, b: JsonObject) = INTERVAL_FIELDS.all { a[it] == b[it] }
    companion object { private val INTERVAL_FIELDS = setOf("artifact_id", "rendition_id", "timescale", "from_tick", "through_tick", "byte_length") }
}
