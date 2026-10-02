package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.CancellationException
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
) {
    private val family = Json.parseToJsonElement(family.toString()).jsonObject.also { require(ContinuousQualityWire.family(it)) }
    private val video = this.family.getValue("video").jsonArray.map { it.jsonObject }
    private val lock = Mutex()
    private var wanted = video.single { it.text("rendition_id") == initialRendition }
    private var transaction: String? = null
    private var automatic = false

    suspend fun initial(through: Long, automatic: Boolean) = lock.withLock {
        this.automatic = automatic
        reserveWindow(through)
    }

    /** The caller must establish actual supported formats before a new target
     * is prepared. This retains the previous choice when that proof is absent. */
    suspend fun change(rendition: String, through: Long, automatic: Boolean, supported: Set<String>): Boolean = lock.withLock {
        val next = video.singleOrNull { it.text("rendition_id") == rendition } ?: return@withLock false
        if (rendition !in supported) return@withLock false
        val previous = wanted
        val previousAutomatic = this.automatic
        try {
            prepare(next, through)
            wanted = next
            this.automatic = automatic
            reserveWindow(through)
            true
        } catch (error: Exception) {
            wanted = previous
            this.automatic = previousAutomatic
            // A lost acknowledgement remains pending. Replaying it before the
            // restorative prepare fences any late optional target admission.
            transaction = null
            if (error !is CancellationException) {
                try { prepare(previous, through); reserveWindow(through) }
                catch (restoration: Exception) {
                    if (restoration is CancellationException) throw restoration
                    error.addSuppressed(restoration)
                }
            }
            throw error
        }
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

    private suspend fun prepare(row: JsonObject, through: Long): JsonObject {
        require(through in 0..ContinuousQualityWire.MAX_SAFE_INTEGER)
        // Refresh server ordering before allocating intent after a restored or
        // uncertain request; the protocol serializes and replays pending work.
        protocol.snapshot()
        val revision = requireNotNull(protocol.ledger?.number("latest_intent_revision"))
        if (revision == ContinuousQualityWire.MAX_SAFE_INTEGER) throw IOException("Continuous intent revision bound")
        val id = ContinuousQualityWire.newId()
        protocol.transition(id, buildJsonObject {
            put("kind", "prepare"); put("intent_revision", revision + 1); put("target_rendition_id", requireNotNull(row.text("rendition_id")))
        }, frontier(row, through))
        transaction = id
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
