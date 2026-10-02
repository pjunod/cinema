package tv.plurx.app.player

import java.io.IOException
import java.util.UUID
import java.util.concurrent.atomic.AtomicInteger
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*

/** Attachment-owned exchanges. An uncertain acknowledgement is replayed before
 * a newer command can enter the server's durable ordering. Media ownership is
 * supplied by the adapter, never inferred by this transport. */
internal class ContinuousQualityProtocol(
    identity: JsonObject,
    private val exchange: suspend (JsonObject) -> JsonObject,
    private val wait: suspend (Long) -> Unit = { delay(it) },
) {
    private val identity = Json.parseToJsonElement(identity.toString()).jsonObject
    private val lock = Mutex()
    private val queued = AtomicInteger()
    private var sequence = 0L
    private var revision = 0L
    private var pending: JsonObject? = null
    @Volatile var ledger: JsonObject? = null
        private set

    init { require(ContinuousQualityWire.identity(this.identity)) }

    suspend fun snapshot(): JsonObject = ordered {
        replayPending()
        send(identity + mapOf("transition" to JsonNull, "frontier" to JsonNull))
    }

    suspend fun transition(transaction: String, operation: JsonObject, frontier: JsonObject? = null): JsonObject {
        require(ContinuousQualityWire.uuid(transaction))
        // JsonObject can wrap a caller-owned mutable map; retain wire bytes.
        val retained = Json.parseToJsonElement(operation.toString()).jsonObject
        val retainedFrontier = frontier?.let { Json.parseToJsonElement(it.toString()).jsonObject }
        return ordered {
            replayPending()
            check(sequence < ContinuousQualityWire.MAX_SAFE_INTEGER)
            val transition = buildJsonObject {
                identity.forEach { (key, value) -> put(key, value) }
                put("sequence", sequence + 1)
                put("transaction_id", transaction)
                put("operation", retained)
            }
            send(identity + mapOf("transition" to transition, "frontier" to (retainedFrontier ?: JsonNull)))
        }
    }

    suspend fun window(transaction: String, frontier: JsonObject): JsonObject {
        require(ContinuousQualityWire.uuid(transaction))
        val retained = Json.parseToJsonElement(frontier.toString()).jsonObject
        return ordered {
            replayPending()
            send(identity + mapOf("transition" to JsonNull, "frontier" to JsonNull,
                "window" to buildJsonObject { put("transaction_id", transaction); put("frontier", retained) }))
        }
    }

    /** Durable End excludes late scheduling. It is required before an adapter
     * may reconcile an uncertain operation with actual pipeline disposal. */
    suspend fun reconcileTerminal(): JsonObject = ordered {
        val request = JsonObject(identity + mapOf("transition" to JsonNull, "frontier" to JsonNull))
        val response = exchange(request)
        check(response["terminal"]?.wireBoolean() == true)
        accept(response, request)
        sequence = maxOf(sequence, pending?.obj("transition")?.number("sequence") ?: 0)
        pending = null
        response
    }

    suspend fun settlePending() = ordered { replayPending() }

    private suspend fun replayPending() { pending?.let { send(it) } }

    private suspend fun send(fields: Map<String, JsonElement>): JsonObject {
        val request = JsonObject(fields)
        pending = request
        var failure: Exception? = null
        repeat(2) { attempt ->
            try {
                val response = exchange(request)
                accept(response, request)
                pending = null
                return response
            } catch (error: kotlinx.coroutines.CancellationException) {
                throw error // The uncertain request remains replayable.
            } catch (error: Exception) {
                failure = error
                if (error is ContinuousQualityHttpFailure) {
                    if (error.status in 400..499 && error.status != 429) throw error
                    if (error.status == 429 && attempt == 0) wait(error.retryAfterMs.coerceIn(1, 1000))
                }
            }
        }
        throw requireNotNull(failure)
    }

    private fun accept(response: JsonObject, request: JsonObject) {
        check(ContinuousQualityWire.response(response, request)) { "Continuous quality receipt identity or bounds" }
        val nextRevision = requireNotNull(response.number("revision"))
        check(nextRevision >= revision) { "Continuous quality revision moved backwards" }
        val nextLedger = requireNotNull(response.obj("ledger"))
        val accepted = requireNotNull(nextLedger.number("accepted_sequence"))
        check(accepted >= sequence) { "Continuous quality sequence moved backwards" }
        revision = nextRevision
        sequence = accepted
        ledger = nextLedger
    }

    private suspend fun <T> ordered(action: suspend () -> T): T {
        if (queued.incrementAndGet() > 32) {
            queued.decrementAndGet()
            throw IOException("Continuous quality exchange queue bound")
        }
        try { return lock.withLock { action() } } finally { queued.decrementAndGet() }
    }
}

internal class ContinuousQualityHttpFailure(val status: Int, val retryAfterMs: Long = 1000) :
    IOException("Continuous quality HTTP $status")

/** The same version-one bounds as the served web client. Unrecognized fields
 * are allowed; known fields and the identity of every receipt are mandatory. */
internal object ContinuousQualityWire {
    const val MAX_SAFE_INTEGER = 9_007_199_254_740_991L
    private val digest = Regex("[0-9a-f]{64}")
    private val candidate = Regex("[0-9a-f]{32}")
    private val uuidPattern = Regex("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
    fun uuid(value: String) = uuidPattern.matches(value)
    fun newId(): String = UUID.randomUUID().toString()

    fun identity(value: JsonObject): Boolean {
        val attachment = value.obj("attachment") ?: return false
        val lifetime = attachment.text("lifetime_id") ?: return false
        return value.number("version") == 1L && uuid(value.text("generation") ?: "") &&
            value.integer("control_epoch", 1) && uuid(attachment.text("client_instance_id") ?: "") &&
            uuid(attachment.text("attachment_id") ?: "") && lifetime.length in 1..128 &&
            lifetime.none { it.code < 32 || it.code == 127 } && hash(attachment["family_id"])
    }

    private fun sameIdentity(a: JsonObject, b: JsonObject): Boolean =
        listOf("version", "generation", "control_epoch", "attachment").all { a[it] == b[it] }

    private fun hash(value: JsonElement?) = (value as? JsonPrimitive)?.let { it.isString && digest.matches(it.content) } == true
    private fun JsonObject.integer(key: String, min: Long = 0, max: Long = MAX_SAFE_INTEGER) = number(key)?.let { it in min..max } == true
    private fun JsonObject.flag(key: String) = get(key)?.wireBoolean() != null
    private fun JsonObject.rows(key: String, limit: Int): List<JsonObject>? = (get(key) as? JsonArray)
        ?.takeIf { it.size <= limit }?.map { it as? JsonObject ?: return null }
    private fun JsonObject.hashes(key: String, limit: Int) = (get(key) as? JsonArray)
        ?.let { it.size <= limit && it.all(::hash) } == true

    private fun interval(row: JsonObject): Boolean = hash(row["artifact_id"]) && hash(row["rendition_id"]) &&
        row.integer("timescale", 1, 1_000_000) && row.integer("from_tick") && row.integer("through_tick", 1) &&
        row.number("from_tick")!! < row.number("through_tick")!! && row.integer("byte_length", 1, 256L * 1024 * 1024)

    private fun sameInterval(a: JsonObject, b: JsonObject) =
        listOf("artifact_id", "rendition_id", "timescale", "from_tick", "through_tick", "byte_length").all { a[it] == b[it] }

    private fun transaction(row: JsonObject): Boolean {
        val ready = row.rows("ready", 128) ?: return false
        val reserved = row.rows("reserved", 128) ?: return false
        val appended = row.rows("appended", 128) ?: return false
        val state = row.text("state") ?: return false
        if (!uuid(row.text("transaction_id") ?: "") || !row.integer("intent_revision", 1) || !hash(row["target_rendition_id"]) ||
            state !in setOf("preparing", "ready", "scheduled", "appended", "presented", "cancelling", "retained_current", "superseded", "recovery_owned", "disposed") ||
            !listOf("intent_superseded", "cancel_requested", "ever_appended").all { row.flag(it) } || !row.hashes("disposed", 128)) return false
        if (!listOf(ready, reserved, appended).all { rows -> rows.all { interval(it) && it["rendition_id"] == row["target_rendition_id"] } &&
                rows.map { it["artifact_id"] }.toSet().size == rows.size }) return false
        if (!appended.all { entry -> reserved.any { sameInterval(it, entry) } }) return false
        val presented = row["first_presented_tick"]?.takeUnless { it is JsonNull }
        val observed = row["first_presented_at_ms"]?.takeUnless { it is JsonNull }
        if (presented == null && observed != null || presented != null &&
            (!row.integer("first_presented_tick") || !row.integer("first_presented_at_ms", 1))) return false
        return row["ever_appended"]?.wireBoolean() == true || (appended.isEmpty() && presented == null)
    }

    fun response(value: JsonObject, request: JsonObject): Boolean {
        if (!identity(request) || !sameIdentity(value, request) || !value.integer("revision", 1)) return false
        val terminal = value["terminal"]?.takeUnless { it is JsonNull }
        if (terminal != null && terminal.wireBoolean() == null) return false
        val transition = request.obj("transition")
        if (terminal?.wireBoolean() == true && (request.obj("window") != null || request.obj("frontier") != null ||
            transition?.obj("operation")?.text("kind") in setOf("prepare", "scheduled"))) return false
        val ledger = value.obj("ledger") ?: return false
        if (!sameIdentity(ledger, request) || !ledger.integer("accepted_sequence") || !ledger.integer("latest_intent_revision")) return false
        val transactions = ledger.rows("transactions", 16) ?: return false
        if (!transactions.all { transaction(it) && it.number("intent_revision")!! <= ledger.number("latest_intent_revision")!! } ||
            transactions.map { it["transaction_id"] }.toSet().size != transactions.size) return false
        val audio = if (ledger["shared_audio_reserved"] == null) emptyList() else ledger.rows("shared_audio_reserved", 128) ?: return false
        if (!audio.all { interval(it) && it.number("timescale") == 48_000L && it["rendition_id"] == ledger["shared_audio_rendition_id"] } ||
            audio.map { it["artifact_id"] }.toSet().size != audio.size) return false
        val pins = transactions.flatMap { requireNotNull(it.rows("reserved", 128)) } + audio
        if (pins.size > 128 || pins.sumOf { requireNotNull(it.number("byte_length")) } > 256L * 1024 * 1024) return false
        val receipt = value.obj("receipt")
        if (transition == null) return value["receipt"] == null || value["receipt"] is JsonNull
        if (receipt == null || !sameIdentity(receipt, request) || receipt.number("accepted_sequence") != transition.number("sequence") ||
            ledger.number("accepted_sequence")!! < (transition.number("sequence") ?: return false)) return false
        val settled = receipt.obj("transaction") ?: return false
        if (!transaction(settled) || settled["transaction_id"] != transition["transaction_id"]) return false
        val operation = transition.obj("operation") ?: return false
        fun acceptedIntervals(field: String, destination: String): Boolean {
            val intervals = operation.rows(field, 128) ?: return false
            val accepted = settled.rows(destination, 128) ?: return false
            return intervals.all { interval(it) && accepted.any { pin -> sameInterval(pin, it) } }
        }
        fun disposed(field: String): Boolean {
            val artifacts = operation[field] as? JsonArray ?: return false
            if (artifacts.size > 128 || !artifacts.all(::hash)) return false
            return artifacts.all { artifact -> artifact in settled.getValue("disposed").jsonArray &&
                listOf("ready", "reserved", "appended").all { key -> settled.getValue(key).jsonArray.none {
                    it.jsonObject["artifact_id"] == artifact
                } } }
        }
        return when (operation.text("kind")) {
            "prepare" -> settled["intent_revision"] == operation["intent_revision"] &&
                settled["target_rendition_id"] == operation["target_rendition_id"]
            "scheduled" -> acceptedIntervals("intervals", "reserved")
            "appended" -> acceptedIntervals("intervals", "appended")
            "presented" -> {
                val tick = operation.number("film_tick") ?: return false
                settled.number("first_presented_tick") != null && settled.getValue("appended").jsonArray.any {
                    val pin = it.jsonObject
                    pin["artifact_id"] == operation["artifact_id"] && requireNotNull(pin.number("from_tick")) <= tick &&
                        tick < requireNotNull(pin.number("through_tick"))
                }
            }
            "cancel_unappended" -> settled["cancel_requested"]?.wireBoolean() == true && acceptedIntervals("completed", "appended")
            "disposed" -> disposed("artifacts")
            "recovery_owned" -> disposed("disposed_artifacts")
            else -> false
        }
    }

    fun family(value: JsonObject): Boolean {
        if (value.number("version") != 1L || value.text("mode") !in setOf("controlled", "autonomous_reserved") ||
            !hash(value["family_id"]) || value.text("master") != "master.m3u8") return false
        val video = value.rows("video", 8)?.takeIf { it.size >= 2 } ?: return false
        if (!video.all { row -> candidate.matches(row.text("candidate_id") ?: "") && hash(row["rendition_id"]) && hash(row["init_id"]) &&
                row.text("codec") == "avc1.640032" && row.integer("width", 1, 8192) && row.integer("height", 1, 8192) &&
                row.integer("timescale", 1, 1_000_000) && row.integer("frame_ticks", 1) && row.integer("segment_ticks", 1) &&
                row.number("segment_ticks")!! % row.number("frame_ticks")!! == 0L && row.integer("peak_bps", 1) &&
                row.text("playlist") == "video/${row.text("rendition_id")}/index.m3u8" }) return false
        if (!listOf("rendition_id", "candidate_id").all { key -> video.map { it[key] }.toSet().size == video.size } ||
            video.map { it["width"] to it["height"] }.toSet().size != video.size ||
            !video.all { row -> listOf("timescale", "frame_ticks", "segment_ticks").all { row[it] == video.first()[it] } }) return false
        val audioValue = value["audio"] ?: return true
        if (audioValue is JsonNull) return true
        val audio = audioValue as? JsonObject ?: return false
        return hash(audio["rendition_id"]) && hash(audio["init_id"]) && audio.text("codec") == "mp4a.40.2" &&
            audio.number("timescale") == 48_000L && audio.integer("channels", 1, 8) && audio.integer("peak_bps", 1) &&
            audio.text("playlist") == "audio/${audio.text("rendition_id")}/index.m3u8" && video.none { it["rendition_id"] == audio["rendition_id"] }
    }
}

internal fun JsonObject.obj(key: String): JsonObject? = get(key) as? JsonObject
internal fun JsonObject.text(key: String): String? = (get(key) as? JsonPrimitive)?.takeIf { it.isString }?.content
internal fun JsonObject.number(key: String): Long? = (get(key) as? JsonPrimitive)?.takeUnless { it.isString }
    ?.longOrNull?.takeIf { it in 0..ContinuousQualityWire.MAX_SAFE_INTEGER }

internal fun JsonElement.wireBoolean(): Boolean? = (this as? JsonPrimitive)?.takeUnless { it.isString }?.booleanOrNull
