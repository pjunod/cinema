package tv.plurx.app.data

import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.json.*
import tv.plurx.app.player.ActionAcknowledgement
import tv.plurx.app.player.CodecPolicy
import tv.plurx.app.player.ControlAction
import tv.plurx.app.player.ControlResponse
import tv.plurx.app.player.DynamicCapabilities
import tv.plurx.app.player.DynamicRangePolicy
import tv.plurx.app.player.PlaybackControl
import tv.plurx.app.player.PlaybackDemand
import tv.plurx.app.player.RenderState
import java.io.IOException

private const val MAX_SAFE = 9_007_199_254_740_991L
private val V4 = Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}")

/** A started B session of either presentation. Every network use repeats
 * the binding checks against the retained context; nothing here is a Local ID. */
internal sealed interface SharedBoundSession {
    val context: PlaybackFileContext
    val request: CreateSessionReq
    val sessionId: String
    fun validateBound()
}

/** The media types B relays for a Shared direct session (sharing_direct_wire::DIRECT_MIMES). */
internal val SharedDirectMimes = setOf(
    "video/mp4", "video/webm", "video/x-matroska", "video/mp2t", "video/x-msvideo",
    "audio/mp4", "audio/aac", "audio/mpeg", "audio/flac", "audio/ogg", "audio/wav",
    "audio/x-ms-wma", "application/octet-stream",
)

/** The subset ExoPlayer's progressive extractors read as a file. WMA has no
 * extractor and an untyped body would be a guess, so those take Copy HLS. */
internal val SharedDirectPlayableMimes = SharedDirectMimes - setOf("audio/x-ms-wma", "application/octet-stream")

/** B's complete direct Start reply: exactly five fields, no control route, no
 * playlist, and a byte URL that is this context's own file alias bound to the
 * returned B session. Played without account headers; ended with DELETE. */
internal class SharedStartedDirect private constructor(
    override val context: PlaybackFileContext, override val request: CreateSessionReq,
    override val sessionId: String, val url: String, val length: Long, val mime: String,
) : SharedBoundSession {
    val playable: Boolean get() = mime in SharedDirectPlayableMimes
    override fun validateBound() {
        require(context.sessionId == sessionId && V4.matches(sessionId))
        context.validateSharedReference(requireNotNull(context.reference), context.sourceFileId, requireNotNull(context.revision))
        require(url == context.path("direct"))
    }
    companion object {
        fun decode(text: String, base: PlaybackFileContext, request: CreateSessionReq): SharedStartedDirect {
            require(text.toByteArray().size <= 16_384)
            require(base.reference != null && base.sessionId == null && request.presentation == "direct")
            val wire = Json.parseToJsonElement(text).jsonObject
            require(wire.keys == setOf("presentation", "session_id", "url", "length", "mime"))
            fun string(key: String) = wire.getValue(key).jsonPrimitive.let { require(it.isString); it.content }
            require(string("presentation") == "direct")
            val session = string("session_id"); require(V4.matches(session))
            val length = wire.getValue("length").jsonPrimitive.let {
                require(!it.isString && it.content.matches(Regex("0|[1-9][0-9]{0,15}"))); requireNotNull(it.longOrNull)
            }
            require(length in 0..MAX_SAFE)
            val mime = string("mime"); require(mime in SharedDirectMimes)
            val bound = base.withSession(session)
            val direct = SharedStartedDirect(bound, request, session, string("url"), length, mime)
            direct.validateBound()
            return direct
        }
    }
}

/** What the viewer asked for, raw. Never the delivered rendition. */
internal data class SharedSelection(val quality: PlaybackQuality, val audio: Int? = null, val subtitle: Int? = null, val burn: Boolean = false) {
    init {
        require(audio == null || audio in 0..1024)
        require(subtitle == null || subtitle in 0..1024)
    }
    /** `/decision` query for this ask, the same closed grammar Start uses. */
    fun decisionQuery(): Map<String, String> = buildMap {
        when (quality) {
            PlaybackQuality.Auto -> Unit
            PlaybackQuality.Original -> put("force", "original")
            else -> put("force", "transcode")
        }
        audio?.let { put("audio", it.toString()) }
        subtitle?.let { put("subtitle", it.toString()) }
    }
    /** The control `selection` this ask expresses; equal to the frozen
     * selection of the Start it produces (see [SharedPlaybackPlan.frozenControlSelection]). */
    fun controlSelection(): JsonObject = buildJsonObject {
        put("quality", buildJsonObject {
            when (quality) {
                PlaybackQuality.Auto -> put("mode", "auto")
                PlaybackQuality.Original -> put("mode", "original")
                else -> { put("mode", "manual"); put("height", requireNotNull(quality.rungHeight)) }
            }
        })
        put("audio_track", audio?.let(::JsonPrimitive) ?: JsonNull)
        put("audio_offset_ms", 0)
        put("subtitle", buildJsonObject {
            if (subtitle != null) { put("mode", if (burn) "burn" else "native"); put("track", subtitle) } else put("mode", "off")
        })
        put("codec", "auto"); put("dynamic_range", "auto")
    }
}

/**
 * One ordinary CreateSession for a Shared Start. Direct play is chosen only when
 * the Source's decision says direct play for these caps and the ask is the
 * file as it is: default audio, no subtitle, no A/V offset. Anything else is
 * Copy or encoded HLS. A reopen keeps the viewer's playback id (B groups one
 * player's sessions by it to supersede the predecessor) and mints a fresh
 * request id; it never carries lineage fields.
 */
internal fun sharedPlaybackPlan(
    subject: SharedPlaybackSubject, result: SharedDecisionClient.Result, selection: SharedSelection,
    playbackId: String, requestId: String, allowDirect: Boolean,
): SharedPlaybackPlan {
    val decision = result.decision
    val presentation = decision.presentation
    val direct = allowDirect && decision.method == "direct_play" && selection.audio == null && selection.subtitle == null &&
        presentation.audio_offset_ms == 0L && !presentation.transcode_audio
    val track = selection.subtitle?.let { index -> presentation.subtitles.firstOrNull { it.index == index.toLong() } }
    val burn = selection.burn || track?.isNativeHls == false
    val start = subject.resumeMs.toDouble() / 1000
    val body = if (direct) CreateSessionReq(
        playback_id = playbackId, request_id = requestId, start = start,
        quality_auto = selection.quality == PlaybackQuality.Auto, caps = result.caps, presentation = "direct",
    ) else CreateSessionReq(
        playback_id = playbackId, request_id = requestId, height = selection.quality.rungHeight,
        quality_auto = selection.quality == PlaybackQuality.Auto, start = start, audio = selection.audio,
        native_subtitles = selection.subtitle?.takeUnless { burn }?.let { true }, subtitle = selection.subtitle?.takeUnless { burn },
        subtitle_burn = selection.subtitle?.takeIf { burn },
        copy = !burn && decision.method != "transcode", aac = presentation.transcode_audio,
        hdr10 = if (decision.method == "transcode" && presentation.delivered_dynamic_range == "hdr10") true else null, caps = result.caps,
    )
    return SharedPlaybackPlan(subject, decision, result.caps, body)
}

/**
 * The name a client declares beside `prepare_replacement` to be offered a
 * Shared successor. The Local promise alone is not enough: B answers a client
 * that has not also declared this one `none`, because such a client would keep
 * beating Shared progress on the session it left.
 */
internal const val SHARED_PREPARE_REPLACEMENT_ACTION = "shared_prepare_replacement"

/** The actions a Shared channel declares: both names, or none at all (P0 reopen). */
internal fun sharedSupportedActions(prepared: Boolean): List<String> =
    if (prepared) listOf(PlaybackControl.PREPARE_REPLACEMENT_ACTION, SHARED_PREPARE_REPLACEMENT_ACTION) else emptyList()

/** Shared capabilities for sequence 1: SDR only; dual-player preparation only
 * when this player will prime a Shared successor B offers. */
internal fun sharedControlCapabilities(caps: DeviceCaps, dualPlayer: Boolean = false): DynamicCapabilities = DynamicCapabilities(
    platform = "android", maxHeight = PlaybackControl.MAX_HEIGHT,
    codecs = caps.video.mapNotNull {
        when (it.codec.lowercase()) { "h264" -> CodecPolicy.H264; "hevc" -> CodecPolicy.HEVC; "av1" -> CodecPolicy.AV1; else -> null }
    }.distinct().ifEmpty { listOf(CodecPolicy.H264) },
    dynamicRanges = buildList {
        add(DynamicRangePolicy.SDR)
        val present = caps.video.flatMap { it.present }
        if (caps.display.hdr && "pq" in present) add(DynamicRangePolicy.HDR10)
        if (caps.display.hdr && "hlg" in present) add(DynamicRangePolicy.HLG)
        if (caps.display.dolby_vision && caps.video.any { !it.dv_profiles.isNullOrEmpty() }) add(DynamicRangePolicy.DOLBY_VISION)
    }, dualPlayerPreparation = dualPlayer,
)

/** What the renderer is doing when an exchange is built. */
internal data class SharedControlState(
    val demand: PlaybackDemand, val positionMs: Long, val bufferedThroughMs: Long,
    val renderState: RenderState, val playbackRate: Double = 1.0, val seekTargetMs: Long? = null,
)

internal sealed interface SharedControlOutcome {
    /**
     * B accepted this exact sequence under the B tuple. [action] is a
     * validated `prepare` naming B's successor (only to a channel that
     * declared both names), with its exact wire in [actionWire]; otherwise null.
     */
    data class Accepted(
        val preparation: String?, val sequence: Long = 0,
        val action: ControlAction? = null, val actionWire: JsonObject? = null,
    ) : SharedControlOutcome
    /** A definitive refusal; the renderer stays where it is. */
    data class Refused(val status: Int, val code: String?) : SharedControlOutcome
    /** 410: B retired the session; only a fresh Start can continue. */
    data class Ended(val code: String?) : SharedControlOutcome
    /** No answer this exchange could act on (transport, transition or budget). */
    data class Unavailable(val status: Int?, val code: String?) : SharedControlOutcome
}

/**
 * Current-rendition control for one started Shared HLS session. The B tuple is
 * the Start's (generation = B incarnation, control_epoch = B epoch), one
 * exchange is in flight at a time, sequences are ordered and never reused, and
 * an uncertain or deferred exchange is resent byte for byte. Unless [prepared],
 * the client declares no actions and B may answer only `none`. A prepared
 * channel declares both successor names and may be offered a `prepare`, and
 * its acknowledgements ride on this, the predecessor's, channel. The caller
 * applies a renderer change only after [SharedControlOutcome.Accepted].
 */
internal class SharedControlChannel(
    private val client: SharedDecisionClient, val playback: SharedStartedPlayback,
    private val clientInstanceId: String, private val capabilities: DynamicCapabilities,
    private val prepared: Boolean = false,
) {
    private val mutex = Mutex()
    private val bootstrap = requireNotNull(playback.start.response.control)
    var sequence = 0L; private set
    /** The last request sent, kept so an uncertain answer can be asked again exactly. */
    private var last: String? = null
    init {
        require(PlaybackControl.isUuid(clientInstanceId) && capabilities.isValid)
        require(bootstrap.url == "/api/v1/hls/${playback.sessionId}/control")
        require(capabilities.dualPlayerPreparation == prepared)
    }
    private fun clamp(value: Long): Long {
        val duration = playback.start.response.duration_ms
        return value.coerceIn(0, if (duration != null && duration in 0..PlaybackControl.MAX_MEDIA_MILLIS) duration else PlaybackControl.MAX_MEDIA_MILLIS)
    }
    fun body(sequence: Long, state: SharedControlState, selection: JsonObject, acknowledgement: ActionAcknowledgement? = null): String {
        val seeking = state.renderState == RenderState.SEEKING
        require(seeking == (state.seekTargetMs != null))
        val position = clamp(state.positionMs)
        return buildJsonObject {
            put("protocol", PlaybackControl.PROTOCOL); put("generation", bootstrap.generation)
            put("control_epoch", bootstrap.controlEpoch); put("client_instance_id", clientInstanceId)
            put("sequence", sequence); put("demand", Net.json.encodeToJsonElement(PlaybackDemand.serializer(), state.demand))
            put("position_ms", position)
            put("buffered_through_ms", if (seeking) clamp(state.bufferedThroughMs) else maxOf(position, clamp(state.bufferedThroughMs)))
            put("playback_rate", state.playbackRate)
            put("render_state", Net.json.encodeToJsonElement(RenderState.serializer(), state.renderState))
            state.seekTargetMs?.let { put("seek_target_ms", clamp(it)) }
            put("selection", selection)
            if (sequence == 1L) put("capabilities", Net.json.encodeToJsonElement(DynamicCapabilities.serializer(), capabilities))
            acknowledgement?.let {
                require(prepared && it.isValid)
                put("acknowledgement", Net.json.encodeToJsonElement(ActionAcknowledgement.serializer(), it))
            }
            put("supported_actions", JsonArray(sharedSupportedActions(prepared).map(::JsonPrimitive)))
        }.toString()
    }
    suspend fun exchange(state: SharedControlState, selection: JsonObject, acknowledgement: ActionAcknowledgement? = null): SharedControlOutcome = mutex.withLock {
        require(sequence < MAX_SAFE)
        val sent = sequence + 1
        val text = body(sent, state, selection, acknowledgement)
        require(text.toByteArray().size <= 65_536)
        sequence = sent; last = text
        send(text, sent)
    }
    /**
     * Ask the last exchange again, byte for byte, under its own sequence. An
     * acknowledgement whose answer was lost is settled this way: B replays the
     * exact answer it gave, or settles now, and never twice.
     */
    suspend fun replayLast(): SharedControlOutcome = mutex.withLock { send(requireNotNull(last), sequence) }
    private suspend fun send(text: String, sent: Long): SharedControlOutcome {
        var uncertain = 0; var deferred = 0
        while (true) {
            val reply = try { client.control(playback, text) } catch (error: IOException) {
                if (++uncertain > 1) return SharedControlOutcome.Unavailable(null, null)
                continue
            }
            val fields = runCatching { Json.parseToJsonElement(reply.text).jsonObject }.getOrNull()
            val code = fields?.get("code")?.jsonPrimitive?.takeIf { it.isString }?.content
            when (reply.status) {
                200 -> return accepted(reply.text, sent)
                409, 422 -> return SharedControlOutcome.Refused(reply.status, code)
                410 -> return SharedControlOutcome.Ended(code)
                425, 429, 503 -> {
                    if (++deferred > 2) return SharedControlOutcome.Unavailable(reply.status, code)
                    val after = fields?.get("retry_after_ms")?.jsonPrimitive?.takeIf { !it.isString }?.longOrNull ?: 500L
                    delay(after.coerceIn(250L, 5_000L))
                }
                else -> return SharedControlOutcome.Unavailable(reply.status, code)
            }
        }
    }
    private fun accepted(text: String, sent: Long): SharedControlOutcome {
        val response = runCatching { Net.json.decodeFromString(ControlResponse.serializer(), text) }.getOrNull()
            ?: return SharedControlOutcome.Refused(200, "protocol")
        val offer = prepared && response.action.type == PlaybackControl.PREPARE_ACTION_TYPE && response.action.preparedPayloadIsValid
        if (response.protocol != PlaybackControl.PROTOCOL || response.generation != bootstrap.generation ||
            response.controlEpoch != bootstrap.controlEpoch || response.acceptedSequence != sent || (response.action.type != "none" && !offer)) {
            return SharedControlOutcome.Refused(200, "protocol")
        }
        if (!offer) return SharedControlOutcome.Accepted(response.delivery?.preparation, sent)
        val wire = Json.parseToJsonElement(text).jsonObject.getValue("action").jsonObject
        return SharedControlOutcome.Accepted(response.delivery?.preparation, sent, response.action, wire)
    }
}
