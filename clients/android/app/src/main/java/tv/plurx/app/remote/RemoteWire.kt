package tv.plurx.app.remote

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import java.util.UUID

internal const val REMOTE_VERSION = "cinema.remote.v1"
internal const val REMOTE_MAX_INTEGER = 9_007_199_254_740_991L
internal enum class RemoteOutcome(val wire: String) {
    Applied("applied"), Duplicate("duplicate_or_old"), Expired("expired"), StaleTarget("stale_target"),
    StaleControl("stale_control"), StaleContext("stale_context"), StaleFocus("stale_focus"),
    Restricted("restricted_surface"), Unauthorized("unauthorized"), Unsupported("unsupported"),
    Busy("busy"), Unavailable("unavailable"), Invalid("invalid");
    companion object { fun parse(value: String) = entries.firstOrNull { it.wire == value } ?: Invalid }
}
@Serializable
internal data class RemoteTarget(val owner_node_id: String, val session_id: String, val receiver_epoch: String)
internal enum class RemoteCreditKind(val wire: String, val ttl: Long) { Interaction("interaction", 1000), Playback("playback", 3000) }
@Serializable
internal data class RemoteCredit(val nonce: String, val kind: String)
internal data class RemoteAction(val type: String, val fields: JsonObject = JsonObject(emptyMap())) {
    val creditKind: RemoteCreditKind get() = if (type in setOf("navigate", "select", "back", "home", "text_replace", "open_tracks")) RemoteCreditKind.Interaction else RemoteCreditKind.Playback
    fun text(key: String) = fields[key]?.jsonPrimitive?.contentOrNull
    fun integer(key: String) = fields[key]?.jsonPrimitive?.longOrNull
    fun boolean(key: String) = fields[key]?.jsonPrimitive?.booleanOrNull
    fun json() = JsonObject(fields + ("type" to JsonPrimitive(type)))
    fun validate() {
        val keys = when (type) {
            "navigate" -> setOf("direction")
            "select", "back", "home", "stop" -> emptySet()
            "set_playing" -> setOf("playing")
            "seek_relative" -> setOf("seconds")
            "seek_absolute" -> setOf("position_ms")
            "open_tracks" -> setOf("kind")
            "choose_track" -> setOf("kind", "option_id")
            "text_replace" -> setOf("text_nonce", "text")
            "play_item" -> setOf("item_id")
            else -> error("unknown remote action")
        }
        require(fields.keys == keys)
        when (type) {
            "navigate" -> require(string("direction") in setOf("up", "down", "left", "right"))
            "set_playing" -> require(fields["playing"] is JsonPrimitive && !fields.getValue("playing").jsonPrimitive.isString && boolean("playing") != null)
            "seek_relative" -> require(number("seconds") in setOf(-30L, -10L, 10L, 30L))
            "seek_absolute" -> require(number("position_ms") in 0..REMOTE_MAX_INTEGER)
            "play_item" -> require(number("item_id") in 1..REMOTE_MAX_INTEGER)
            "open_tracks", "choose_track" -> require(string("kind") in setOf("audio", "subtitles", "quality"))
            "text_replace" -> { RemoteWire.uuid(string("text_nonce")); require(string("text").toByteArray(Charsets.UTF_8).size <= 512) }
        }
        if (type == "choose_track") require(string("option_id").isNotEmpty() && string("option_id").toByteArray(Charsets.UTF_8).size <= 128)
    }
    private fun string(key: String): String { val p = fields.getValue(key).jsonPrimitive; require(p.isString); return p.content }
    private fun number(key: String): Long { val p = fields.getValue(key).jsonPrimitive; require(!p.isString); return requireNotNull(p.longOrNull) }
}
internal data class RemoteCommand(val target: RemoteTarget, val grantId: String, val controlEpoch: String, val sequence: Long,
    val credit: String, val contextRevision: Long, val focusRevision: Long, val action: RemoteAction) {
    fun validate() {
        RemoteWire.validateTarget(target); RemoteWire.uuid(grantId); RemoteWire.uuid(controlEpoch); RemoteWire.uuid(credit)
        require(sequence in 1..REMOTE_MAX_INTEGER && contextRevision in 1..REMOTE_MAX_INTEGER && focusRevision in 1..REMOTE_MAX_INTEGER)
        action.validate()
    }
    fun json(): JsonObject = buildJsonObject {
        put("version", REMOTE_VERSION); put("target", RemoteWire.targetJson(target)); put("grant_id", grantId); put("control_epoch", controlEpoch)
        put("sequence", sequence); put("credit", credit); put("context_revision", contextRevision); put("focus_revision", focusRevision); put("action", action.json())
    }
}
internal object RemoteWire {
    val json = Json { ignoreUnknownKeys = false; isLenient = false; explicitNulls = true }
    private val uuidPattern = Regex("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
    fun uuid(value: String): String { require(uuidPattern.matches(value)); UUID.fromString(value); return value.lowercase() }
    fun validateTarget(target: RemoteTarget) {
        require(target.owner_node_id.isNotEmpty() && target.owner_node_id.toByteArray(Charsets.UTF_8).size <= 128 && target.owner_node_id.none { it.code < 32 || it.code == 127 })
        uuid(target.session_id); uuid(target.receiver_epoch)
    }
    fun targetJson(target: RemoteTarget) = buildJsonObject { put("owner_node_id", target.owner_node_id); put("session_id", target.session_id); put("receiver_epoch", target.receiver_epoch) }
    fun target(value: JsonObject): RemoteTarget {
        require(value.keys == setOf("owner_node_id", "session_id", "receiver_epoch"))
        return RemoteTarget(value.string("owner_node_id"), uuid(value.string("session_id")), uuid(value.string("receiver_epoch"))).also(::validateTarget)
    }
    fun command(bytes: ByteArray): RemoteCommand {
        require(bytes.size <= 16 * 1024)
        val root = objectBody(bytes, 16 * 1024)
        require(root.keys == setOf("version", "target", "grant_id", "control_epoch", "sequence", "credit", "context_revision", "focus_revision", "action"))
        require(root.string("version") == REMOTE_VERSION)
        val action = root.getValue("action").jsonObject
        val value = RemoteCommand(target(root.getValue("target").jsonObject), uuid(root.string("grant_id")), uuid(root.string("control_epoch")), root.number("sequence"),
            uuid(root.string("credit")), root.number("context_revision"), root.number("focus_revision"), RemoteAction(action.string("type"), JsonObject(action - "type")))
        value.validate(); return value
    }
    /** Production HTTP uses this before parsing nested commands, never a permissive DTO decoder. */
    fun objectBody(bytes: ByteArray, maximum: Int = 64 * 1024): JsonObject {
        require(bytes.size <= maximum)
        val text = Charsets.UTF_8.newDecoder().decode(java.nio.ByteBuffer.wrap(bytes)).toString()
        StrictRemoteJson.check(text)
        return json.parseToJsonElement(text).jsonObject
    }
    fun pollCommands(body: JsonObject): List<RemoteCommand> {
        val values = body.getValue("commands").jsonArray
        require(values.size <= 32)
        return values.map { command(it.toString().toByteArray(Charsets.UTF_8)) }
    }
    fun safeLabel(value: String, maximum: Int = 256, fallback: String = "Untitled"): String {
        val normalized = value.filterNot { Character.isISOControl(it) }.trim().split(Regex("\\s+")).filter(String::isNotEmpty).joinToString(" ")
        val candidate = normalized.ifEmpty { fallback }
        val out = StringBuilder()
        candidate.codePoints().forEachOrdered { code -> val next = String(Character.toChars(code)); if ((out.toString() + next).toByteArray(Charsets.UTF_8).size <= maximum) out.append(next) }
        return out.toString()
    }
}
internal fun JsonObject.string(key: String): String { val p = getValue(key).jsonPrimitive; require(p.isString); return p.content }
internal fun JsonObject.number(key: String): Long { val p = getValue(key).jsonPrimitive; require(!p.isString); return requireNotNull(p.longOrNull) }

/** Duplicate keys and floating/exponent integer lexemes must be rejected before JSON drops them. */
internal object StrictRemoteJson {
    fun check(text: String) {
        var i = 0
        val objectKeys = ArrayDeque<MutableSet<String>?>()
        while (i < text.length) {
            when (val ch = text[i]) {
                '{' -> { require(objectKeys.size < 32); objectKeys.addLast(mutableSetOf()); i++ }
                '[' -> { require(objectKeys.size < 32); objectKeys.addLast(null); i++ }
                '}', ']' -> { require(objectKeys.isNotEmpty()); objectKeys.removeLast(); i++ }
                '"' -> {
                    val start = i++
                    var closed = false
                    while (i < text.length) {
                        if (text[i] == '\\') { i += 2; continue }
                        if (text[i++] == '"') { closed = true; break }
                    }
                    require(closed)
                    var next = i
                    while (next < text.length && text[next].isWhitespace()) next++
                    if (next < text.length && text[next] == ':') {
                        val key = RemoteWire.json.parseToJsonElement(text.substring(start, i)).jsonPrimitive.content
                        require(objectKeys.lastOrNull()?.add(key) == true)
                    }
                }
                '-', in '0'..'9' -> {
                    val start = i++
                    while (i < text.length && text[i] in "0123456789.eE+-") i++
                    val token = text.substring(start, i)
                    require(token != "-0" && Regex("-?(0|[1-9][0-9]*)").matches(token)); require(token.toLongOrNull() != null)
                }
                else -> { require(ch.code >= 32 || ch in "\n\r\t"); i++ }
            }
        }
        require(objectKeys.isEmpty())
    }
}

internal fun RemoteOutcome.viewerMessage(): String = when (this) {
    RemoteOutcome.Applied -> "TV accepted the action."
    RemoteOutcome.Duplicate -> "That action was already handled."
    RemoteOutcome.Expired -> "That action arrived too late. Try again."
    RemoteOutcome.StaleTarget -> "The TV session changed. Choose the screen again."
    RemoteOutcome.StaleControl -> "Control changed. Request control again."
    RemoteOutcome.StaleContext -> "The TV screen changed. Try again."
    RemoteOutcome.StaleFocus -> "TV focus moved. Try again."
    RemoteOutcome.Restricted -> "Use the TV remote for this screen."
    RemoteOutcome.Unauthorized -> "Pair or sign in again."
    RemoteOutcome.Unsupported -> "That control is unavailable here."
    RemoteOutcome.Busy -> "The TV is busy. Try again."
    RemoteOutcome.Unavailable -> "The TV is unavailable."
    RemoteOutcome.Invalid -> "That action could not be sent."
}

/** Presence reserves server response space without changing media/option identities. */
internal object RemoteStateBudget {
    const val maximumBytes = 48 * 1024
    fun fit(state: JsonObject): JsonObject {
        fun fits(value: JsonObject) = value.toString().toByteArray(Charsets.UTF_8).size <= maximumBytes
        if (fits(state)) return state
        val playback = state["playback"] as? JsonObject
        val shortened = if (playback != null) JsonObject(state + ("playback" to JsonObject(playback +
            ("title" to JsonPrimitive(RemoteWire.safeLabel(playback["title"]?.jsonPrimitive?.content.orEmpty(), 64))) +
            ("tracks" to JsonArray(playback["tracks"]?.jsonArray.orEmpty().map { element ->
                val track = element.jsonObject
                JsonObject(track + ("label" to JsonPrimitive(RemoteWire.safeLabel(track["label"]?.jsonPrimitive?.content.orEmpty(), 64))))
            }))))) else state
        if (fits(shortened)) return shortened
        // Size alone does not restrict transport. Omit unrepresentable presentation,
        // and its track-choice capabilities; Pause/Stop remain truthful and usable.
        val result = JsonObject(shortened + ("playback" to JsonNull) + ("capabilities" to JsonArray(
            shortened["capabilities"]!!.jsonArray.filter { it.jsonPrimitive.content !in setOf("open_tracks", "choose_track") })))
        require(fits(result))
        return result
    }
}

internal object RemotePresenceState {
    fun encode(revision: Long, snapshot: RemoteNavigationCoordinator.Snapshot, capabilities: Set<String>, credits: List<RemoteCredit>, playback: JsonObject?): JsonObject =
        RemoteStateBudget.fit(buildJsonObject {
            put("state_revision", revision); put("context_revision", snapshot.context.revision); put("focus_revision", snapshot.context.focusRevision); put("route", snapshot.route)
            put("capabilities", JsonArray(capabilities.take(12).map(::JsonPrimitive)))
            put("focused_label", snapshot.label?.let(::JsonPrimitive) ?: JsonNull)
            put("credits", JsonArray(credits.map { buildJsonObject { put("nonce", it.nonce); put("kind", it.kind) } }))
            put("text_nonce", snapshot.textNonce?.let(::JsonPrimitive) ?: JsonNull); put("playback", playback ?: JsonNull)
        })
}
