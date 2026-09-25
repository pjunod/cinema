package tv.plurx.app.livetv

import android.content.Context
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.KSerializer
import kotlinx.serialization.descriptors.SerialDescriptor
import kotlinx.serialization.encoding.Decoder
import kotlinx.serialization.encoding.Encoder
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonDecoder
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonEncoder
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.encodeToJsonElement
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.booleanOrNull
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import tv.plurx.app.data.DeveloperReadiness
import tv.plurx.app.data.Caps
import tv.plurx.app.data.DeviceCaps
import java.util.concurrent.TimeUnit

internal enum class TvLiveLayout(val storageValue: String, val label: String) {
    GuidePreview("guide_preview", "Preview"),
    GuideOverlay("guide_overlay", "Over picture"),
    ChannelBrowser("channel_browser", "Preview");

    /**
     * What the stored value renders as. `channel_browser` and `guide_preview`
     * only ever differed in which browse view they opened with; now that On
     * now is a list beside the picture in both, a third entry would be a
     * choice with no consequence. The case stays so an existing stored value
     * still decodes — and keeps its own raw string.
     */
    val presented: TvLiveLayout get() = if (this == ChannelBrowser) GuidePreview else this

    companion object {
        fun fromStorage(value: String?): TvLiveLayout = entries.firstOrNull {
            it.storageValue == value
        }?.presented ?: GuidePreview

        /** The entries the Layout menu offers. */
        val offered: List<TvLiveLayout> = listOf(GuidePreview, GuideOverlay)
    }
}

@Serializable(with = LiveTvSourceFormatSerializer::class)
data class LiveTvSourceFormat(
    val video_width: Int? = null,
    val video_height: Int? = null,
    val scan: String? = null,
    val audio_channels: Int? = null,
    val audio_layout: String? = null,
    val observed_at: Long,
) {
    val validVideoWidth: Int? get() = video_width?.takeIf { it in 1..16_384 }
    val validVideoHeight: Int? get() = video_height?.takeIf { it in 1..16_384 }
    val validScan: String? get() = scan?.takeIf { it == "progressive" || it == "interlaced" }
    val validAudioChannels: Int? get() = audio_channels?.takeIf { it in 1..32 }
    val validAudioLayout: String? get() = audio_layout?.trim()?.lowercase()?.takeIf { value ->
        value.isNotEmpty() && value.length <= 32 && value.all {
            it.isLetterOrDigit() || it in ".+_- "
        }
    }
    val valid: Boolean get() = observed_at > 0
}

/**
 * Decode each observation independently. A tuner or older owner sending one
 * malformed optional fact must not discard the channel or the other measured
 * facts that are still honest.
 */
internal object LiveTvSourceFormatSerializer : KSerializer<LiveTvSourceFormat> {
    override val descriptor: SerialDescriptor = JsonObject.serializer().descriptor

    override fun deserialize(decoder: Decoder): LiveTvSourceFormat {
        val input = decoder as? JsonDecoder ?: error("LiveTvSourceFormat is JSON-only")
        val objectValue = input.decodeJsonElement() as? JsonObject ?: return LiveTvSourceFormat(observed_at = 0)
        fun element(name: String): JsonElement? = objectValue[name]
        fun int(name: String): Int? = runCatching { element(name)?.jsonPrimitive?.intOrNull }.getOrNull()
        fun long(name: String): Long? = runCatching { element(name)?.jsonPrimitive?.longOrNull }.getOrNull()
        fun text(name: String): String? = runCatching { element(name)?.jsonPrimitive?.contentOrNull }.getOrNull()
        return LiveTvSourceFormat(
            video_width = int("video_width"),
            video_height = int("video_height"),
            scan = text("scan"),
            audio_channels = int("audio_channels"),
            audio_layout = text("audio_layout"),
            observed_at = long("observed_at") ?: 0,
        )
    }

    override fun serialize(encoder: Encoder, value: LiveTvSourceFormat) {
        val output = encoder as? JsonEncoder ?: error("LiveTvSourceFormat is JSON-only")
        output.encodeJsonElement(buildJsonObject {
            value.video_width?.let { put("video_width", it) }
            value.video_height?.let { put("video_height", it) }
            value.scan?.let { put("scan", it) }
            value.audio_channels?.let { put("audio_channels", it) }
            value.audio_layout?.let { put("audio_layout", it) }
            put("observed_at", value.observed_at)
        })
    }
}

@Serializable
data class LiveTvChannel(
    val id: String,
    val guide_number: String,
    val guide_name: String,
    val favorite: Boolean = false,
    val drm: Boolean = false,
    val support: String = "ready",
    val hd: Boolean? = null,
    val video_codec: String? = null,
    val audio_codec: String? = null,
    val source_format: LiveTvSourceFormat? = null,
) {
    val title: String get() = "$guide_number · $guide_name"
    val watchable: Boolean get() = !drm && support == "ready"
    val formatBadges: List<String> get() = buildList {
        pictureClass?.let(::add)
        listOfNotNull(video_codec, sourceAudioDescription).forEach { raw ->
            val badge = raw.trim().uppercase()
            if (badge.isNotEmpty() && badge !in this) add(badge)
        }
    }
    val sourceFormatDescription: String? get() = buildList {
        exactSourcePicture?.let(::add)
        video_codec?.trim()?.takeIf { it.isNotEmpty() }?.let { add("${it.uppercase()} video") }
        sourceAudioDescription?.let { add("$it audio") }
    }.takeIf { it.isNotEmpty() }?.joinToString(" · ")

    val measuredSource: LiveTvSourceFormat? get() = source_format?.takeIf { it.valid }
    val pictureClass: String? get() = measuredSource?.validVideoHeight?.let { height ->
        when {
            height > 2160 -> "4K+"
            height == 2160 -> "4K"
            height >= 720 -> "HD"
            else -> "SD"
        }
    } ?: hd?.let { if (it) "HD" else "SD" }
    val exactSourcePicture: String? get() {
        val source = measuredSource ?: return pictureClass
        val width = source.validVideoWidth ?: return pictureClass
        val height = source.validVideoHeight ?: return pictureClass
        val suffix = when (source.validScan) {
            "progressive" -> "p"
            "interlaced" -> "i"
            else -> ""
        }
        return "$width×$height$suffix"
    }
    val sourceAudioDescription: String? get() {
        val codec = audio_codec?.trim()?.uppercase()?.takeIf { it.isNotEmpty() }
        val source = measuredSource
        val layout = when (val raw = source?.validAudioLayout) {
            "mono" -> "Mono"
            "stereo" -> "Stereo"
            null -> source?.validAudioChannels?.let { "$it ch" }
            else -> raw
        }
        return listOfNotNull(codec, layout).takeIf { it.isNotEmpty() }?.joinToString(" ")
    }
}

@Serializable
data class LiveTvLineup(
    val channels: List<LiveTvChannel>,
    val freshness: String = "fresh",
    /**
     * The start protocols the ingress and its owner both carry. Absent on an
     * ingress older than this contract, which is not the same fact as an empty
     * list — hence nullable.
     */
    val protocols: List<Int>? = null,
)

/** The answer to `POST /api/v1/live-tv/starts/{id}/resume`. */
@Serializable
data class LiveTvResumeAnswer(val outcome: String, val session: LiveTvStarted? = null)

@Serializable data class LiveTvRational(val num: Int, val den: Int)
@Serializable data class LiveTvHlsFormat(val container: String, val video: String, val audio: String)
@Serializable data class LiveTvVideoLimit(
    val codec: String, val profile: String? = null, val max_width: Int,
    val max_height: Int, val max_frame_rate: LiveTvRational, val interlaced: Boolean = false,
)
@Serializable data class LiveTvAudioLimit(val codec: String, val max_channels: Int)
@Serializable data class LiveTvCompatibility(
    val failed_video: Boolean = false,
    val failed_audio: Boolean = false,
    val failed_container: Boolean = false,
)
@Serializable data class LiveTvPlaybackEnvelope(
    // Required on the wire; default-valued fields are omitted by Net.json.
    val v: Int,
    val caps: DeviceCaps,
    val hls_formats: List<LiveTvHlsFormat>,
    val video_limits: List<LiveTvVideoLimit>,
    val audio_limits: List<LiveTvAudioLimit>,
    val max_height: Int? = null,
    val max_bitrate_bps: Long? = null,
    val compatibility: LiveTvCompatibility? = null,
) {
    companion object {
        fun from(
            caps: DeviceCaps,
            compatibility: LiveTvCompatibility? = null,
            sink: tv.plurx.app.data.LiveSinkFacts = tv.plurx.app.data.LiveSinkFacts(),
        ): LiveTvPlaybackEnvelope {
            val liveAudio = caps.audio.filter { it in setOf("aac", "ac3", "eac3") }
            val formats = buildList {
                add(LiveTvHlsFormat("mpegts", "h264", "aac"))
                caps.video.forEach { video ->
                    // Media3's TS extractor reads HEVC as readily as fMP4 does,
                    // and MPEG-TS has no init file for late AC-3 to race, so a
                    // copied HEVC picture with copied AC-3 rides MPEG-TS.
                    val containers = if (video.codec == "hevc") listOf("fmp4", "mpegts") else listOf("mpegts")
                    containers.forEach { container ->
                        liveAudio.forEach { audio -> add(LiveTvHlsFormat(container, video.codec, audio)) }
                    }
                }
            }.distinct()
            val limits = caps.video.flatMap { video ->
                (video.profiles.map { it.lowercase() }.map { it as String? }.ifEmpty { listOf(null) }).map { profile ->
                    LiveTvVideoLimit(video.codec, profile, 3840, video.max_height ?: 2160,
                        LiveTvRational(60, 1), sink.deinterlaces)
                }
            }
            return LiveTvPlaybackEnvelope(
                v = 1,
                caps = caps,
                hls_formats = formats,
                video_limits = limits,
                audio_limits = liveAudio.map { LiveTvAudioLimit(it, if (it == "aac") sink.aacChannels else 8) },
                compatibility = compatibility,
            )
        }
    }
}

@Serializable data class LiveTvDeliveryOutput(
    val container: String,
    val video_codec: String,
    val audio_codec: String,
    val width: Int,
    val height: Int,
    val bit_depth: Int? = null,
    val frame_rate: LiveTvRational? = null,
    val hdr: String? = null,
    val audio_channels: Int,
)
@Serializable data class LiveTvDeliverySource(
    val field_order: String? = null,
)
@Serializable data class LiveTvDelivery(
    val output: LiveTvDeliveryOutput,
    val video_action: String,
    val audio_action: String,
    val packaging: String,
    val source: LiveTvDeliverySource? = null,
    val deinterlace: Boolean = false,
)

@Serializable
data class LiveTvStarted(
    val session_id: String,
    val channel: LiveTvChannel,
    val live: Boolean = false,
    val delivery: LiveTvDelivery? = null,
    val playlist_url: String,
)

@Serializable
data class LiveTvSignal(
    val strength_percent: Int? = null,
    val quality_percent: Int? = null,
    val symbol_quality_percent: Int? = null,
)

@Serializable
data class LiveTvStatus(
    val state: String,
    val channel: LiveTvChannel? = null,
    val owner_node_id: String? = null,
    val encoder: String? = null,
    val output_height: Int? = null,
    val signal: LiveTvSignal? = null,
    val delivery: LiveTvDelivery? = null,
)

@Serializable
data class LiveTvSettings(
    val playback_display_mode_match: Boolean = false,
    val library_channels_enabled: Boolean = false,
    val dvr_enabled: Boolean = false,
    val dvr_root: String = "",
    val dvr_tuner_reserve: Int = 0,
    val live_tv_enabled: Boolean = false,
    val live_tv_device_ipv4: String = "",
    val live_tv_owner_node_id: String = "",
    val live_tv_max_sessions: Int = 2,
    val live_tv_output_height: Int = 720,
    val live_tv_max_output_height: Int = 0,
    val live_tv_config_generation: Long = 0,
    val live_tv_transition_from_owner_node_id: String = "",
    val live_tv_transition_drain_before: Long = 0,
)

@Serializable
data class LiveTvReadiness(val ready: Boolean, val generation: Long, val checks: List<LiveTvCheck>)

@Serializable
data class LiveTvCheck(val id: String, val ready: Boolean, val message: String)

/**
 * The guide's own advisory enablement facts, for the Developer tab.
 *
 * Every field outside [checks] is a summary line; [checks] is the part that
 * matters and the part that is rendered generically, row by row, so an owner
 * that grows a row shows it on a client built before that row existed. Nothing
 * in here gates anything — `advisory` is the server saying so itself.
 */
@Serializable
data class LiveTvGuideReadiness(
    val advisory: Boolean = true,
    val source: String = "off",
    val guide_hours: Int = 0,
    val freshness: String = "unavailable",
    val age_seconds: Long = 0,
    val fetched_at: Long? = null,
    val refresh_error: String? = null,
    val matched_channels: Int = 0,
    val lineup_channels: Int = 0,
    val programmes: Int = 0,
    val refresh_interval_seconds: Long = 0,
    val checks: List<LiveTvCheck> = emptyList(),
)

/**
 * A typed failure. [retry] and [ownerDecided] are the two fields of the
 * server's typed envelope the start reducer dispatches on; both are absent on
 * a refusal minted outside the Live TV module, which is itself the fact that
 * matters.
 */
data class LiveTvWatchable(val channelId: String, val guideNumber: String) {
    val offer: String get() = "Watch $guideNumber instead"
}

class LiveTvFailure(
    val code: String,
    val retry: String? = null,
    val ownerDecided: Boolean? = null,
    /**
     * The HTTP status the envelope arrived with, or null when this failure was
     * minted locally. The start reducer needs it: "the ingress decided this
     * before an owner saw it" is a claim about a 4xx, and the same code at 5xx
     * is a server that got as far as trying.
     */
    val status: Int? = null,
    val watchable: List<LiveTvWatchable> = emptyList(),
) : Exception(liveTvMessage(code) + if (code == "tuner_capacity" && watchable.isNotEmpty()) {
    " " + watchable.joinToString(" · ") { it.offer } + "."
} else "")

/**
 * The copy this client has of its own, or null. Null is a real answer: it is
 * how [LiveTvStartReducer] tells a code it can draw from one it cannot, and a
 * code it cannot draw is a refusal that carries no verdict.
 */
internal fun liveTvKnownMessage(code: String): String? = when (code) {
    "live_tv_disabled" -> "Live TV is disabled. An administrator can enable it in Settings → Developer."
    "tuner_capacity" -> "All Live TV slots are busy. Stop another session and try again."
    "tuner_unavailable" -> "Every tuner is busy. Stop another session and try again."
    "owner_unavailable" -> "The tuner owner is unavailable. Check its network and cluster health."
    "no_answer" -> "The server did not answer. Press the channel again."
    "drm_unsupported" -> "DRM-protected channels are unsupported. Select an unprotected channel."
    "codec_unsupported" -> "This stream's audio or video cannot be decoded by the server or this device. Try an ATSC 1.0 channel."
    "capability_expired" -> "The live session expired. Select the channel again."
    "admin_required" -> "An administrator account is required to configure Live TV."
    "settings_conflict" -> "Settings changed on another client. Reload before editing."
    "channel_not_found" -> "This channel is no longer in the saved lineup. Reload channels."
    "startup_timeout" -> "The tuner did not produce playable media in time."
    "source_format_changed" -> "The broadcast changed format. plurx will select a fresh compatible route."
    "invalid_request" -> "This build sent a start request the server refused. Update plurx."
    "invalid_settings" -> "Check the private IPv4 address, voter node ID, session budget and output height."
    else -> null
}

internal fun liveTvMessage(code: String): String = liveTvKnownMessage(code)
    ?: "The live stream could not continue. Stop and select a channel again."

/**
 * A non-2xx response, as a typed failure.
 *
 * The server's envelope is flat: `code` and `message`, plus the optional
 * `retry` and `owner_decided` that [LiveTvStartReducer] dispatches on. A body
 * with no `code` is not a refusal anyone minted — for a start it is no answer
 * at all, which is the one thing the ninety-second unknown-outcome refusal this
 * replaced could never say.
 *
 * Separate from the call so `LiveTvStartCasesTest` can drive it from the bodies
 * in `tests/playback/live-tv-start-cases.json` rather than from a description
 * of them.
 */
internal fun liveTvTypedFailure(body: String, status: Int, starting: Boolean): LiveTvFailure {
    val envelope = runCatching { Net.json.decodeFromString<JsonObject>(body) }.getOrNull()
    fun field(name: String) = runCatching { envelope?.get(name)?.jsonPrimitive }.getOrNull()
    val code = field("code")?.contentOrNull
    return LiveTvFailure(
        code = code ?: when {
            status in setOf(401, 403) -> "admin_required"
            starting -> "no_answer"
            else -> "stream_failed"
        },
        // Only ever read off a body that actually typed itself: a `retry` beside
        // no `code` is not this contract's envelope.
        retry = if (code == null) null else field("retry")?.contentOrNull,
        ownerDecided = if (code == null) null else field("owner_decided")?.booleanOrNull,
        status = status,
        watchable = if (code == "tuner_capacity") {
            runCatching { envelope?.get("watchable")?.jsonArray }.getOrNull()
                ?.mapNotNull { entry ->
                    val row = entry as? JsonObject ?: return@mapNotNull null
                    val id = runCatching { row["channel_id"]?.jsonPrimitive?.contentOrNull }.getOrNull()
                    val number = runCatching { row["guide_number"]?.jsonPrimitive?.contentOrNull }.getOrNull()
                    if (id.isNullOrBlank() || number.isNullOrBlank()) null else LiveTvWatchable(id, number)
                }?.distinctBy { it.channelId } ?: emptyList()
        } else emptyList(),
    )
}

sealed interface LiveTvSettingsChange {
    data class Configure(
        val ipv4: String, val owner: String, val sessions: Int, val height: Int,
        val maxHeight: Int = 0,
    ) : LiveTvSettingsChange
    data class Enabled(val enabled: Boolean) : LiveTvSettingsChange
    data class LibraryChannelsEnabled(val enabled: Boolean) : LiveTvSettingsChange
    data class DvrEnabled(val enabled: Boolean) : LiveTvSettingsChange
    data class DisplayModeMatch(val enabled: Boolean) : LiveTvSettingsChange
    data class FencedOwner(val owner: String, val cutoff: Long) : LiveTvSettingsChange

    fun body(generation: Long): JsonObject = buildJsonObject {
        // The display-mode preference is an ordinary playback setting, not
        // part of the replicated Live TV owner tuple. Sending the tuple's CAS
        // with it is rejected by the server and would falsely make this
        // advisory switch depend on unrelated tuner configuration churn.
        if (this@LiveTvSettingsChange !is DisplayModeMatch) {
            put("live_tv_config_generation", generation)
        }
        when (val change = this@LiveTvSettingsChange) {
            is Configure -> {
                put("live_tv_device_ipv4", change.ipv4)
                put("live_tv_owner_node_id", change.owner)
                put("live_tv_max_sessions", change.sessions)
                put("live_tv_output_height", change.height)
                put("live_tv_max_output_height", change.maxHeight)
            }
            is Enabled -> put("live_tv_enabled", change.enabled)
            is LibraryChannelsEnabled -> put("library_channels_enabled", change.enabled)
            is DvrEnabled -> put("dvr_enabled", change.enabled)
            is DisplayModeMatch -> put("playback_display_mode_match", change.enabled)
            is FencedOwner -> putJsonObject("live_tv_fenced_owner") {
                put("owner_node_id", change.owner)
                put("drain_before_generation", change.cutoff)
                put("stopped_and_restart_prevented", true)
            }
        }
    }
}

interface LiveTvRequests {
    /**
     * [requestId] is sent only when the last channels answer listed protocol 3;
     * the implementation drops it otherwise, so the lease always mints one and
     * always persists its hint.
     */
    suspend fun start(channel: String, requestId: String): LiveTvStarted
    suspend fun release(capability: String)

    /** `DELETE /api/v1/live-tv/starts/{id}`. Idempotent by the owner's design. */
    suspend fun retire(requestId: String)

    /** `POST /api/v1/live-tv/starts/{id}/resume`. */
    suspend fun resume(requestId: String): LiveTvResumeAnswer

    /** Whether the last channels answer listed protocol 3's recovery routes. */
    val recoveryRoutes: Boolean
}

/** Immutable profile-bound API. Narrow capabilities never inherit Session.token. */
class LiveTvApi(origin: String, private val token: String, context: Context? = null) : LiveTvRequests {
    private companion object {
        /** Every lineup-shaped read. A device document is far smaller. */
        const val MAX_BODY_BYTES: Long = 1_048_576

        /**
         * The guide's ceiling is the server's own `MAX_GUIDE_RESPONSE_BYTES`,
         * plus room for the envelope. Reusing the lineup's 1 MiB would refuse
         * a large lineup's guide as a transport failure, on a limit that has
         * nothing to do with what went wrong.
         */
        const val MAX_GUIDE_BYTES: Long = 2 * 1_048_576 + 8_192
    }

    private val base = (Session.canonicalOrigin(origin) ?: throw LiveTvFailure("invalid_settings")).toHttpUrl()
    private val capabilityContext = context?.applicationContext
    @Volatile private var nextCompatibility: LiveTvCompatibility? = null

    /**
     * What the last `GET /live-tv/channels` said both ends carry. Legacy until
     * a lineup is read: an ingress older than this contract omits `protocols`
     * entirely, would answer 400 to a `request_id` it does not know, and has no
     * `/live-tv/starts/…` routes to call.
     */
    @Volatile private var protocols: LiveTvProtocolSupport = LiveTvProtocolSupport.legacy
    override val recoveryRoutes: Boolean get() = protocols.recoveryRoutes
    private val client: OkHttpClient = Net.capabilityClient.newBuilder()
        .connectTimeout(8, TimeUnit.SECONDS).readTimeout(45, TimeUnit.SECONDS)
        .callTimeout(45, TimeUnit.SECONDS).build()
    val mediaClient: OkHttpClient = Net.capabilityClient.newBuilder()
        .connectTimeout(8, TimeUnit.SECONDS).readTimeout(15, TimeUnit.SECONDS)
        .callTimeout(20, TimeUnit.SECONDS).build()

    private fun url(vararg parts: String): HttpUrl = base.newBuilder()
        .addPathSegments("api/v1").apply { parts.forEach(::addPathSegment) }.build()

    internal fun playlistUrl(capability: String): String {
        if (capability.isEmpty() || capability.length > 1024) throw LiveTvFailure("no_answer")
        return url("live-tv", "sessions", capability, "index.m3u8").toString()
    }

    /**
     * The activation chooses the playable playlist. M4 returns a master
     * playlist with caption renditions. A legacy resume may still return the
     * media playlist; promote it to the same capability's master so resumed
     * playback retains captions. Keep it on this origin and exact session path.
     */
    internal fun playbackUrl(started: LiveTvStarted): String {
        val capability = started.session_id
        if (capability.isEmpty() || capability.length > 1024) throw LiveTvFailure("no_answer")
        val master = url("live-tv", "sessions", capability, "master.m3u8")
        val index = url("live-tv", "sessions", capability, "index.m3u8")
        return when (started.playlist_url) {
            master.encodedPath -> master.toString()
            index.encodedPath -> master.toString()
            else -> throw LiveTvFailure("no_answer")
        }
    }

    /** Probe the media playlist for liveness before rewinding a live decoder. */
    internal suspend fun playlistIsLive(capability: String): Boolean = withContext(Dispatchers.IO) {
        runCatching {
            val probe = Request.Builder().url(playlistUrl(capability)).head().build()
            mediaClient.newCall(probe).execute().use { it.code == 200 }
        }.getOrDefault(false)
    }

    private suspend fun request(
        target: HttpUrl, method: String = "GET", authenticated: Boolean = false,
        body: JsonObject? = null, timeout: Long = 45, starting: Boolean = false,
        maxBytes: Long = MAX_BODY_BYTES,
    ): String = withContext(Dispatchers.IO) {
        try {
            val payload = if (method in setOf("POST", "PUT")) {
                (body?.let { Net.json.encodeToString(it) } ?: "{}").toRequestBody("application/json".toMediaType())
            } else null
            val request = Request.Builder().url(target).method(method, payload)
                .apply { if (authenticated) header("Authorization", "Bearer $token") }.build()
            val call = client.newCall(request)
            call.timeout().timeout(timeout, TimeUnit.SECONDS)
            call.execute().use { response ->
                if (method == "DELETE" && response.code in setOf(404, 410)) return@withContext ""
                val source = response.body?.source()
                if (source?.request(maxBytes + 1) == true) throw LiveTvFailure(if (starting) "no_answer" else "stream_failed")
                val text = source?.readUtf8().orEmpty()
                if (!response.isSuccessful) throw liveTvTypedFailure(text, response.code, starting)
                text
            }
        } catch (error: LiveTvFailure) {
            throw error
        } catch (cancelled: CancellationException) {
            // Cancellation is this coroutine being told to stop, not the tuner
            // failing. Converting it into a typed failure breaks structured
            // concurrency and would let a cancelled start be reported as an
            // ambiguous outcome, arming the barrier for a request that never
            // reached the owner.
            throw cancelled
        } catch (_: Exception) {
            // Never surface a transport exception containing a capability URL.
            throw LiveTvFailure(if (starting) "no_answer" else "stream_failed")
        }
    }

    /**
     * The lineup, and the protocol negotiation that rides on it. An answer with
     * no `protocols` field is an ingress older than this contract: no
     * `request_id` on the start body, and no `/live-tv/starts/…` routes. Hints
     * are still written — the ingress this device talks to tomorrow may carry
     * both halves.
     */
    suspend fun lineup(): LiveTvLineup =
        Net.json.decodeFromString<LiveTvLineup>(request(url("live-tv", "channels"), authenticated = true))
            .also { protocols = LiveTvProtocolSupport.from(it.protocols) }
    fun retryCompatibility(compatibility: LiveTvCompatibility) {
        nextCompatibility = compatibility
    }

    override suspend fun start(channel: String, requestId: String): LiveTvStarted = try {
        val playback = capabilityContext?.let { current ->
            val compatibility = nextCompatibility
            nextCompatibility = null
            Net.json.encodeToJsonElement(
                LiveTvPlaybackEnvelope.from(
                    Caps.snapshot(current).document,
                    compatibility,
                    Caps.liveSinkFacts(current),
                )
            )
        }
        val body = buildJsonObject {
            playback?.let { put("playback", it) }
            // Guardrail §4.2 in the client's direction: an ingress that did not
            // publish protocol 3 would answer 400 to this field and take the
            // whole start down.
            if (protocols.requestId) put("request_id", requestId)
        }.takeIf { it.isNotEmpty() }
        Net.json.decodeFromString(request(url("live-tv", "channels", channel, "sessions"), "POST",
            authenticated = true, body = body, starting = true))
    } catch (error: LiveTvFailure) { throw error }
    catch (cancelled: CancellationException) { throw cancelled }
    catch (_: Exception) { throw LiveTvFailure("no_answer") }

    /**
     * Stop whatever a request id produced and fence it. Fire and forget from
     * the lease: the press never waits for it, so the timeout is short.
     */
    override suspend fun retire(requestId: String) {
        request(url("live-tv", "starts", requestId), "DELETE", authenticated = true, timeout = 8)
    }

    /**
     * `starting = true` because the question a resume asks is the same one a
     * start asks: an answer that never arrived, or arrived untyped, says
     * nothing about the id — so it must classify as no answer rather than as a
     * refusal, and the hint must survive it.
     */
    override suspend fun resume(requestId: String): LiveTvResumeAnswer = Net.json.decodeFromString(
        request(
            url("live-tv", "starts", requestId, "resume"), "POST",
            authenticated = true, timeout = 20, starting = true,
        ),
    )

    override suspend fun release(capability: String) {
        repeat(2) { attempt ->
            try {
                request(url("live-tv", "sessions", capability), "DELETE", timeout = 8)
                return
            } catch (error: LiveTvFailure) { if (attempt == 1) throw error }
        }
    }
    suspend fun keepalive(capability: String) { request(url("live-tv", "sessions", capability, "keepalive"), "PUT", timeout = 15) }
    suspend fun status(capability: String): LiveTvStatus = Net.json.decodeFromString(request(url("live-tv", "sessions", capability, "status"), timeout = 15))
    /**
     * The programme guide. A read of the owner's cache: it never triggers a
     * fetch, so it is always fast and always answers — including with
     * `freshness: "unavailable"`, which the client draws rather than retries.
     * Its ceiling is the server's own response cap, not the lineup's.
     */
    suspend fun guide(from: Long? = null, hours: Int? = null): LiveTvGuide = Net.json.decodeFromString(
        request(
            url("live-tv", "guide").newBuilder().apply {
                from?.let { addQueryParameter("from", it.toString()) }
                hours?.let { addQueryParameter("hours", it.coerceIn(1, 72).toString()) }
            }.build(),
            authenticated = true,
            timeout = 20,
            maxBytes = MAX_GUIDE_BYTES,
        ),
    )

    suspend fun settings(): LiveTvSettings = Net.json.decodeFromString(request(url("settings"), authenticated = true))
    suspend fun save(settings: LiveTvSettings, change: LiveTvSettingsChange): LiveTvSettings = Net.json.decodeFromString(
        request(url("settings"), "PUT", authenticated = true, body = change.body(settings.live_tv_config_generation)),
    )
    suspend fun readiness(): LiveTvReadiness = Net.json.decodeFromString(request(url("live-tv", "readiness", "refresh"), "POST", authenticated = true))

    /**
     * The guide's advisory rows. A read, never a refresh: it answers with
     * whatever the owner's cache already knows, so opening the Developer tab
     * cannot itself become a device or network request.
     */
    suspend fun guideReadiness(): LiveTvGuideReadiness = Net.json.decodeFromString(
        request(url("live-tv", "guide", "readiness"), authenticated = true, timeout = 20),
    )
    suspend fun developerReadiness(): DeveloperReadiness = Net.json.decodeFromString(
        request(url("developer", "readiness"), authenticated = true),
    )
}
