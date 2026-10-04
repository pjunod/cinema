package tv.plurx.app.data

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.player.PGSOverlayCue
import tv.plurx.app.player.PGSOverlayPolicy

@Serializable
internal data class SharedPlaybackFileReference(
    val item: SharedPlaybackReference, val file_id: String, val revision: String, val lifecycle_generation: Long? = null,
) {
    fun validate(context: PlaybackFileContext) {
        item.validate()
        require(lifecycle_generation != null && lifecycle_generation > 0 && lifecycle_generation == context.lifecycleGeneration)
        context.validateSharedReference(item, file_id, revision)
    }
}

@Serializable
internal data class SharedDecisionPresentation(
    val delivery: Delivery? = null,
    val source: SourceSummary? = null,
    val reasons: List<String> = emptyList(),
    val transcode_audio: Boolean = false,
    /** The source DV metadata survives this decision, including direct play. */
    val preserve_dolby_vision: Boolean = false,
    val audio: List<AudioTrack> = emptyList(),
    val subtitles: List<SubTrack> = emptyList(),
    /** Present only for a selection-aware request; see [DecisionSelection]. */
    val selection: DecisionSelection? = null,
    val markers: List<Marker> = emptyList(),
    val ladder: List<Rung> = emptyList(),
    val quality_candidates: List<QualityCandidate> = emptyList(),
    val quality_candidate_id: String? = null,
    val display_aware_auto_protocol: String? = null,
    val audio_offset_ms: Long = 0,
    val declared_offset_ms: Long? = null,
    /**
     * The dynamic range of the bytes this delivery plan would put on the wire —
     * `"dolby_vision" | "hdr10" | "hlg" | "sdr"`, the source's own vocabulary
     * plus `"sdr"`, so a client compares source against delivered with string
     * equality. Absent on a server that predates it; the badge then falls back
     * to describing the source alone.
     */
    val delivered_dynamic_range: String? = null,
    /**
     * The Dolby Vision profile actually on the wire, when the delivery carries
     * Dolby Vision at all.
     *
     * [delivered_dynamic_range] answers `"dolby_vision"` for both a Profile 7
     * title preserved for a device that enumerates 7 and the same title
     * converted to 8.1 for one that does not, because the grade really is the
     * same in both. What differs is that in the second case the profile on
     * screen is not the profile on disk, and a badge reading the file's
     * profile for both is describing the disk rather than the picture.
     *
     * **Absent means "no answer", not "not Dolby Vision."** Three things
     * produce it: a delivery carrying no Dolby Vision at all (a transcode, a
     * strip), a source that never had any, and a library row scanned before
     * the profile columns existed. [delivered_dynamic_range] beside it is the
     * field that answers "is this Dolby Vision".
     */
    val delivered_dolby_vision_profile: Int? = null,
)

/** Retains every server wire field; never decoded through the numeric Local Decision. */
internal class SharedDecision private constructor(
    val fileId: String, val reference: SharedPlaybackFileReference,
    val method: String, val playUrl: String, val wire: JsonObject, val presentation: SharedDecisionPresentation,
) {
    fun validated(context: PlaybackFileContext): SharedDecision {
        reference.validate(context)
        context.validateDescriptiveUrl(playUrl)
        val delivery = wire.getValue("delivery").jsonObject
        require(delivery.strictString("mode") == if (method == "direct_play") "direct" else method)
        listOf("url", "sessions_url").forEach { key ->
            delivery[key]?.takeUnless { it == JsonNull }?.let {
                val url = delivery.strictString(key)
                context.validateDescriptiveUrl(url)
                if (key == "sessions_url") require(url == "${context.fileBase}/hls/sessions")
            }
        }
        return this
    }
    companion object {
        fun decode(text: String): SharedDecision {
            require(text.toByteArray().size <= 4_194_304)
            val wire = Json.parseToJsonElement(text).jsonObject
            val file = wire.strictString("file_id")
            require(PlaybackFileContext.canonicalId(file))
            val reference = decodeSharedReference(wire.getValue("reference"))
            require(reference.file_id == file)
            val method = wire.strictString("method")
            require(method in setOf("direct_play", "remux", "transcode"))
            wire.getValue("delivery").jsonObject
            return SharedDecision(file, reference, method, wire.strictString("play_url"), wire, Json { ignoreUnknownKeys = true }.decodeFromJsonElement(wire))
        }
    }
}

private fun decodeSharedReference(value: JsonElement): SharedPlaybackFileReference {
    val binding = value.jsonObject
    binding.strictString("file_id"); binding.strictString("revision")
    val item = binding.getValue("item").jsonObject
    listOf("import_id", "server_id", "catalogue_epoch", "library_id", "item_id").forEach(item::strictString)
    return Json.decodeFromJsonElement(value)
}

private fun JsonObject.strictString(key: String): String = getValue(key).jsonPrimitive.let {
    require(it.isString); it.content
}

internal sealed interface PlaybackSubject {
    data class Local(val itemId: String, val context: PlaybackFileContext) : PlaybackSubject
    data class Shared(val context: PlaybackFileContext) : PlaybackSubject
    fun validated(): PlaybackSubject {
        when (this) {
            is Local -> { require(PlaybackFileContext.canonicalId(itemId)); context.localId() }
            is Shared -> context.validateSharedReference(requireNotNull(context.reference), context.sourceFileId, requireNotNull(context.revision))
        }
        return this
    }
}

/** Preserves the full additive start wire beside the existing session-only projection. */
internal class SharedStart private constructor(val response: HlsStart, val wire: JsonObject) {
    fun validated(context: PlaybackFileContext): SharedStart {
        SharedStartValidation.validated(response, context)
        return this
    }
    companion object {
        fun decode(text: String): SharedStart {
            require(text.toByteArray().size <= 1_048_576)
            val wire = Json.parseToJsonElement(text).jsonObject
            wire.strictString("session_id"); wire.strictString("playlist_url")
            return SharedStart(Json { ignoreUnknownKeys = true }.decodeFromJsonElement(wire), wire)
        }
    }
}

/** Authenticated ordinary B Start metadata, never Source physical evidence. */
internal class SharedStartedPlayback(val start: SharedStart, val context: PlaybackFileContext, val request: CreateSessionReq)

internal fun SharedStart.bindInitial(context: PlaybackFileContext, request: CreateSessionReq): SharedStartedPlayback {
    require(context.reference != null && context.sessionId == null)
    require(response.vod == true && response.start_seconds.isFinite() && response.start_seconds >= 0 && response.start_seconds <= 9_007_199_254_740.0)
    require(response.duration_ms?.let { it >= 0 } != false)
    val control = requireNotNull(response.control)
    require(Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(control.generation))
    require(control.nextExchangeMs == 5_000L && control.leaseTimeoutMs == 300_000L)
    val bound = context.withSession(response.session_id)
    validated(bound)
    return SharedStartedPlayback(this, bound, request)
}

internal object SharedStartValidation {
    fun validated(start: HlsStart, context: PlaybackFileContext): HlsStart {
        context.validateSessionPlaylist(start.playlist_url, start.session_id)
        val control = requireNotNull(start.control)
        require(control.isValid && control.url == "/api/v1/hls/${start.session_id}/control")
        return start
    }
}

@Serializable
internal data class SharedPGSManifest(
    val schema: Int,
    val generation: String,
    @kotlinx.serialization.SerialName("file_id") val fileId: String,
    val reference: SharedPlaybackFileReference,
    @kotlinx.serialization.SerialName("track_index") val trackIndex: Long,
    val kind: String,
    val timebase: String,
    @kotlinx.serialization.SerialName("duration_ms") val durationMs: Long,
    val cues: List<PGSOverlayCue>,
) {
    fun validated(context: PlaybackFileContext, expectedTrackIndex: Long): SharedPGSManifest {
        reference.validate(context)
        require(
            schema == 1 &&
                Regex("[0-9a-f]{64}").matches(generation) &&
                fileId == reference.file_id &&
                trackIndex == expectedTrackIndex &&
                kind == "pgs" &&
                timebase == "source_ms" &&
                durationMs > 0 &&
                cues.size <= PGSOverlayPolicy.maximumManifestCues,
        ) { "invalid PGS overlay manifest header" }

        var previousEnd = 0L
        val dimensions = mutableMapOf<String, Pair<Int, Int>>()
        cues.forEach { cue ->
            require(
                cue.id.isNotBlank() &&
                    cue.startMs >= previousEnd &&
                    cue.endMs > cue.startMs &&
                    cue.endMs <= durationMs &&
                    cue.canvasWidth in 1..PGSOverlayPolicy.maximumCanvasWidth &&
                    cue.canvasHeight in 1..PGSOverlayPolicy.maximumCanvasHeight &&
                    cue.objects.size <= PGSOverlayPolicy.maximumObjectsPerCue,
            ) { "invalid PGS overlay cue" }
            previousEnd = cue.endMs

            cue.objects.forEach { object_ ->
                require(
                    object_.x >= 0 &&
                        object_.y >= 0 &&
                        object_.width > 0 &&
                        object_.height > 0 &&
                        object_.x <= cue.canvasWidth &&
                        object_.y <= cue.canvasHeight &&
                        object_.width <= cue.canvasWidth - object_.x &&
                        object_.height <= cue.canvasHeight - object_.y &&
                        objectHash(object_.image) != null,
                ) { "invalid PGS overlay object" }
                val existing = dimensions[object_.image]
                require(existing == null || existing == (object_.width to object_.height)) {
                    "PGS overlay object dimensions changed"
                }
                if (existing == null) {
                    dimensions[object_.image] = object_.width to object_.height
                }
            }
        }
        return this
    }

    companion object {
        fun decode(text: String): SharedPGSManifest {
            require(text.toByteArray().size <= 1_048_576)
            val wire = Json.parseToJsonElement(text).jsonObject
            wire.strictString("file_id")
            decodeSharedReference(wire.getValue("reference"))
            return Json.decodeFromJsonElement(wire)
        }
    }

    fun objectHash(path: String): String? {
        val prefix = "overlay/$generation/objects/"
        if (!path.startsWith(prefix) || !path.endsWith(".png")) return null
        return path.removePrefix(prefix).removeSuffix(".png")
            .takeIf { Regex("[0-9a-f]{64}").matches(it) }
    }
}


@Serializable
internal data class SharedProgressBeat(
    val session_id: String, val sequence: Long, val position_ms: Long,
    val duration_ms: Long? = null, val watched: Boolean,
) {
    fun validate() {
        require(Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(session_id))
        require(sequence in 0..9_007_199_254_740_991L && position_ms in 0..9_007_199_254_740_991L)
        require(duration_ms == null || duration_ms in 0..9_007_199_254_740_991L)
    }
}
internal sealed interface SharedProgressResult {
    data object Acknowledged : SharedProgressResult
    data object PreviousBeatAcknowledged : SharedProgressResult
    data class ResyncRequired(val currentSequence: Long?) : SharedProgressResult
}
/** One watch-key order: uncertain sends retain the exact beat; conflicts require
 * a fresh authorized watch read and never renumber the discarded old beat. */
internal class SharedProgressOrder(initialSequence: Long) {
    var sequence = initialSequence; private set
    var pending: SharedProgressBeat? = null; private set
    var needsResync = false; private set
    init { require(sequence in 0..9_007_199_254_740_991L) }
    fun beat(sessionId: String, positionMs: Long, durationMs: Long?, watched: Boolean): SharedProgressBeat {
        require(!needsResync)
        pending?.let { require(it.session_id == sessionId); return it }
        require(sequence < 9_007_199_254_740_991L)
        val beat = SharedProgressBeat(sessionId, sequence + 1, positionMs, durationMs, watched)
        beat.validate(); sequence = beat.sequence; pending = beat; return beat
    }
    fun complete(beat: SharedProgressBeat, result: SharedProgressResult) {
        require(pending == beat)
        if (result is SharedProgressResult.ResyncRequired) {
            result.currentSequence?.let { require(it in 0..9_007_199_254_740_991L); sequence = maxOf(sequence, it) }
            needsResync = true
        }
        pending = null
    }
    fun resync(freshAuthorizedSequence: Long) {
        require(needsResync && freshAuthorizedSequence in 0..9_007_199_254_740_991L)
        sequence = maxOf(sequence, freshAuthorizedSequence); needsResync = false
    }
}

/** Initial Shared subject carries full strings/opaque B context, never a Local ID. */
internal data class SharedPlaybackSubject(val context: PlaybackFileContext, val title: String, val resumeMs: Long, val watchSequence: Long) {
    fun validate() {
        require(context.sessionId == null && title.toByteArray().size <= 4096)
        require(resumeMs in 0..9_007_199_254_740_991L && watchSequence in 0..9_007_199_254_740_991L)
        context.validateSharedReference(requireNotNull(context.reference), context.sourceFileId, requireNotNull(context.revision))
    }
}
/** Retains the raw original desired ask, not normalized delivered dimensions. */
internal class SharedPlaybackPlan(val subject: SharedPlaybackSubject, val decision: SharedDecision, val caps: DeviceCaps, val request: CreateSessionReq) {
    init {
        subject.validate(); decision.validated(subject.context)
        require(caps.v == 2 && "hls" in caps.transports && request.caps == caps)
        require(request.presentation == "vod" && request.intent == null && request.previous_session_id == null && request.control_sequence == null && request.reopen_reason == null)
        require(request.subtitle_burn == null && request.preserve_dolby_vision != true && request.hdr10 != true)
        require(decision.presentation.delivered_dynamic_range?.let { it == "sdr" } != false)
        require((request.start ?: 0.0) == subject.resumeMs.toDouble() / 1000)
        require(request.height?.let { it in 1..8192 } != false)
        require(if (decision.method == "transcode") request.copy != true else request.copy == true)
    }
}
