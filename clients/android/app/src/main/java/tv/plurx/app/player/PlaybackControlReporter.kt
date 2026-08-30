package tv.plurx.app.player

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
 * Passive means passive: the server's only permitted action today is `none`,
 * and anything else is treated as a protocol error that stops the reporter
 * rather than as an instruction. M5 gives clients an action owner; this type
 * keeps reporting when it arrives.
 */
object PlaybackControl {
    const val PROTOCOL = "plurx-playback-control-v1"
    const val MIN_EXCHANGE_MS = 250L
    const val MAX_EXCHANGE_MS = 60_000L
    const val EXCHANGE_DEADLINE_MS = 6_000L
    const val MAX_LEASE_TIMEOUT_MS = 600_000L
    const val MIN_HEIGHT = 144
    const val MAX_HEIGHT = 2_160
    const val MAX_CAPABILITY_VALUES = 8
    const val MAX_OBSERVED_DOWNLOAD_BPS = 10_000_000_000_000L
    const val MAX_ERROR_DETAIL_BYTES = 512

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
            capabilities.isValid
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
)

@Serializable
data class ControlAction(val type: String)

@Serializable
data class ControlResponse(
    val protocol: String,
    val generation: String,
    @SerialName("control_epoch") val controlEpoch: Long,
    @SerialName("accepted_sequence") val acceptedSequence: Long,
    val action: ControlAction,
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

    suspend fun stop() {
        val job = mutex.withLock {
            if (stopped) return
            stopped = true
            pending = null
            retryRequest = null
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
            request.demand == PlaybackDemand.END
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
        // M2 clients consume nothing. An action of any other type is a server
        // this client does not understand, not an instruction to improvise.
        if (response.action.type != "none") {
            throw ControlProtocolException("action")
        }
    }

    private suspend fun handle(failure: Throwable, request: ControlRequest) {
        onExchange(Exchange(request, null, describe(failure)))
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
