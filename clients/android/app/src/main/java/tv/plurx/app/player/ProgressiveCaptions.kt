@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.os.SystemClock
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.PlayerMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.withTimeout
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.TimeoutCancellationException
import java.io.IOException
import kotlinx.serialization.decodeFromString
import okhttp3.ResponseBody
import retrofit2.Response
import tv.plurx.app.data.*
import java.math.BigInteger
import java.security.MessageDigest
import java.util.UUID

internal fun captionCounter(value: String): BigInteger {
    require(value.matches(Regex("0|[1-9][0-9]{0,18}"))) { "Invalid caption revision" }
    return BigInteger(value).also { require(it <= BigInteger.valueOf(Long.MAX_VALUE)) }
}
internal fun nextCaptionCounter(value: String): String = captionCounter(value).add(BigInteger.ONE).toString().also { captionCounter(it) }
internal fun captionDigest(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(bytes)
    .joinToString("") { "%02x".format(it.toInt() and 255) }
internal fun captionHash(value: String): Boolean = value.matches(Regex("[a-f0-9]{64}"))

/** The cap applies to the stream, including unknown or dishonest Content-Length. */
internal suspend fun captionBytes(response: Response<ResponseBody>, limit: Long = 65_536): ByteArray = withContext(Dispatchers.IO) {
    if (!response.isSuccessful) {
        val refusal = parseRefusal(response.code(), boundedErrorBody(response))
        throw refusal ?: retrofit2.HttpException(response)
    }
    val body = response.body() ?: error("Empty caption response")
    body.use {
        require(response.headers()["Content-Type"]?.substringBefore(';')?.trim() == "application/json") { "Invalid caption content type" }
        require(it.contentLength() <= limit) { "Caption response is too large" }
        val source = it.source()
        source.request(limit + 1)
        require(source.buffer.size <= limit) { "Caption response is too large" }
        source.buffer.readByteArray()
    }
}
internal suspend inline fun <reified T> captionRead(response: Response<ResponseBody>, limit: Long = 65_536): T =
    Net.json.decodeFromString(captionBytes(response, limit).decodeToString(throwOnInvalidSequence = true))

/** Reject stale operation/manifest views without numeric precision loss. */
internal class CaptionViewFence {
    private var id: String? = null
    private var epoch = "0"
    private var revision = "0"
    fun acceptCurrent(operationId: String, operationEpoch: String, viewRevision: String): Boolean {
        val nextEpoch = captionCounter(operationEpoch); val nextView = captionCounter(viewRevision)
        require(nextEpoch > BigInteger.ZERO && nextView > BigInteger.ZERO)
        require(id == null || id == operationId) { "Caption operation identity changed" }
        if (nextEpoch < captionCounter(epoch) || nextView < captionCounter(revision)) return false
        id = operationId; epoch = operationEpoch; revision = viewRevision
        return true
    }
    fun accept(operationId: String, operationEpoch: String, viewRevision: String) {
        require(acceptCurrent(operationId, operationEpoch, viewRevision)) { "Stale caption view" }
    }
}

internal fun verifiedCaptionSegment(bytes: ByteArray, ref: CaptionSegmentRef, manifest: CaptionManifest,
    chosen: CaptionBinding): CaptionSegment {
    require(bytes.size <= 16_384 && captionHash(ref.digest) && captionDigest(bytes) == ref.digest) { "Caption segment checksum mismatch" }
    val segment = Net.json.decodeFromString<CaptionSegment>(bytes.decodeToString(throwOnInvalidSequence = true))
    require(segment.version == 1 && segment.asset_id == manifest.asset_id && segment.segment_id == ref.segment_id &&
        segment.start_ms == ref.start_ms && segment.end_ms == ref.end_ms && segment.cues.size <= 128 &&
        segment.provenance.profile_id == chosen.profile.profile_id && segment.provenance.source_sha256 == manifest.content_revision)
    require(captionCounter(segment.provenance.operation_epoch) <= captionCounter(manifest.operation.epoch))
    require(segment.cues.all { cue -> captionHash(cue.cue_id) && cue.start_ms >= ref.start_ms && cue.start_ms < ref.end_ms &&
        cue.end_ms > cue.start_ms && cue.end_ms <= ref.end_ms && cue.text.isNotBlank() &&
        cue.text.toByteArray().size <= 4096 && cue.text.none { it.isISOControl() && it != '\n' && it != '\t' } }) { "Invalid caption cue" }
    return segment
}

internal data class ProgressiveCaptionSelection(val task: String, val spokenLanguage: String, val profileId: String? = null)
/** Presentation-owned intent survives quality replans; no invented subtitle stream index. */
internal class ProgressiveCaptionIntent {
    val continuity = CaptionContinuity()
    val producer = CaptionProducerOwnership()
    val mutations = CaptionProducerMutations<CaptionProducerRequest>(producer)
    var stopRevision: Long = 0; private set
    fun requestStop() { stopRevision++ }
    private var traversal: Pair<Long, Long>? = null
    fun bindTraversal(fileId: Long, audio: Long) {
        val next = fileId to audio
        if (traversal != null && traversal != next) { mutations.retire(); producer.newTraversal() }
        traversal = next
        continuity.bind(fileId, audio)
    }
    fun endTraversal() { traversal = null; mutations.retire(); producer.newTraversal(); continuity.endTraversal() }
    fun prepareOwnerTransfer() { if (selection != null) producer.prepareTransfer() }
    fun retire() { traversal = null; mutations.retire(); producer.retire(); continuity.endTraversal() }
    var selectionRevision: Long = 0; private set
    fun captionChoiceChanged() { selectionRevision++ }
    var selection: ProgressiveCaptionSelection? by mutableStateOf(null)
    var stopped: Boolean = false
    var operationId: String? = null
    var audioOrdinal: Long? = null
    var offsetMs: Long by mutableStateOf(0L)
    /** A late renewal or a running observation can never undo the viewer's Stop. */
    fun acceptDemand(state: String?, explicitResume: Boolean = false): Boolean {
        if (state == "stopped") { stopped = true; return false }
        if (stopped && !explicitResume) return false
        if (explicitResume) stopped = false
        return true
    }
}

internal data class CaptionBinding(val options: CaptionOptions, val profile: CaptionProfile, val audio: CaptionAudio,
    val selection: ProgressiveCaptionSelection) {
    val outputLanguage: String get() = if (selection.task == "translate") "en" else selection.spokenLanguage
    fun matches(asset: CaptionAsset): Boolean = asset.source_size == options.source_size &&
        asset.source_mtime == options.source_mtime && asset.profile_id == profile.profile_id &&
        asset.audio_index == audio.audio_ordinal && asset.spoken_language == selection.spokenLanguage &&
        asset.output_language == outputLanguage && asset.timeline_contract == options.timeline_contract
}
internal fun captionBinding(options: CaptionOptions, selectedAudioOrdinal: Long?, selection: ProgressiveCaptionSelection): CaptionBinding {
    require(options.version == 1 && options.duration_ms in 1..14_400_000 && options.models.size <= 16 && options.audio_tracks.size <= 64)
    require(options.source_origin_ms != null) { "The source clock has not been verified" }
    require(selection.task in setOf("translate", "transcribe"))
    val audio = options.audio_tracks.singleOrNull { it.audio_ordinal == selectedAudioOrdinal && selectedAudioOrdinal != null }
        ?: error("The selected audio track has no verified caption mapping")
    require(audio.audio_ordinal != null && audio.audio_ordinal >= 0 && audio.capability?.supported == true) { "The selected audio clock is unsupported" }
    require(captionCounter(options.source_size) > BigInteger.ZERO && options.source_mtime.toLongOrNull()?.toString() == options.source_mtime)
    val profile = captionPreferredProfile(options.models, options.default_profile_id, selection.task, selection.profileId)
        ?.takeIf { selection.spokenLanguage in it.input_languages }
        ?: error("The selected caption model or language is unavailable; choose a model in Settings")
    require(captionHash(profile.profile_id))
    return CaptionBinding(options, profile, audio, selection)
}

/** Immutable UI target: a later view may refresh controls, never redirect an old Retry tap. */
internal data class CaptionRetryTarget(val operationId: String, val epoch: String, val view: String,
    val state: String, val range: CaptionFailedRange? = null)
internal fun captionRetryTarget(op: CaptionOperation, range: CaptionFailedRange? = null): CaptionRetryTarget {
    captionCounter(op.operation_epoch); captionCounter(op.view_revision)
    if (range == null) require(op.state == "stopped" || op.state == "failed" && op.retryable)
    else require(op.state == "partial" && range.retryable && range in op.failed_ranges &&
        range.core_index in 0..2047 && captionCounter(range.range_revision) > BigInteger.ZERO)
    return CaptionRetryTarget(op.operation_id, op.operation_epoch, op.view_revision, op.state, range)
}
internal fun captionRetryTargetCurrent(target: CaptionRetryTarget, op: CaptionOperation): Boolean =
    runCatching { captionRetryTarget(op, target.range) == target }.getOrDefault(false)
internal data class CaptionRetryAttempt(val target: CaptionRetryTarget, val operation: CaptionRetry? = null,
    val range: CaptionRangeRetry? = null)
internal fun captionRetryAttempt(target: CaptionRetryTarget, requestId: String, expires: Long,
    playbackId: String, position: Long, rate: Double, paused: Boolean, continuityId: String? = null): CaptionRetryAttempt =
    target.range?.let { range -> CaptionRetryAttempt(target, range = CaptionRangeRetry(1, requestId, expires,
        target.epoch, "partial", range.core_index, range.range_revision, "failed", playbackId, position, rate, paused, "range", continuityId)) }
        ?: CaptionRetryAttempt(target, operation = CaptionRetry("operation", 1, requestId, expires,
            target.epoch, target.view, target.state, playbackId, position, rate, paused, continuityId))
internal fun captionValidateFailures(rows: List<CaptionFailedRange>, cursor: String?) {
    require(rows.size <= 16 && rows.map { it.core_index }.distinct().size == rows.size)
    require(cursor == null || cursor.length in 1..4096)
    require(rows.all { it.core_index in 0..2047 && captionCounter(it.range_revision) > BigInteger.ZERO && it.attempts >= 0 })
}
internal fun captionReadRetryable(error: Exception): Boolean {
    val status = refusalStatusOf(error)
    if (error is RefusalException && error.code in setOf("source_changed", "source_missing", "forbidden", "unauthorized")) return false
    return error is IOException || error is TimeoutCancellationException || status in setOf(408, 425, 429) ||
        error is RefusalException && status == 410 && error.code == "translation_snapshot_expired" ||
        status != null && status >= 500
}

/** Lost renewal replies replay the same UUID and generation; late completions cannot clear a successor. */
internal class CaptionDemandAttempt {
    private var key: List<String?>? = null
    private var body: CaptionDemand? = null
    fun request(receipt: CaptionReceipt, epoch: String, create: () -> CaptionDemand): CaptionDemand {
        val next = listOf(receipt.operation_id, epoch, receipt.demand_id, receipt.demand_generation)
        if (key != next) { key = next; body = null }
        return body ?: create().also { body = it }
    }
    fun finish(value: CaptionDemand, ambiguous: Boolean) { if (body === value && !ambiguous) clear() }
    fun clear() { key = null; body = null }
}

/** Two children of the same caption owner: slow window I/O cannot starve demand renewal. */
internal suspend fun captionPollingLifecycle(current: () -> Boolean, read: suspend () -> Unit,
    renew: suspend () -> Unit, readFailed: (Exception) -> Unit, renewalFailed: (Exception) -> Unit,
    pause: suspend (Long) -> Unit = { delay(it) }, renewalPause: suspend (Long) -> Unit = pause) = coroutineScope {
    suspend fun loop(interval: Long, maximumBackoff: Long, work: suspend () -> Unit, failed: (Exception) -> Unit, wait: suspend (Long) -> Unit) {
        var backoff = interval
        while (current()) {
            try { work(); backoff = interval }
            catch (e: Exception) {
                if (e is CancellationException && e !is TimeoutCancellationException) throw e
                if (!current()) return
                if (!captionReadRetryable(e)) throw e
                failed(e)
                backoff = (backoff * 2).coerceAtMost(maximumBackoff)
            }
            if (current()) wait(backoff)
        }
    }
    val renewal = launch { loop(3_000, 3_000, { withTimeout(3_000) { renew() } }, renewalFailed, renewalPause) }
    try { loop(2_000, 8_000, { withTimeout(12_000) { read() } }, readFailed, pause) }
    finally { renewal.cancelAndJoin() }
}

/** Immutable segments stay inside a bounded source window. No whole-film cue list. */
internal class CaptionWindow {
    var cues: List<CaptionCue> = emptyList(); private set
    fun clear() { cues = emptyList() }
    fun replace(segments: List<CaptionSegment>, positionMs: Long, durationMs: Long) {
        val low = (positionMs - 30_000).coerceAtLeast(0)
        val high = (positionMs + 90_000).coerceAtMost(durationMs)
        val next = segments.flatMap { it.cues }.filter { it.end_ms > low && it.start_ms < high }
            .distinctBy { it.cue_id }.sortedWith(compareBy({ it.start_ms }, { it.cue_id }))
        require(next.size <= 1024 && next.sumOf { it.text.toByteArray().size + 128 } <= 524_288) { "Caption cue window exceeds its limit" }
        cues = next
    }
    fun textAt(positionMs: Long): String = cues.filter { positionMs >= it.start_ms && positionMs < it.end_ms }
        .joinToString("\n") { it.text }
    fun nextBoundary(positionMs: Long): Long? = cues.asSequence().flatMap { sequenceOf(it.start_ms, it.end_ms) }
        .filter { it > positionMs }.minOrNull()
}

/** API polling schedules work; Media3 schedules every visible cue boundary. */
internal class ProgressiveCaptions(
    private val api: () -> PlurxApi,
    private val scope: CoroutineScope,
    private val cleanupScope: CoroutineScope,
    private val fileId: Long,
    private val intent: ProgressiveCaptionIntent,
    private val sourceAudio: () -> Long?,
    private val sourcePosition: () -> Long,
    private val playerPosition: (Long) -> Long,
    private val player: () -> ExoPlayer,
    private val authorized: () -> Boolean,
) {
    var options: CaptionOptions? by mutableStateOf(null); private set
    var text: String by mutableStateOf(""); private set
    var status: String by mutableStateOf("Captions Off"); private set
    var operation: CaptionOperation? by mutableStateOf(null); private set
    val selected: ProgressiveCaptionSelection? get() = intent.selection
    val offsetMs: Long get() = intent.offsetMs
    val canProduce: Boolean get() = options?.permissions?.demand == true
    val hasPendingRetry: Boolean get() = retryRequest != null || intent.mutations.pending?.request?.retry != null
    private val owner = intent.producer.claim()
    private val playbackId: String get() = intent.producer.playbackId
    private var generation = 0L
    private var job: Job? = null
    private var boundary: PlayerMessage? = null
    private var leaseExpiry: Job? = null
    private var active = true
    private var binding: CaptionBinding? = null
    private var receipt: CaptionReceipt? = null
    private var serverNow = 0L
    private var observedAt = 0L
    private var lastHeartbeat = 0L
    private val actions = CaptionActionFence()
    private val heartbeatAttempt = CaptionDemandAttempt()
    private val renewalWake = Channel<Unit>(Channel.CONFLATED)
    private var refreshingStatus = false
    private var createRequest: Pair<String, CaptionCreate>? = null
    private var retryRequest: Pair<String, CaptionRetryAttempt>? by mutableStateOf(null)
    private var failedRangePage: CaptionFailedRangePage? = null
    private var loadingFailures = false
    private var readRetired = false
    private var createStopResolution = false
    private var stopping = false
    private var viewFence = CaptionViewFence()
    private var lastManifestAsset: String? = null
    private var lastManifestRevision: String? = null
    private var lastLease: String? = null
    private var lastLeaseDeadline = 0L
    private var cachedSegments = emptyMap<String, CaptionSegment>()
    private val window = CaptionWindow()
    private var clockOwner: ExoPlayer? = null
    private var clockOrigin: Long? = null
    private fun current(g: Long) = active && intent.producer.owns(owner) && g == generation && authorized()
    private fun position() = sourcePosition().coerceIn(0, binding?.options?.duration_ms ?: 14_400_000)
    private fun rate() = player().playbackParameters.speed.toDouble().coerceIn(0.25, 4.0)
    private fun now() = serverNow + (SystemClock.elapsedRealtime() - observedAt).coerceAtLeast(0)
    private fun expires() = now() + 60_000
    private fun observe(time: Long) { serverNow = if (serverNow > 0) maxOf(now(), time) else time; observedAt = SystemClock.elapsedRealtime() }

    fun loadOptions() {
        if (!current(generation)) return
        val g = generation
        scope.launch {
            try {
                val answer = captionRead<CaptionOptions>(api().captionOptions(fileId))
                if (!current(g)) return@launch
                require(answer.version == 1 && answer.file_id == fileId.toString())
                captionPreferredProfile(answer.models, answer.default_profile_id, "translate")
                options = answer; observe(answer.server_now_ms)
                if (intent.selection == null && answer.default_profile_id == null &&
                    answer.models.any { it.default != null } && answer.models.none { it.default == true })
                    status = "The saved caption model is unavailable. Choose a model and task in Settings."
                if (intent.selection != null && job?.isActive != true && !readRetired) select(intent.selection!!)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) { if (current(g)) status = e.message ?: "Caption options unavailable" }
        }
    }

    fun select(selection: ProgressiveCaptionSelection) {
        if (!current(generation) || actions.inFlight && intent.selection == selection) return
        intent.captionChoiceChanged()
        readRetired = false; failedRangePage = null; loadingFailures = false; refreshingStatus = false
        actions.invalidate(keepPending = intent.selection == selection)
        if (intent.selection != selection) {
            intent.mutations.retire(); intent.producer.newTraversal()
            createRequest = null; retryRequest = null; createStopResolution = false
        }
        if (intent.selection != selection) { intent.operationId = null; intent.stopped = false }
        releaseDemand(preserveRetained = true)
        generation++; job?.cancel(); job = null; clearCues(); operation = null; viewFence = CaptionViewFence()
        intent.selection = selection
        val g = generation
        job = scope.launch {
            try {
                val opt = captionRead<CaptionOptions>(api().captionOptions(fileId))
                if (!current(g)) return@launch
                options = opt; observe(opt.server_now_ms)
                val chosen = captionBinding(opt, sourceAudio(), selection)
                binding = chosen
                if (intent.audioOrdinal != chosen.audio.audio_ordinal) { intent.operationId = null; intent.stopped = false }
                intent.audioOrdinal = chosen.audio.audio_ordinal
                intent.bindTraversal(fileId, requireNotNull(chosen.audio.audio_ordinal))
                intent.selection = selection.copy(profileId = chosen.profile.profile_id)
                var cached = opt.assets.firstOrNull(chosen::matches)
                var cursor = opt.assets_next_cursor
                val visited = mutableSetOf<String>()
                while (cached == null && cursor != null && visited.size < 8) {
                    require(visited.add(cursor) && cursor.length <= 4096)
                    val page = captionRead<CaptionAssetPage>(api().captionAssets(fileId, cursor))
                    if (!current(g)) return@launch
                    require(page.version == 1 && page.entries.size <= 64)
                    cached = page.entries.firstOrNull(chosen::matches); cursor = page.next_cursor
                }
                intent.operationId = intent.operationId ?: cached?.operation_id
                intent.operationId?.let { id ->
                    var refreshed = false
                    captionPollingLifecycle(current = { current(g) && !refreshed },
                        read = { observeOperation(id, chosen, g); refreshed = true }, renew = {},
                        readFailed = { status = "Caption status temporarily unavailable; retrying…" }, renewalFailed = {})
                }
                if (!current(g)) return@launch
                // A predecessor may have committed after its transport was cancelled. Resolve
                // the immutable shared request before any successor can mutate this playback.
                val inherited = intent.mutations.pending
                if (inherited != null) {
                    require(inherited.request.matches(fileId, chosen)) { "Caption source or profile changed" }
                    if (!settleProducer(chosen, g)) return@launch
                }
                if (inherited == null && canProduce && (!intent.stopped || createRequest != null)) {
                    val key = "create:${opt.source_size}:${opt.source_mtime}:${chosen.profile.profile_id}:${chosen.audio.audio_ordinal}:${selection.task}:${selection.spokenLanguage}:$playbackId"
                    val owner = actions.begin(key) ?: return@launch
                    if (!owner.replay) createStopResolution = false
                    val body = createRequest?.takeIf { it.first == owner.requestId }?.second ?: CaptionCreate(1,
                        owner.requestId, expires(), opt.source_size, opt.source_mtime, chosen.audio.audio_ordinal!!,
                        selection.spokenLanguage, chosen.outputLanguage, selection.task, chosen.profile.model_id,
                        chosen.profile.profile_id, playbackId, position(), rate(), !player().playWhenReady, intent.continuity.id)
                    createRequest = owner.requestId to body
                    var ambiguous = false
                    try {
                        val endpoint = api()
                        intent.mutations.install(this@ProgressiveCaptions.owner,
                            CaptionProducerMutations.Pending(CaptionProducerRequest(fileId, chosen, create = body),
                                explicitResume = false, stopRevision = intent.stopRevision, authorized = authorized) {
                                captionRead<CaptionReceipt>(endpoint.createCaptions(fileId, body)).also {
                                    captionReceiptContinuity(it, body.continuity_id)
                                }
                            })
                        if (!settleProducer(chosen, g)) return@launch
                    } catch (e: CancellationException) { throw e }
                    catch (e: Exception) { ambiguous = captionActionAmbiguous(e); if (!actions.owns(owner)) return@launch; throw e }
                    finally { if (actions.finish(owner, ambiguous) && !ambiguous) createRequest = null }
                }
                val id = intent.operationId
                if (id == null) {
                    status = if (intent.stopped) "Stopped before generation started. Resume requires an explicit action."
                        else "No cached captions. An administrator can generate them."
                    return@launch
                }
                pollOperation(id, chosen, g)
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                if (current(g)) {
                    readRetired = !captionReadRetryable(e)
                    clearCues(); releaseDemand(preserveRetained = intent.mutations.pending != null)
                    status = e.message ?: "Captions failed; refresh and retry"
                    if (intent.stopped && createRequest != null && !createStopResolution) {
                        createStopResolution = true; intent.selection?.let(::select) // Preserve the resolved profile and exact pending request for Stop.
                    }
                }
            }
        }
    }
    private suspend fun settleProducer(chosen: CaptionBinding, g: Long, resolveOnly: Boolean = false): Boolean {
        val pending = intent.mutations.pending ?: return false
        require(pending.request.matches(fileId, chosen)) { "Caption source or profile changed" }
        val settled = intent.mutations.settle(owner) { current(g) } ?: return false
        val value = settled.receipt
        if (!current(g)) return false
        intent.operationId = value.operation_id
        observe(value.server_now_ms)
        if (settled.pending.requiresStop(intent.stopRevision, intent.stopped, resolveOnly)) {
            val epoch = operation?.takeIf { it.operation_id == value.operation_id &&
                captionCounter(it.operation_epoch) > captionCounter(value.operation_epoch) }?.operation_epoch ?: value.operation_epoch
            stopOperation(value.operation_id, epoch, g)
        } else acceptReceipt(value, explicitResume = settled.pending.explicitResume)
        return true
    }

    private suspend fun pollOperation(id: String, chosen: CaptionBinding, g: Long) {
        try {
            captionPollingLifecycle(current = { current(g) }, read = {
                observeOperation(id, chosen, g)
                if (current(g)) operation?.asset_id?.let { readWindow(it, chosen, g) }
            }, renew = {
                if (current(g) && !intent.stopped && receipt?.demand_id != null &&
                    (SystemClock.elapsedRealtime() - lastHeartbeat >= 3_000 || receipt?.continuity_id != intent.continuity.id)) heartbeat(g)
            }, readFailed = {
                if (current(g)) {
                    if (SystemClock.elapsedRealtime() >= lastLeaseDeadline ||
                        it is RefusalException && it.code == "source_verifying") clearCues()
                    status = "Caption read temporarily unavailable; retrying…"
                }
            }, renewalFailed = {
                if (current(g)) status = "Caption renewal temporarily unavailable; retrying…"
            }, renewalPause = { interval -> withTimeoutOrNull(interval) { renewalWake.receive() }; Unit })
        } catch (e: CancellationException) { throw e }
        catch (e: Exception) {
            if (current(g)) { readRetired = true; clearCues(); releaseDemand() }
            throw e
        } finally {
            if (active && generation == g && !authorized()) {
                readRetired = true; clearCues(); receipt = null; heartbeatAttempt.clear()
                status = "Caption access changed; reopen this source."
            }
        }
    }

    private suspend fun observeOperation(id: String, chosen: CaptionBinding, g: Long) {
        val value = captionRead<CaptionOperation>(api().captionOperation(id))
        if (!current(g)) return
        require(value.version == 1 && value.operation_id == id && value.profile_id == chosen.profile.profile_id &&
            value.source_size == chosen.options.source_size && value.source_mtime == chosen.options.source_mtime &&
            value.audio_index == chosen.audio.audio_ordinal && value.task == chosen.selection.task &&
            value.spoken_language == chosen.selection.spokenLanguage && value.output_language == chosen.outputLanguage) { "Caption source changed" }
        if (!viewFence.acceptCurrent(value.operation_id, value.operation_epoch, value.view_revision)) return
        captionValidateFailures(value.failed_ranges, value.failed_ranges_next_cursor)
        val page = failedRangePage?.takeIf { it.operation_id == value.operation_id &&
            it.operation_epoch == value.operation_epoch && it.view_revision == value.view_revision }
        failedRangePage = page
        operation = if (page != null) value.copy(failed_ranges = page.entries, failed_ranges_next_cursor = page.next_cursor) else value
        observe(value.server_now_ms)
        if (value.state == "stopped") { intent.stopped = true; receipt = null }
        status = when {
            intent.stopped -> "Stopped — cached captions remain available. Resume requires an explicit action."
            value.state == "failed" -> "Failed: ${value.reason ?: "unknown reason"}"
            value.source_check.state == "checking" -> "Checking source: ${value.source_check.bytes_read} / ${value.source_check.total_bytes} bytes"
            value.active_job != null -> value.active_job.phase.replace('_', ' ')
            else -> value.state.replace('_', ' ')
        }
    }

    private fun acceptReceipt(value: CaptionReceipt, explicitResume: Boolean = false): Boolean {
        require(value.version == 1); captionCounter(value.operation_epoch); captionCounter(value.view_revision)
        value.demand_generation?.let { captionCounter(it) }
        require(intent.operationId == null || intent.operationId == value.operation_id)
        val currentView = viewFence.acceptCurrent(value.operation_id, value.operation_epoch, value.view_revision)
        if (!currentView && operation?.operation_epoch != value.operation_epoch) return false
        val existing = receipt
        if (existing?.demand_id == value.demand_id && existing?.demand_generation != null && value.demand_generation != null &&
            captionCounter(value.demand_generation) < captionCounter(existing.demand_generation)) return false
        if (currentView) operation = operation?.copy(operation_epoch = value.operation_epoch, view_revision = value.view_revision,
            state = value.state ?: operation!!.state)
        receipt = value.takeIf { intent.acceptDemand(value.state, explicitResume) }
        receipt?.let { accepted ->
            val acceptedPlayback = playbackId
            intent.producer.retain(owner) { releaseReceipt(accepted, acceptedPlayback) }
        }
        observe(value.server_now_ms); lastHeartbeat = SystemClock.elapsedRealtime()
        return true
    }
    private suspend fun heartbeat(g: Long) {
        // A seek never rewrites an uncertain request. Resolve its exact body,
        // then serially publish the current traversal on the acknowledged generation.
        repeat(2) {
            val r = receipt ?: return; val demand = r.demand_id ?: return
            val op = operation
            if (!current(g) || intent.stopped || op?.state == "stopped") return
            val epoch = op?.operation_epoch ?: r.operation_epoch
            val body = heartbeatAttempt.request(r, epoch) {
                CaptionDemand(1, UUID.randomUUID().toString(), expires(), epoch,
                    nextCaptionCounter(requireNotNull(r.demand_generation)), playbackId, position(), rate(),
                    !player().playWhenReady, intent.continuity.id)
            }
            var ambiguous = true
            try {
                val value = captionRead<CaptionReceipt>(api().captionDemand(r.operation_id, demand, body))
                captionReceiptContinuity(value, body.continuity_id)
                ambiguous = false
                if (!current(g) || intent.stopped || receipt !== r || !acceptReceipt(value)) return
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                ambiguous = captionActionAmbiguous(e)
                if (!intent.stopped && receipt === r) throw e
                return
            } finally { heartbeatAttempt.finish(body, ambiguous) }
            if (body.continuity_id == intent.continuity.id) return
        }
        renewalWake.trySend(Unit)
    }

    private suspend fun readWindow(assetId: String, chosen: CaptionBinding, g: Long) {
        require(captionHash(assetId))
        val at = position()
        val manifestRequestedAt = SystemClock.elapsedRealtime()
        val reusableLease = lastLease.takeIf { lastManifestAsset == assetId && lastLeaseDeadline > manifestRequestedAt + 5_000 }
        val manifest = captionRead<CaptionManifest>(api().captionManifest(assetId, at, reusableLease))
        if (!current(g)) return
        require(manifest.version == 1 && manifest.asset_id == assetId && manifest.file_id == fileId.toString() &&
            manifest.profile_id == chosen.profile.profile_id && manifest.audio_index == chosen.audio.audio_ordinal &&
            manifest.spoken_language == chosen.selection.spokenLanguage && manifest.output_language == chosen.outputLanguage &&
            manifest.timeline_contract == chosen.options.timeline_contract && captionHash(manifest.content_revision) &&
            manifest.content_revision == operation?.content_revision &&
            manifest.snapshot.revision == manifest.manifest_revision && manifest.snapshot.expires_in_ms in 1..30_000 &&
            manifest.operation.operation_id == intent.operationId) { "Invalid caption manifest identity" }
        captionCounter(manifest.manifest_revision); captionCounter(manifest.view_revision); captionCounter(manifest.operation.epoch)
        if (!viewFence.acceptCurrent(manifest.operation.operation_id, manifest.operation.epoch, manifest.view_revision)) return
        if (lastManifestAsset == assetId) lastManifestRevision?.let {
            require(captionCounter(manifest.manifest_revision) >= captionCounter(it)) { "Stale caption revision" }
        }
        operation = operation?.copy(operation_epoch = manifest.operation.epoch, view_revision = manifest.view_revision,
            state = manifest.operation.state, reason = manifest.operation.reason)
        val leaseDeadline = manifestRequestedAt + manifest.snapshot.expires_in_ms
        if (manifest.operation.state == "stopped") { intent.stopped = true; receipt = null; status = "Stopped — cached captions remain available" }
        val refs = manifest.segments.toMutableList()
        require(refs.size <= 64)
        var cursor = manifest.segment_next_cursor
        val cursors = mutableSetOf<String>()
        while (cursor != null) {
            require(cursors.size < 8 && cursors.add(cursor) && cursor.length <= 4096)
            val page = captionRead<CaptionSegmentPage>(api().captionSegments(assetId, cursor, manifest.snapshot.lease_id))
            if (!current(g)) return
            require(page.version == 1 && page.asset_id == assetId && page.snapshot_revision == manifest.manifest_revision && page.entries.size <= 64)
            refs += page.entries; require(refs.size <= 64); cursor = page.next_cursor
        }
        require(refs.map { it.segment_id }.distinct().size == refs.size) { "Duplicate caption segment reference" }
        val segments = mutableListOf<CaptionSegment>()
        val nextCache = linkedMapOf<String, CaptionSegment>()
        var windowBytes = 0L
        for (ref in refs.distinctBy { it.segment_id }) {
            require(captionHash(ref.segment_id) && captionHash(ref.digest) && ref.start_ms >= 0 && ref.end_ms > ref.start_ms && ref.end_ms <= chosen.options.duration_ms)
            if (SystemClock.elapsedRealtime() >= leaseDeadline) throw IOException("Caption read lease expired")
            val key = "${ref.segment_id}:${ref.digest}"
            val segment = cachedSegments[key] ?: run {
                val bytes = captionBytes(api().captionSegment(assetId, ref.segment_id, manifest.snapshot.lease_id), 16_384)
                if (!current(g)) return
                verifiedCaptionSegment(bytes, ref, manifest, chosen)
            }
            require(segment.provenance.source_sha256 == manifest.content_revision &&
                captionCounter(segment.provenance.operation_epoch) <= captionCounter(manifest.operation.epoch))
            windowBytes += segment.cues.sumOf { it.text.toByteArray().size + 128 } + 512
            require(windowBytes <= 524_288) { "Caption segment window exceeds its limit" }
            nextCache[key] = segment
            segments += segment
        }
        if (SystemClock.elapsedRealtime() >= leaseDeadline) throw IOException("Caption read lease expired")
        if (!current(g)) return
        window.replace(segments, at, chosen.options.duration_ms)
        cachedSegments = nextCache; lastManifestAsset = assetId; lastManifestRevision = manifest.manifest_revision
        lastLease = manifest.snapshot.lease_id; lastLeaseDeadline = leaseDeadline
        leaseExpiry?.cancel()
        leaseExpiry = scope.launch {
            delay((leaseDeadline - SystemClock.elapsedRealtime()).coerceAtLeast(0))
            if (current(g) && lastLeaseDeadline == leaseDeadline && lastLease == manifest.snapshot.lease_id) clearCues()
        }
        reconcile()
    }

    fun viewerSeek() {
        if (!current(generation)) return
        intent.continuity.viewerSeekRequested(); renewalWake.trySend(Unit)
    }
    fun executionAttached() {
        if (current(generation) && intent.continuity.executionAttached()) renewalWake.trySend(Unit)
    }
    fun rendererDiscontinuity(reason: Int) {
        if (!current(generation)) return
        val before = intent.continuity.discontinuitySerial
        intent.continuity.rendererDiscontinuity(reason)
        if (before != intent.continuity.discontinuitySerial) renewalWake.trySend(Unit)
        reconcile()
    }
    fun <T> ownedMediaMutation(block: () -> T): T = intent.continuity.ownedMediaMutation(block)

    /** Reuses Controller's owner sample; cue delivery itself stays on Media3's timeline. */
    fun clockChanged() {
        if (!authorized()) { generation++; job?.cancel(); clearCues(); return }
        val owner = player()
        val origin = playerPosition(14_400_000)
        if (clockOwner !== owner || clockOrigin != origin) {
            clockOwner = owner; clockOrigin = origin; reconcile()
        }
    }

    /** Called by the existing player's seek, rate, pause and item events. */
    fun reconcile() {
        boundary?.cancel(); boundary = null
        if (!current(generation) || selected == null) { text = ""; return }
        val g = generation
        val at = position() - intent.offsetMs
        text = window.textAt(at)
        val next = window.nextBoundary(at) ?: return
        val owner = player()
        val local = playerPosition(next + intent.offsetMs)
        if (local <= owner.currentPosition) return
        boundary = owner.createMessage { _, _ ->
            if (current(g) && owner === player()) reconcile()
        }.setLooper(owner.applicationLooper).setPosition(local).setDeleteAfterDelivery(true).send()
    }
    fun setOffset(ms: Long) { if (!current(generation)) return; intent.offsetMs = ms.coerceIn(-15_000, 15_000); reconcile() }
    private fun clearCues() { leaseExpiry?.cancel(); leaseExpiry = null; boundary?.cancel(); boundary = null; window.clear(); cachedSegments = emptyMap(); text = "" }

    fun stop() {
        if (!current(generation) || options?.permissions?.stop != true) return
        actions.stop(); intent.requestStop(); intent.stopped = true
        status = "Stopping generation…"
        if (actions.inFlight || stopping) return
        if (intent.mutations.pending?.request?.create != null && operation == null) { selected?.let(::select); return }
        if (intent.mutations.pending?.request?.retry != null) { resume(resolveOnly = true); return }
        val op = operation
        if (op?.state == "stopped") { status = "Stopped — cached captions remain available"; return }
        val id = op?.operation_id ?: receipt?.operation_id ?: return
        val epoch = op?.operation_epoch ?: receipt?.operation_epoch ?: return
        val g = generation
        scope.launch { stopOperation(id, epoch, g) }
    }
    private suspend fun stopOperation(id: String, epoch: String, g: Long) {
        if (stopping) return
        val stopRevision = actions.stopRevision
        stopping = true
        try {
            val answer = captionRead<CaptionReceipt>(api().stopCaptions(id,
                CaptionStop(1, UUID.randomUUID().toString(), expires(), epoch)))
            if (current(g) && stopRevision == actions.stopRevision && acceptReceipt(answer)) {
                receipt = null; status = "Stopped — cached captions remain available"
            }
        } catch (e: CancellationException) { throw e }
        catch (_: Exception) {
            if (current(g) && stopRevision == actions.stopRevision && operation?.operation_epoch == epoch)
                status = "Stop could not be confirmed. Refresh before resuming."
        } finally { stopping = false }
    }
    fun retry() = resume(resolveOnly = false)
    fun retry(target: CaptionRetryTarget) = resume(resolveOnly = false, target = target)
    fun loadMoreFailedRanges() {
        if (!current(generation)) return
        val op = operation ?: return
        val cursor = op.failed_ranges_next_cursor ?: return
        val lease = lastLease ?: return
        if (loadingFailures || SystemClock.elapsedRealtime() >= lastLeaseDeadline) return
        loadingFailures = true
        val g = generation
        scope.launch {
            try {
                val page = captionRead<CaptionFailedRangePage>(api().captionFailedRanges(op.operation_id, cursor, lease))
                if (!current(g) || operation?.operation_epoch != op.operation_epoch || operation?.view_revision != op.view_revision ||
                    lastLease != lease || SystemClock.elapsedRealtime() >= lastLeaseDeadline) return@launch
                require(page.version == 1 && page.operation_id == op.operation_id && page.operation_epoch == op.operation_epoch && page.view_revision == op.view_revision)
                captionValidateFailures(page.entries, page.next_cursor)
                failedRangePage = page
                operation = operation?.copy(failed_ranges = page.entries, failed_ranges_next_cursor = page.next_cursor)
            } catch (e: CancellationException) { throw e }
            catch (_: Exception) { if (current(g)) status = "Failed ranges changed; refresh current status before retrying." }
            finally { if (current(g)) loadingFailures = false }
        }
    }
    /** Explicit stale Retry refreshes observations only; it never creates or redirects work. */
    private fun refreshStatus() {
        val chosen = binding ?: return
        val id = intent.operationId ?: return
        if (refreshingStatus) return
        refreshingStatus = true
        val g = generation
        scope.launch {
            try { withTimeout(12_000) { observeOperation(id, chosen, g) } }
            catch (e: CancellationException) { if (e !is TimeoutCancellationException) throw e }
            catch (e: Exception) {
                if (current(g) && !captionReadRetryable(e)) {
                    readRetired = true; job?.cancel(); clearCues(); releaseDemand()
                    status = e.message ?: "Caption authority changed; reopen this source."
                }
            }
            finally { if (current(g)) refreshingStatus = false }
        }
    }
    private fun resume(resolveOnly: Boolean, target: CaptionRetryTarget? = null) {
        if (actions.inFlight || stopping || !current(generation)) return
        val op = operation
        if (op == null) {
            if (!resolveOnly && createRequest == null) intent.stopped = false
            selected?.let(::select); return
        }
        if (options?.permissions?.resume != true) return
        val pending = intent.mutations.pending?.request?.retry ?: retryRequest?.second
        if (pending != null && target != null && pending.target != target) {
            status = "Resolve the previous retry before choosing another failed range."; return
        }
        val chosenTarget = pending?.target ?: target ?: runCatching { captionRetryTarget(op) }.getOrElse {
            status = "Choose a current failed range to retry."; return
        }
        if (pending == null && !captionRetryTargetCurrent(chosenTarget, op)) {
            status = "Failed ranges changed; refresh current status before retrying."; refreshStatus(); return
        }
        val chosen = binding ?: return
        val key = "retry:$chosenTarget"
        val owner = actions.begin(key) ?: return
        val body = pending ?: retryRequest?.takeIf { it.first == owner.requestId }?.second ?: captionRetryAttempt(chosenTarget,
            owner.requestId, expires(), playbackId, position(), rate(), !player().playWhenReady, intent.continuity.id)
        retryRequest = owner.requestId to body
        val endpoint = api()
        intent.mutations.install(this@ProgressiveCaptions.owner,
            CaptionProducerMutations.Pending(CaptionProducerRequest(fileId, chosen, retry = body),
                explicitResume = !resolveOnly, stopRevision = intent.stopRevision, authorized = authorized) {
                val response = body.range?.let { endpoint.retryCaptionRange(body.target.operationId, it) }
                    ?: endpoint.retryCaptions(body.target.operationId, requireNotNull(body.operation))
                captionRead<CaptionReceipt>(response).also {
                    captionReceiptContinuity(it, body.range?.continuity_id ?: body.operation?.continuity_id)
                }
            })
        val g = generation
        scope.launch {
            var ambiguous = false
            try {
                if (!settleProducer(chosen, g, resolveOnly)) return@launch
                val value = receipt ?: return@launch
                if (job?.isActive != true) binding?.let { chosen ->
                    job = scope.launch {
                        try { pollOperation(value.operation_id, chosen, g) }
                        catch (e: CancellationException) { throw e }
                        catch (e: Exception) { if (current(g)) status = e.message ?: "Caption status unavailable" }
                    }
                }
            } catch (e: CancellationException) { throw e }
            catch (e: Exception) {
                ambiguous = captionActionAmbiguous(e)
                if (current(g) && actions.owns(owner) && refusalStatusOf(e) == 409) refreshStatus()
                if (current(g) && actions.owns(owner) && !actions.stoppedSince(owner) &&
                    operation?.operation_epoch == op.operation_epoch && operation?.view_revision == op.view_revision)
                    status = e.message ?: "Resume failed; refresh current status"
            } finally {
                val finished = actions.finish(owner, ambiguous)
                if (finished && !ambiguous) retryRequest = null
                if (finished && ambiguous && current(g) && actions.stoppedSince(owner) && !resolveOnly)
                    resume(resolveOnly = true) // One exact replay, only to carry the already requested Stop.
            }
        }
    }
    fun off() {
        if (!active || !intent.producer.owns(owner)) return
        intent.endTraversal()
        intent.captionChoiceChanged()
        intent.selection = null; intent.operationId = null; intent.stopped = false; intent.audioOrdinal = null
        generation++; actions.invalidate(); createRequest = null; retryRequest = null
        job?.cancel(); job = null; readRetired = false; failedRangePage = null; loadingFailures = false; refreshingStatus = false; clearCues(); binding = null; operation = null; status = "Captions Off"
        releaseDemand()
    }
    private fun releaseReceipt(value: CaptionReceipt, ownerPlaybackId: String = playbackId) {
        if (!authorized()) return
        val demand = value.demand_id ?: return
        val generation = value.demand_generation ?: return
        val body = CaptionRelease(1, UUID.randomUUID().toString(), expires(), value.operation_epoch,
            nextCaptionCounter(generation), ownerPlaybackId)
        cleanupScope.launch {
            if (!authorized()) return@launch
            try { captionBytes(api().releaseCaptions(value.operation_id, demand, body)) }
            catch (e: CancellationException) { throw e }
            catch (_: Exception) { /* A lost release expires after the bounded server demand TTL. */ }
        }
    }
    private fun releaseDemand(preserveRetained: Boolean = false) {
        heartbeatAttempt.clear()
        receipt = null
        if (intent.producer.owns(owner) && !preserveRetained) intent.producer.retireDemand()
    }
    fun release() {
        active = false; generation++; actions.invalidate(); job?.cancel(); job = null; clearCues()
        heartbeatAttempt.clear(); receipt = null; intent.producer.release(owner)
    }
}
