package tv.plurx.app.player

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonClassDiscriminator

/**
 * Android's passive playback-control reporter.
 *
 * M2's contract is one coalescing controller per platform that reports what
 * the player is actually doing — intent, film position, contiguous runway,
 * render state, selection, and throughput evidence — and consumes no action.
 * `crates/plurxd/src/web/playback-control.js` is the reference implementation
 * and `clients/apple/Sources/PlaybackControlReporter.swift` is its Apple
 * port; this is the third, and the three are meant to stay observably
 * identical.
 *
 * Passive means passive: this reporter declares the actions it will accept and
 * treats anything else as a protocol error that stops it, rather than as an
 * instruction to improvise. Today that vocabulary is `hold`, which is an
 * explanation and not an instruction — the server saying production is
 * deliberately not advancing. Accepting it is what keeps this client reporting
 * through a deliberate hold; acting on it is the action owner's job, and
 * recovery authority still belongs to this platform's own timers until M5
 * moves it.
 */
object PlaybackControl {
    const val PROTOCOL = "plurx-playback-control-v1"

    /**
     * The name a client *declares* to be offered a prepared replacement, and
     * the tag that then arrives as `action.type`. They are deliberately
     * different strings and the difference is silent both ways: declaring
     * `"prepare"` is never matched by the server's literal `accepts()`
     * comparison and is never offered anything, while switching on
     * `"prepare_replacement"` as an action type never fires. The transaction is
     * named for the transaction; the tag is named for the moment.
     *
     * `crates/plurxd/src/playback_control.rs:53` and its
     * `ControlAction::vocabulary_name()` at `:1730` are the two sides.
     *
     * [SUPPORTED_ACTIONS] spells these literally rather than referring to the
     * constants: `tests/validation/test_control_wire_conformance.py` reads that
     * list out of the source of all four ports and cannot resolve a symbol, and
     * a vocabulary the cross-client check cannot read is a vocabulary nothing
     * pins. A test asserts the two spellings agree.
     */
    const val PREPARE_REPLACEMENT_ACTION = "prepare_replacement"
    const val PREPARE_ACTION_TYPE = "prepare"

    /**
     * The actions this client will accept, and therefore the only ones the
     * server will send it. An action that is never declared is never sent, so
     * a client cannot be silenced by one it does not understand.
     *
     * Declaring `prepare_replacement` costs nothing on a client the server will
     * never stage a successor for: it makes `can_settle_preparation` true, so
     * the server performs a quorum store read for a staged generation on
     * exchanges that would not otherwise have done one — sessions still on
     * `owner_epoch == 1`, carrying no acknowledgement, not ending — and that
     * read always answers `Absent` while `dual_player_preparation` is false.
     */
    val SUPPORTED_ACTIONS =
        listOf("hold", "retry_resource", "terminal", "prepare_replacement")
    const val MIN_EXCHANGE_MS = 250L
    const val MAX_EXCHANGE_MS = 60_000L
    const val EXCHANGE_DEADLINE_MS = 6_000L
    const val MAX_LEASE_TIMEOUT_MS = 600_000L
    const val MIN_HEIGHT = 144
    const val MAX_HEIGHT = 2_160
    const val MAX_CAPABILITY_VALUES = 8
    const val MAX_OBSERVED_DOWNLOAD_BPS = 10_000_000_000_000L
    const val MAX_ERROR_DETAIL_BYTES = 512

    /**
     * The server's own bounds, mirrored so a malformed `prepare` is refused
     * here rather than acted on. Every one is copied from
     * `crates/plurxd/src/playback_control.rs` — `MAX_MEDIA_MILLIS` at `:34`,
     * `MAX_ACTION_ID_LEN` at `:56`, `MAX_PLAYLIST_URL_LEN` at `:58` — and
     * [MAX_HEIGHT] above is already `crate::transcode::MAX_HEIGHT`.
     */
    const val MAX_MEDIA_MILLIS = 366L * 24 * 60 * 60 * 1_000
    const val MAX_ACTION_ID_LEN = 64
    const val MAX_PLAYLIST_URL_LEN = 512
    const val MAX_TRACK_INDEX = 1_024L
    const val MAX_AUDIO_OFFSET_MS = 15_000L

    /**
     * `effective_selection.codec` is not a codec name. Two values, and they
     * distinguish direct-play/remux from transcode — the server's
     * `PreparationAxis::DeliveryMethod`. Rendering this string to a viewer as a
     * codec would be wrong.
     */
    val DELIVERY_METHODS = setOf("source", "server_selected")
    val DYNAMIC_RANGES = setOf("dolby_vision", "hdr10", "hlg", "sdr")

    /**
     * `/api/v1/hls/{session}/index.m3u8` or `…/master.m3u8` and nothing else,
     * with the session segment being the successor's own id.
     *
     * `is_node_relative_playlist` (`playback_control.rs:1759`) is the server's
     * copy of this rule, and the relay path already refuses to point a client
     * anywhere. So does this: an absolute URL is refused before any request,
     * even if a future server sends one.
     */
    fun isNodeRelativePlaylist(playlistUrl: String, sessionId: String): Boolean {
        if (playlistUrl.length > MAX_PLAYLIST_URL_LEN) return false
        val path = playlistUrl.split('?', '#').first()
        return path == "/api/v1/hls/$sessionId/index.m3u8" ||
            path == "/api/v1/hls/$sessionId/master.m3u8"
    }

    private val UUID_RE = Regex(
        "^[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$",
        RegexOption.IGNORE_CASE,
    )

    /**
     * The server sends a UUID; a client that accepts anything else would let a
     * redirected or spoofed bootstrap rebind the session. `UUID.fromString` is
     * deliberately not used here — it accepts short groups, and widening this
     * check is exactly the mistake it exists to prevent.
     */
    fun isUuid(value: String): Boolean = UUID_RE.matches(value)

    private val UUID_SHAPE_RE = Regex(
        "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
        RegexOption.IGNORE_CASE,
    )

    /**
     * The hyphenated shape, without pinning a version or a variant.
     *
     * Deliberately laxer than [isUuid], and only for the two identifiers inside
     * a `prepare`. [isUuid] guards the *bootstrap*, where a loose check would
     * let a redirected or spoofed generation rebind the session — that is a
     * credential. `action_id` and `session_id` are not: the action is fatal if
     * malformed, the playlist is separately checked against `session_id`
     * character for character, and the server has already run
     * `Uuid::parse_str` over both, which accepts v6 and v7 where the strict
     * regex does not. Refusing an id this client merely does not recognise the
     * version of would kill the whole control plane for the rest of the film —
     * the "stricter than the server" failure — and a shape check is what the
     * fields are actually load-bearing for.
     */
    fun isUuidShape(value: String): Boolean = UUID_SHAPE_RE.matches(value)
}

// ---------------------------------------------------------------- wire types

@Serializable
data class ControlBootstrap(
    val protocol: String,
    val url: String,
    val generation: String,
    @SerialName("control_epoch") val controlEpoch: Long,
    @SerialName("next_exchange_ms") val nextExchangeMs: Long,
    @SerialName("lease_timeout_ms") val leaseTimeoutMs: Long,
) {
    /**
     * The same acceptance the web reporter applies before it will speak at
     * all. An older or foreign server that omits a field leaves the client
     * silent rather than reporting into a session it cannot address.
     */
    val isValid: Boolean
        get() = protocol == PlaybackControl.PROTOCOL &&
            isSessionControlPath(url) &&
            PlaybackControl.isUuid(generation) &&
            controlEpoch > 0 &&
            nextExchangeMs >= PlaybackControl.MIN_EXCHANGE_MS &&
            nextExchangeMs <= PlaybackControl.MAX_EXCHANGE_MS &&
            leaseTimeoutMs >= nextExchangeMs &&
            leaseTimeoutMs <= PlaybackControl.MAX_LEASE_TIMEOUT_MS

    companion object {
        /**
         * `/api/v1/hls/{session}/control` and nothing else. A server-relative
         * path with exactly one session segment cannot be steered to another
         * origin, another route, or a traversal.
         */
        fun isSessionControlPath(url: String): Boolean {
            val parts = url.split("/")
            return parts.size == 6 &&
                parts[0].isEmpty() &&
                parts[1] == "api" &&
                parts[2] == "v1" &&
                parts[3] == "hls" &&
                parts[4].isNotEmpty() &&
                !parts[4].contains(".") &&
                parts[5] == "control"
        }
    }
}

@Serializable
enum class PlaybackDemand {
    @SerialName("active") ACTIVE,
    @SerialName("hold") HOLD,
    @SerialName("end") END,
}

@Serializable
enum class RenderState {
    @SerialName("starting") STARTING,
    @SerialName("rendering") RENDERING,
    @SerialName("waiting") WAITING,
    @SerialName("stalled") STALLED,
    @SerialName("seeking") SEEKING,
    @SerialName("ended") ENDED,
    @SerialName("failed") FAILED,
}

@Serializable
enum class CodecPolicy {
    @SerialName("auto") AUTO,
    @SerialName("h264") H264,
    @SerialName("hevc") HEVC,
    @SerialName("av1") AV1,
}

@Serializable
enum class DynamicRangePolicy {
    @SerialName("auto") AUTO,
    @SerialName("dolby_vision") DOLBY_VISION,
    @SerialName("hdr10") HDR10,
    @SerialName("hlg") HLG,
    @SerialName("sdr") SDR,
}

@Serializable
enum class SubtitleMode {
    @SerialName("off") OFF,
    @SerialName("native") NATIVE,
    @SerialName("overlay") OVERLAY,
    @SerialName("burn") BURN,
}

@Serializable
enum class DecoderState {
    @SerialName("unknown") UNKNOWN,
    @SerialName("ready") READY,
    @SerialName("starved") STARVED,
    @SerialName("failed") FAILED,
}

@Serializable
enum class ClientErrorCode {
    @SerialName("network") NETWORK,
    @SerialName("manifest") MANIFEST,
    @SerialName("media") MEDIA,
    @SerialName("decoder") DECODER,
    @SerialName("drm") DRM,
    @SerialName("unknown") UNKNOWN,
}

/**
 * `{"mode":"auto"}` or `{"mode":"manual","height":1080}` — the server tags
 * this one internally on `mode`, not on the shared `type` discriminator.
 */
@OptIn(ExperimentalSerializationApi::class)
@Serializable
@JsonClassDiscriminator("mode")
sealed class QualitySelection {
    @Serializable
    @SerialName("auto")
    data object Auto : QualitySelection()

    @Serializable
    @SerialName("manual")
    data class Manual(val height: Int) : QualitySelection()

    val isValid: Boolean
        get() = this !is Manual || height in PlaybackControl.MIN_HEIGHT..PlaybackControl.MAX_HEIGHT
}

@Serializable
data class SubtitleSelection(
    val mode: SubtitleMode,
    val track: Int? = null,
) {
    val isValid: Boolean
        get() = (track == null || track in 0..1_024) && !(mode == SubtitleMode.OFF && track != null)
}

@Serializable
data class ClientSelection(
    val quality: QualitySelection,
    @SerialName("audio_track") val audioTrack: Int? = null,
    val subtitle: SubtitleSelection,
    @SerialName("audio_offset_ms") val audioOffsetMs: Long,
    val codec: CodecPolicy,
    @SerialName("dynamic_range") val dynamicRange: DynamicRangePolicy,
) {
    val isValid: Boolean
        get() = (audioTrack == null || audioTrack in 0..1_024) &&
            quality.isValid &&
            subtitle.isValid
}

@Serializable
data class DynamicCapabilities(
    val platform: String,
    @SerialName("max_height") val maxHeight: Int,
    val codecs: List<CodecPolicy>,
    @SerialName("dynamic_ranges") val dynamicRanges: List<DynamicRangePolicy>,
    @SerialName("dual_player_preparation") val dualPlayerPreparation: Boolean,
) {
    val isValid: Boolean
        get() = maxHeight in PlaybackControl.MIN_HEIGHT..PlaybackControl.MAX_HEIGHT &&
            codecs.isNotEmpty() &&
            codecs.size <= PlaybackControl.MAX_CAPABILITY_VALUES &&
            dynamicRanges.isNotEmpty() &&
            dynamicRanges.size <= PlaybackControl.MAX_CAPABILITY_VALUES
}

/**
 * What a session actually delivers, as the server answers it.
 *
 * It appears twice on the wire: on the response, describing the session that is
 * playing now, and inside a `prepare` action, describing the successor. The
 * difference between the two is what is about to change.
 *
 * `crates/plurxd/src/playback_control.rs:861-891`.
 */
@Serializable
data class EffectiveSelection(
    // No defaults on the four the server declares as plain fields. It carries
    // `deny_unknown_fields` and no `Option` on these, so a payload omitting one
    // is a serde error there — and a Kotlin default here would turn that
    // refusal into a silent `height = 0` that passes validation and gets acted
    // on. Only the three genuine `Option`s below may be absent.
    @SerialName("quality_auto") val qualityAuto: Boolean,
    val height: Long,
    @SerialName("audio_track") val audioTrack: Long? = null,
    @SerialName("subtitle_burn") val subtitleBurn: Long? = null,
    @SerialName("audio_offset_ms") val audioOffsetMs: Long,
    /** `source` or `server_selected` — a delivery method, not a codec name. */
    val codec: String,
    @SerialName("dynamic_range") val dynamicRange: String? = null,
) {
    /** `EffectiveSelection::is_valid`, boundary for boundary. */
    val isValid: Boolean
        // `MAX_HEIGHT` is 2160 here and `crate::transcode::MAX_HEIGHT` is 2160
        // on the server (verified against `transcode.rs:256`). They are two
        // literals that must agree; if the server's ever rises, a legitimate
        // `prepare` at the new height would be refused here and the refusal is
        // fatal to the control plane, so this is the pair to move together.
        get() = height in 0..PlaybackControl.MAX_HEIGHT.toLong() &&
            (audioTrack == null || audioTrack in 0..PlaybackControl.MAX_TRACK_INDEX) &&
            (subtitleBurn == null || subtitleBurn in 0..PlaybackControl.MAX_TRACK_INDEX) &&
            audioOffsetMs in -PlaybackControl.MAX_AUDIO_OFFSET_MS..PlaybackControl.MAX_AUDIO_OFFSET_MS &&
            codec in PlaybackControl.DELIVERY_METHODS &&
            (dynamicRange == null || dynamicRange in PlaybackControl.DYNAMIC_RANGES)
}

/**
 * How far a prepared replacement got, named by the transaction it belongs to.
 *
 * Exactly five states, and a sixth is a serde error on the server — a `400`
 * with no `invalid_field`, because it never reaches the semantic validator.
 * `crates/plurxd/src/playback_control.rs:490-534`.
 */
@Serializable
enum class AcknowledgementState {
    @SerialName("metadata_ready") METADATA_READY,
    @SerialName("buffer_ready") BUFFER_READY,
    @SerialName("committed") COMMITTED,
    @SerialName("failed") FAILED,
    @SerialName("aborted") ABORTED,
}

@Serializable
data class ActionAcknowledgement(
    @SerialName("action_id") val actionId: String,
    val state: AcknowledgementState,
    @SerialName("buffered_through_ms") val bufferedThroughMs: Long? = null,
    @SerialName("first_frame_unix_ms") val firstFrameUnixMs: Long? = null,
) {
    /**
     * The server's own acceptance, applied before the request is built rather
     * than discovered as a `400`. `buffer_ready` requires a runway and
     * `committed` requires the wall clock of a frame that actually rendered;
     * an acknowledgement missing either is worse than none, because the
     * exchange it rides on is refused whole.
     */
    val isValid: Boolean
        get() = PlaybackControl.isUuidShape(actionId) &&
            actionId.length <= PlaybackControl.MAX_ACTION_ID_LEN &&
            (
                bufferedThroughMs == null ||
                    bufferedThroughMs in 0..PlaybackControl.MAX_MEDIA_MILLIS
                ) &&
            (firstFrameUnixMs == null || firstFrameUnixMs > 0) &&
            (state != AcknowledgementState.BUFFER_READY || bufferedThroughMs != null) &&
            (state != AcknowledgementState.COMMITTED || firstFrameUnixMs != null)
}

@Serializable
data class ClientObservation(
    @SerialName("dropped_frames") val droppedFrames: Long? = null,
    @SerialName("decoder_state") val decoderState: DecoderState? = null,
    @SerialName("error_code") val errorCode: ClientErrorCode? = null,
    @SerialName("error_detail") val errorDetail: String? = null,
) {
    val isEmpty: Boolean
        get() = droppedFrames == null && decoderState == null &&
            errorCode == null && errorDetail == null

    /**
     * Detail is free text from a player error, so it is bounded and stripped
     * of the control characters that would let it forge a log line. Detail
     * without a code is dropped entirely: the server rejects that pairing.
     */
    fun bounded(): ClientObservation? {
        val frames = droppedFrames?.takeIf { it >= 0 }
        var detail: String? = null
        if (errorCode != null && errorDetail != null) {
            val builder = StringBuilder()
            var bytes = 0
            for (character in errorDetail) {
                val replacement = when (character) {
                    '\r', '\n', '\u0000' -> ' '
                    else -> character
                }
                val width = replacement.toString().toByteArray(Charsets.UTF_8).size
                if (bytes + width > PlaybackControl.MAX_ERROR_DETAIL_BYTES) break
                builder.append(replacement)
                bytes += width
            }
            detail = builder.toString().takeIf { it.isNotEmpty() }
        }
        val value = ClientObservation(frames, decoderState, errorCode, detail)
        return if (value.isEmpty) null else value
    }
}

/**
 * What the player is doing right now. The reporter reads one of these each
 * time it is ready to speak, so a snapshot is always the newest truth rather
 * than a queued history.
 */
data class PlaybackControlSnapshot(
    val demand: PlaybackDemand,
    val positionMs: Long,
    val bufferedFromMs: Long? = null,
    val bufferedThroughMs: Long,
    val playbackRate: Double,
    val renderState: RenderState,
    val seekTargetMs: Long? = null,
    val observedDownloadBps: Long? = null,
    val selection: ClientSelection,
    val capabilities: DynamicCapabilities,
    val observation: ClientObservation? = null,
    /**
     * The prepared replacement's progress, when there is one to report.
     *
     * It rides the ordinary snapshot rather than a channel of its own because
     * the protocol has no channel of its own: an acknowledgement is a field on
     * the next request, and there is at most one request in flight.
     */
    val acknowledgement: ActionAcknowledgement? = null,
) {
    val isValid: Boolean
        get() = positionMs >= 0 &&
            bufferedThroughMs >= positionMs &&
            playbackRate.isFinite() &&
            playbackRate >= 0 &&
            (
                bufferedFromMs == null ||
                    (bufferedFromMs >= 0 && bufferedFromMs <= bufferedThroughMs)
                ) &&
            (seekTargetMs == null || seekTargetMs >= 0) &&
            (
                observedDownloadBps == null ||
                    observedDownloadBps in 0..PlaybackControl.MAX_OBSERVED_DOWNLOAD_BPS
                ) &&
            selection.isValid &&
            capabilities.isValid &&
            (acknowledgement == null || acknowledgement.isValid)

    /**
     * The acknowledgement this snapshot may actually carry.
     *
     * You may not commit a replacement on the same exchange that ends the
     * session (`playback_control.rs:253-259`), and the pairing is a `400` that
     * costs the `end` as well as the commit. Dropping the commit rather than
     * the whole snapshot is the cheaper loss: the session still ends on time
     * and the unsettled preparation is reaped at its 330 s deadline, whereas a
     * dropped `end` strands the session until its lease expires.
     *
     * The Controller sends `committed` urgently at the switch, well before any
     * teardown, so this is a fence rather than a path.
     */
    val sendableAcknowledgement: ActionAcknowledgement?
        get() = acknowledgement?.takeUnless {
            demand == PlaybackDemand.END && it.state == AcknowledgementState.COMMITTED
        }
}

@Serializable
data class ControlRequest(
    val protocol: String,
    val generation: String,
    @SerialName("control_epoch") val controlEpoch: Long,
    @SerialName("client_instance_id") val clientInstanceId: String,
    val sequence: Long,
    val demand: PlaybackDemand,
    @SerialName("position_ms") val positionMs: Long,
    @SerialName("buffered_from_ms") val bufferedFromMs: Long? = null,
    @SerialName("buffered_through_ms") val bufferedThroughMs: Long,
    @SerialName("playback_rate") val playbackRate: Double,
    @SerialName("render_state") val renderState: RenderState,
    @SerialName("seek_target_ms") val seekTargetMs: Long? = null,
    @SerialName("observed_download_bps") val observedDownloadBps: Long? = null,
    val selection: ClientSelection,
    val capabilities: DynamicCapabilities? = null,
    val observation: ClientObservation? = null,
    /**
     * How the prepared replacement this client was offered is going, if it was
     * offered one. Absent is the ordinary case and absent is correct: the
     * server's field is an `Option`, and `explicitNulls = false` drops a null
     * rather than sending one. Unlike [supportedActions] this field must *not*
     * appear on every exchange, so a null default is the right shape.
     */
    val acknowledgement: ActionAcknowledgement? = null,
    // No default. `Json` encodes defaults only when asked to, so a defaulted
    // value here would be silently dropped from the request — and a server that
    // never sees the vocabulary never sends the action, which is the exact
    // failure this field exists to prevent.
    @SerialName("supported_actions") val supportedActions: List<String>,
)

/**
 * Deliberately flat, and deliberately not a sealed class.
 *
 * The server tags this internally on `type` with no `deny_unknown_fields`, so a
 * later server may add keys and this parser must tolerate them. Web and Apple
 * both keep a flat shape and this file's header makes cross-client sameness a
 * contract; [QualitySelection] is the one sealed class here and it is not the
 * pattern to follow.
 */
@Serializable
data class ControlAction(
    val type: String,
    /**
     * Present on `hold` and `retry_resource`, and diagnostic rather than
     * dispositive: a reason this client has never heard of is a newer server,
     * not a broken one.
     */
    val reason: String? = null,
    /** `retry_resource` only: when the server wants to be asked again. */
    @SerialName("after_ms") val afterMs: Long? = null,
    /** `terminal` only: which producer decision ended this session. */
    val code: String? = null,
    val message: String? = null,
    /**
     * `prepare` only, and the transaction's identity: minted once per staging
     * and replayed byte-identically on every exchange until it settles. Every
     * acknowledgement echoes it, which is how a commit that arrives late names
     * *this* staging rather than whichever one is current when it lands — and
     * why a repeat of the same id is the same preparation, not a new one.
     */
    @SerialName("action_id") val actionId: String? = null,
    /** `prepare` only: the successor's own session UUID. */
    @SerialName("session_id") val sessionId: String? = null,
    /** `prepare` only: node-relative, and never to be followed as absolute. */
    @SerialName("playlist_url") val playlistUrl: String? = null,
    /**
     * `prepare` only: the source position the successor's session-relative zero
     * maps to. Without it the second timeline cannot be aligned with the first,
     * and the commit boundary is expressed in film time.
     */
    @SerialName("media_origin_ms") val mediaOriginMs: Long? = null,
    /** `prepare` only: what the successor will deliver, not what was asked. */
    @SerialName("effective_selection") val effectiveSelection: EffectiveSelection? = null,
) {
    /**
     * `prepared_payload_is_valid` (`playback_control.rs:1776`), client side.
     *
     * The playlist is checked against this action's *own* `session_id`, as the
     * server checks it: a URL naming a different session is not this
     * successor's, and following it would prime a pipeline on a stream nobody
     * staged.
     */
    val preparedPayloadIsValid: Boolean
        get() {
            val id = actionId ?: return false
            val session = sessionId ?: return false
            val playlist = playlistUrl ?: return false
            val origin = mediaOriginMs ?: return false
            val selection = effectiveSelection ?: return false
            return PlaybackControl.isUuidShape(id) &&
                id.length <= PlaybackControl.MAX_ACTION_ID_LEN &&
                PlaybackControl.isUuidShape(session) &&
                PlaybackControl.isNodeRelativePlaylist(playlist, session) &&
                origin in 0..PlaybackControl.MAX_MEDIA_MILLIS &&
                selection.isValid
        }
}

@Serializable
data class ControlDelivery(
    @SerialName("subtitle_readiness") val subtitleReadiness: String? = null,
)

internal object SubtitleReadinessDecision {
    fun meansReady(value: String?): Boolean = value == "ready"
}

internal class SubtitleReadinessRetryState {
    private var lastReady: Boolean? = null

    @Synchronized
    fun record(value: String?): Boolean {
        val ready = SubtitleReadinessDecision.meansReady(value)
        val retry = lastReady == false && ready
        lastReady = ready
        return retry
    }
}

@Serializable
data class ControlResponse(
    val protocol: String,
    val generation: String,
    @SerialName("control_epoch") val controlEpoch: Long,
    @SerialName("accepted_sequence") val acceptedSequence: Long,
    val action: ControlAction,
    val delivery: ControlDelivery? = null,
    /**
     * What the session that is playing *now* delivers. The server has always
     * sent it; `ignoreUnknownKeys` meant this client dropped it. Comparing it
     * against a `prepare`'s own selection is how a client knows what is about
     * to change.
     */
    @SerialName("effective_selection") val effectiveSelection: EffectiveSelection? = null,
)

/**
 * A transport failure the reporter can classify. [status] is the HTTP status
 * where there was one and `null` where the request never reached a server;
 * [code] is the server's typed `ControlErrorBody.code`.
 */
class ControlTransportException(
    val status: Int? = null,
    val code: String? = null,
    val generation: String? = null,
    val controlEpoch: Long? = null,
    val retryAfterMs: Long? = null,
) : Exception("control transport ${status ?: "-"}/${code ?: "-"}")

/**
 * The reporter refuses a response it cannot bind to the request it sent. This
 * is terminal on purpose: a mismatched generation or a non-`none` action means
 * something other than this session's owner is answering.
 */
class ControlProtocolException(val reason: String) : Exception("control protocol $reason")

// ------------------------------------------------------------------ reporter

/**
 * One in-flight exchange, newest-snapshot-wins coalescing, monotonic
 * sequences, and a bounded exchange deadline.
 *
 * The state lives behind [mutex] rather than in atomics because every rule
 * here is about ordering, not about a single value: at most one exchange
 * outstanding, a snapshot arriving during an exchange replacing any other
 * waiting snapshot instead of queueing behind it, and a sequence that is
 * never reused or skipped.
 */
class PlaybackControlReporter private constructor(
    bootstrap: ControlBootstrap,
    private val clientInstanceId: String,
    private val snapshot: () -> PlaybackControlSnapshot?,
    private val send: suspend (String, ControlRequest) -> ControlResponse,
    private val pace: suspend (Long) -> Unit,
    private val now: () -> Long,
    private val onExchange: (Exchange) -> Unit,
) {
    data class Exchange(
        val request: ControlRequest,
        val response: ControlResponse?,
        val failure: String?,
    )

    data class Status(
        val sequence: Long,
        val acceptedSequence: Long,
        val inFlight: Boolean,
        val pending: Boolean,
        val retrying: Boolean,
        val stopped: Boolean,
    )

    private val mutex = Mutex()

    var bootstrap: ControlBootstrap = bootstrap
        private set

    private var sequence = 0L
    private var acceptedSequence = 0L
    private var stopped = false
    private var inFlight = false
    private var pending: PlaybackControlSnapshot? = null
    private var retryRequest: ControlRequest? = null
    private var acceptedCapabilities: DynamicCapabilities? = null
    private var lastStartedAt: Long? = null
    private var nextAllowedAt = 0L
    private var pump: Job? = null

    companion object {
        /**
         * Returns null rather than throwing for a bootstrap or identity this
         * client cannot address: a server that offers no usable control
         * bootstrap is the ordinary passive case, not a playback failure.
         */
        fun create(
            bootstrap: ControlBootstrap,
            clientInstanceId: String,
            snapshot: () -> PlaybackControlSnapshot?,
            send: suspend (String, ControlRequest) -> ControlResponse,
            pace: suspend (Long) -> Unit,
            now: () -> Long,
            onExchange: (Exchange) -> Unit = {},
        ): PlaybackControlReporter? {
            if (!bootstrap.isValid || !PlaybackControl.isUuid(clientInstanceId)) return null
            return PlaybackControlReporter(
                bootstrap, clientInstanceId, snapshot, send, pace, now, onExchange,
            )
        }
    }

    /**
     * Report the current state now, and keep reporting until the player ends
     * or the reporter is stopped. Idempotent: a second call while the pump is
     * alive is a no-op rather than a second exchange loop.
     */
    suspend fun start(scope: CoroutineScope) {
        mutex.withLock {
            if (stopped || pump != null) return
            if (pending == null) pending = snapshot()
        }
        val job = scope.launch { run() }
        val alreadyStopped = mutex.withLock {
            if (stopped) true else { pump = job; false }
        }
        if (alreadyStopped) job.cancel()
    }

    /**
     * The player changed. The newest snapshot replaces any waiting one — an
     * intermediate position between two exchanges is not worth a round trip,
     * and reporting it late would be worse than not reporting it.
     */
    suspend fun notify(value: PlaybackControlSnapshot? = null) {
        val newest = value ?: snapshot() ?: return
        if (!newest.isValid) return
        mutex.withLock {
            if (stopped) return
            pending = newest
        }
    }

    /**
     * Report now rather than at the next cadence.
     *
     * [notify] leaves the pump asleep for `next_exchange_ms`, which the server
     * may set as high as a minute. For a position update that is the point.
     * For a recovery owner about to reopen it is fatal: the reopen ends this
     * reporter before the pump wakes, so the evidence is never sent at all
     * rather than sent late. The web reporter drains inline at exactly this
     * call site, for exactly this reason.
     *
     * Waking is a cancel-and-relaunch because the pump is suspended inside the
     * injected pace. An exchange already in flight is left alone: [run] picks
     * up `pending` immediately after it without pacing.
     */
    suspend fun notifyUrgently(
        scope: CoroutineScope,
        value: PlaybackControlSnapshot? = null,
    ) {
        notify(value)
        // The swap is one critical section on purpose. Releasing the lock
        // between clearing `pump` and setting it would let a concurrent
        // `start()` — whose guard is `pump != null` — launch a second run
        // loop, and `stop()` can only cancel the one it can see. `launch`
        // does not suspend, so holding the mutex across it is safe.
        val stale = mutex.withLock {
            if (stopped || inFlight || pending == null) return
            val running = pump
            pump = scope.launch { run() }
            running
        }
        stale?.cancel()
    }

    /**
     * Hand this reporter to another scope and report once more.
     *
     * The teardown counterpart to [notifyUrgently], and it differs in exactly
     * one way: it re-homes the pump **unconditionally**, including across an
     * exchange that is already in flight. [notifyUrgently] deliberately leaves
     * that exchange alone because `run()` picks the new snapshot up straight
     * afterwards — which is right while the pump is alive, and wrong here,
     * because the caller is about to cancel the scope that pump is on. A
     * coroutine cancelled inside `send` never reaches its
     * `mutex.withLock { inFlight = false }`, so waiting for it to finish waits
     * forever.
     *
     * Returns false when there was nothing to say — a stopped reporter, or a
     * snapshot the server would refuse — so the caller can stop rather than
     * wait out a deadline for an exchange that will never be built.
     */
    suspend fun settle(scope: CoroutineScope, value: PlaybackControlSnapshot): Boolean {
        if (!value.isValid) return false
        val stale = mutex.withLock {
            if (stopped) return false
            val running = pump
            pump = scope.launch { run() }
            running
        }
        stale?.cancel()
        // Re-asserted after the cancel, not before it. The cancelled coroutine
        // no longer writes anything — it re-throws — which also means it never
        // reached its own `inFlight = false`, so the slot it was holding has to
        // be reclaimed here or the exchange this method exists to send would
        // wait behind a coroutine that no longer exists. `retryRequest` and the
        // pacing floor go with it: a replay of the pre-teardown request would
        // carry no acknowledgement, which is the one thing that had to go out.
        return mutex.withLock {
            if (stopped) {
                false
            } else {
                inFlight = false
                retryRequest = null
                nextAllowedAt = 0L
                pending = value
                true
            }
        }
    }

    suspend fun stop() {
        val job = mutex.withLock {
            if (stopped) return
            stopped = true
            pending = null
            retryRequest = null
            // The coroutine about to be cancelled re-throws rather than
            // running its tail, so it never clears this itself. No reader
            // consults `inFlight` without checking `stopped` first, so this is
            // hygiene rather than a fix — but a slot left held on a stopped
            // reporter is the kind of state that becomes a fix later.
            inFlight = false
            val running = pump
            pump = null
            running
        }
        job?.cancel()
    }

    suspend fun status(): Status = mutex.withLock {
        Status(sequence, acceptedSequence, inFlight, pending != null, retryRequest != null, stopped)
    }

    suspend fun isStopped(): Boolean = mutex.withLock { stopped }

    private suspend fun run() {
        while (true) {
            val cadence = bootstrap.nextExchangeMs
            val wait = mutex.withLock {
                if (stopped) return
                if (pending == null && retryRequest == null) pending = snapshot()
                if (pending == null && retryRequest == null) return@withLock cadence
                val rateAllowedAt = lastStartedAt?.plus(PlaybackControl.MIN_EXCHANGE_MS) ?: 0L
                maxOf(0L, maxOf(rateAllowedAt, nextAllowedAt) - now())
            }
            if (wait > 0) {
                pace(wait)
                continue
            }
            val request = mutex.withLock { if (stopped) return else nextRequestLocked() }
            if (request == null) {
                pace(cadence)
                continue
            }
            exchange(request)
            val idle = mutex.withLock {
                if (stopped) return
                pending == null && retryRequest == null
            }
            if (idle) pace(cadence)
        }
    }

    /**
     * A retry replays the exact request that failed — the same sequence, the
     * same body — because a control exchange the server never accepted must
     * not consume a sequence number, and the server dedupes on it.
     */
    private fun nextRequestLocked(): ControlRequest? {
        retryRequest?.let { return it }
        val newest = pending
        pending = null
        if (newest == null || !newest.isValid) return null
        sequence += 1
        // Capabilities are static for the life of a player. Repeating them on
        // every exchange is bytes the server already has; the first request of
        // a generation must carry them, and a change must resend them.
        val repeats = sequence != 1L && newest.capabilities == acceptedCapabilities
        return ControlRequest(
            protocol = PlaybackControl.PROTOCOL,
            generation = bootstrap.generation,
            controlEpoch = bootstrap.controlEpoch,
            clientInstanceId = clientInstanceId,
            sequence = sequence,
            demand = newest.demand,
            positionMs = newest.positionMs,
            bufferedFromMs = newest.bufferedFromMs,
            bufferedThroughMs = newest.bufferedThroughMs,
            playbackRate = newest.playbackRate,
            renderState = newest.renderState,
            seekTargetMs = newest.seekTargetMs,
            observedDownloadBps = newest.observedDownloadBps,
            selection = newest.selection,
            capabilities = if (repeats) null else newest.capabilities,
            observation = newest.observation?.bounded(),
            acknowledgement = newest.sendableAcknowledgement,
            supportedActions = PlaybackControl.SUPPORTED_ACTIONS,
        )
    }

    private suspend fun exchange(request: ControlRequest) {
        val url = mutex.withLock {
            nextAllowedAt = 0
            lastStartedAt = now()
            inFlight = true
            bootstrap.url
        }
        val outcome = try {
            Result.success(withExchangeDeadline { send(url, request) })
        } catch (cancelled: CancellationException) {
            // A cancellation is not a failed exchange, and catching it as one
            // is worse than losing the exchange. `CancellationException` is an
            // `Exception`, and every `mutex.withLock` below is uncontended —
            // so a cancelled coroutine runs the whole tail: it records a
            // fabricated `transport:none:-`, and `handle` classifies a null
            // status as retryable and arms `retryRequest` plus a five-second
            // `nextAllowedAt`. The pump that took over then paces five seconds
            // and, when it does speak, replays the request it inherited rather
            // than the one it was handed. That is exactly the state a teardown
            // hand-off leaves behind, so the one path that most needs to speak
            // is the one this silenced.
            //
            // `withExchangeDeadline` has already turned a real deadline into a
            // `ControlTransportException(408)` above, so the timeout path is
            // untouched by this.
            throw cancelled
        } catch (failure: Exception) {
            Result.failure(failure)
        }
        mutex.withLock { inFlight = false }
        val response = outcome.getOrElse { failure ->
            if (mutex.withLock { stopped }) return
            handle(failure, request)
            return
        }
        try {
            accept(request, response)
        } catch (protocolFailure: ControlProtocolException) {
            onExchange(Exchange(request, null, describe(protocolFailure)))
            stop()
            return
        }
        val ended = mutex.withLock {
            if (stopped) return
            retryRequest = null
            request.capabilities?.let { acceptedCapabilities = it }
            acceptedSequence = maxOf(acceptedSequence, response.acceptedSequence)
            // `retry_resource` paces the next exchange from the server's own
            // cadence rather than this client's guess. The exchange succeeded;
            // the server only said when to ask again, so this does not touch
            // the retry path.
            if (response.action.type == "retry_resource" && response.action.afterMs != null) {
                nextAllowedAt = now() + maxOf(
                    PlaybackControl.MIN_EXCHANGE_MS,
                    response.action.afterMs,
                )
            }
            // A terminal verdict ends reporting. It does not tear the player
            // down: this reporter still owns no recovery, and buffer already
            // fetched is still worth playing.
            request.demand == PlaybackDemand.END || response.action.type == "terminal"
        }
        onExchange(Exchange(request, response, null))
        if (ended) stop()
    }

    private fun accept(request: ControlRequest, response: ControlResponse) {
        if (response.protocol != PlaybackControl.PROTOCOL) {
            throw ControlProtocolException("protocol")
        }
        if (response.generation != request.generation) {
            throw ControlProtocolException("generation")
        }
        if (response.controlEpoch != request.controlEpoch) {
            throw ControlProtocolException("control_epoch")
        }
        if (response.acceptedSequence != request.sequence) {
            throw ControlProtocolException("accepted_sequence")
        }
        // An action outside the declared vocabulary means the server and this
        // client disagree about the contract, and continuing would be
        // guessing. A `hold` is inside it: the server is explaining that
        // production is deliberately not advancing, which is the opposite of a
        // reason to stop reporting. Its reason must be present but need not be
        // one this client recognises.
        when (response.action.type) {
            "none" -> Unit
            "hold" ->
                if (response.action.reason == null) {
                    throw ControlProtocolException("action")
                }
            // An action inside the declared vocabulary but missing the field
            // this client acts on is worse than one it has never heard of,
            // because it would be acted on.
            "terminal" ->
                if (response.action.code == null || response.action.message == null) {
                    throw ControlProtocolException("action")
                }
            "retry_resource" -> {
                val afterMs = response.action.afterMs
                if (response.action.reason == null ||
                    afterMs == null ||
                    afterMs <= 0 ||
                    afterMs > PlaybackControl.MAX_EXCHANGE_MS
                ) {
                    throw ControlProtocolException("action")
                }
            }
            // `"prepare"`, not `"prepare_replacement"`. The declared name and
            // the wire tag differ for this one action alone, and switching on
            // the declared name here would never fire while every test that
            // fed it kept passing.
            //
            // A malformed `prepare` costs the viewer the whole control plane —
            // this exception records `protocol:action` and stops the reporter
            // for the rest of the film — so it is validated strictly here,
            // before anything primes a second pipeline on it.
            PlaybackControl.PREPARE_ACTION_TYPE ->
                if (!response.action.preparedPayloadIsValid) {
                    throw ControlProtocolException("action")
                }
            else -> throw ControlProtocolException("action")
        }
    }

    private suspend fun handle(failure: Throwable, request: ControlRequest) {
        onExchange(Exchange(request, null, describe(failure)))
        // A protocol failure is not a transport failure, and folding it into
        // the null-status arm below makes it retryable — which turns a
        // diagnosable fatal into an unbounded silent loop, replaying the same
        // request at the cadence forever and logging `transport:none:-`. The
        // body this client could not read will not become readable on the
        // fourth attempt.
        if (failure is ControlProtocolException) {
            stop()
            return
        }
        val transport = failure as? ControlTransportException
        val status = transport?.status
        val code = transport?.code
        if (status == 409 && code == "owner_changed") {
            val adopted = mutex.withLock {
                if (!adoptNewOwnerLocked(transport)) {
                    false
                } else {
                    nextAllowedAt = now() + retryDelay(transport, PlaybackControl.MIN_EXCHANGE_MS)
                    true
                }
            }
            if (!adopted) stop()
            return
        }
        val retryableControl = (status == 425 && code == "owner_transition") ||
            (status == 429 && code == "control_rate_limited") ||
            (status == 503 && code == "control_unavailable")
        val retryableTransport = status == 408 || status == null
        if (!retryableControl && !retryableTransport) {
            stop()
            return
        }
        val fallback = if (retryableControl) 500L else bootstrap.nextExchangeMs
        mutex.withLock {
            if (stopped) return
            retryRequest = request
            nextAllowedAt = now() + retryDelay(transport, fallback)
        }
    }

    /**
     * A 409 names the generation and epoch that now own the session. Adopting
     * them restarts the sequence at zero for the new owner rather than
     * carrying a number the new owner never issued — but only when the server
     * actually named a newer owner, so a malformed 409 cannot silently rebind
     * this player to another session.
     */
    private fun adoptNewOwnerLocked(error: ControlTransportException?): Boolean {
        val generation = error?.generation ?: return false
        val epoch = error.controlEpoch ?: return false
        if (!PlaybackControl.isUuid(generation) || epoch <= 0) return false
        val generationChanged = generation != bootstrap.generation
        val epochChanged = epoch > bootstrap.controlEpoch
        if (!generationChanged && !epochChanged) return false
        val newest = snapshot() ?: return false
        if (!newest.isValid) return false
        bootstrap = bootstrap.copy(generation = generation, controlEpoch = epoch)
        sequence = 0
        acceptedSequence = 0
        retryRequest = null
        acceptedCapabilities = null
        lastStartedAt = null
        pending = newest
        return true
    }

    private fun retryDelay(error: ControlTransportException?, fallback: Long): Long {
        val value = error?.retryAfterMs ?: return fallback
        if (value < 0 || value > PlaybackControl.MAX_EXCHANGE_MS) return fallback
        return maxOf(PlaybackControl.MIN_EXCHANGE_MS, value)
    }

    private fun describe(failure: Throwable): String = when (failure) {
        is ControlProtocolException -> "protocol:${failure.reason}"
        is ControlTransportException ->
            "transport:${failure.status ?: "none"}:${failure.code ?: "-"}"
        else -> "transport:none:-"
    }

    /**
     * The exchange itself is bounded, not just the socket: a server that
     * accepts the request and then never answers must not hold the only
     * in-flight slot open forever.
     */
    private suspend fun <T> withExchangeDeadline(work: suspend () -> T): T =
        try {
            withTimeout(PlaybackControl.EXCHANGE_DEADLINE_MS) { work() }
        } catch (_: TimeoutCancellationException) {
            throw ControlTransportException(status = 408, code = "exchange_deadline")
        }
}
