@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
package tv.plurx.app.player

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.PlayerMessage
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlinx.serialization.decodeFromString
import tv.plurx.app.data.*

/** Session, viewer, active import lifecycle and Source revision all belong to this principal. */
internal data class SharedCaptionReadBinding(val context: PlaybackFileContext, val audioOrdinal: Long?, val ordinarySubtitleSelected: Boolean) {
    val identity: String get() = "${context.sourceKey}|${context.sessionId}|$audioOrdinal"
    fun validate() {
        require(context.reference != null && context.sessionId != null && context.lifecycleGeneration != null)
        context.validateSharedReference(context.reference, context.sourceFileId, requireNotNull(context.revision))
    }
}

/** Deliberately read-only: no Local IDs, work routes, mutation method or producer credentials. */
internal fun interface SharedCaptionReadTransport {
    suspend fun read(binding: SharedCaptionReadBinding, resource: String, query: Map<String, String>, maximumBytes: Long): ByteArray
}
internal class RetrofitSharedCaptionReadTransport(private val api: () -> PlurxApi) : SharedCaptionReadTransport {
    override suspend fun read(binding: SharedCaptionReadBinding, resource: String, query: Map<String, String>, maximumBytes: Long): ByteArray {
        binding.validate()
        val path = binding.context.apiPath(resource, query)
        val bytes = captionBytes(api().sharedCaptionReadPath(path), maximumBytes)
        binding.validate() // Account changes during the streamed body cannot adopt its bytes.
        return bytes
    }
}
internal fun sharedCaptionReadVerifying(error: Exception): Boolean =
    error is RefusalException && error.status == 425 && error.code == "source_verifying"
internal data class SharedCaptionWindowRead(val manifest: SharedCaptionManifest, val segments: Map<String, CaptionSegment>)

/** Bounded protocol reader; both the body and its read lease belong to the captured principal. */
internal class SharedCaptionReader(
    private val transport: SharedCaptionReadTransport,
    private val currentPrincipal: () -> String?,
    private val clock: () -> Long = { System.nanoTime() / 1_000_000 },
) {
    private suspend fun read(binding: SharedCaptionReadBinding, resource: String, query: Map<String, String>, maximum: Long): ByteArray {
        binding.validate(); require(currentPrincipal() == binding.identity) { "Shared caption principal retired" }
        val bytes = transport.read(binding, resource, query, maximum)
        binding.validate(); require(currentPrincipal() == binding.identity) { "Shared caption principal changed during read" }
        require(bytes.size <= maximum) { "Shared caption response exceeds its budget" }
        return bytes
    }
    suspend fun catalog(binding: SharedCaptionReadBinding, after: String? = null): SharedCaptionCatalog {
        require(after == null || captionHash(after))
        val value = Net.json.decodeFromString<SharedCaptionCatalog>(read(binding, "captions", after?.let { mapOf("after" to it) } ?: emptyMap(), 65_536).decodeToString(throwOnInvalidSequence = true))
        require(value.version == 1 && value.assets.size <= 16 && (value.next_asset == null || captionHash(value.next_asset)))
        require(value.assets.map { it.asset_id }.distinct().size == value.assets.size)
        value.assets.forEach(::validateSharedCaptionAsset)
        return value
    }
    suspend fun window(binding: SharedCaptionReadBinding, asset: SharedCaptionAsset, positionMs: Long,
        cached: Map<String, CaptionSegment> = emptyMap()): SharedCaptionWindowRead {
        validateSharedCaptionAsset(asset)
        require(binding.audioOrdinal == asset.audio_index && positionMs in 0..asset.duration_ms)
        val started = clock()
        val value = Net.json.decodeFromString<SharedCaptionManifest>(read(binding, "captions/${asset.asset_id}/manifest",
            mapOf("position_ms" to positionMs.toString()), 65_536).decodeToString(throwOnInvalidSequence = true))
        require(value.version == 1 && value.asset_id == asset.asset_id && value.content_revision == asset.content_revision &&
            value.profile_id == asset.profile_id && value.audio_index == asset.audio_index && value.spoken_language == asset.spoken_language &&
            value.output_language == asset.output_language && value.task == asset.task && value.duration_ms == asset.duration_ms &&
            value.snapshot.revision == value.manifest_revision && value.snapshot.expires_in_ms in 1..30_000 &&
            PlaybackFileContext.canonicalUuid(value.snapshot.lease_id) && value.snapshot.lease_id != "00000000-0000-0000-0000-000000000000")
        captionCounter(value.manifest_revision)
        require(value.segments.size <= 8 && value.coverage.size <= 64 && value.segments.map { it.segment_id }.distinct().size == value.segments.size)
        val low = (positionMs - 30_000).coerceAtLeast(0); val high = (positionMs + 90_000).coerceAtMost(asset.duration_ms)
        value.coverage.forEach { require(it.start_ms in 0 until it.end_ms && it.end_ms <= asset.duration_ms && it.end_ms > low && it.start_ms < high) }
        require(value.coverage.zipWithNext().all { (a, b) -> a.end_ms <= b.start_ms })
        require(value.segments.zipWithNext().all { (a, b) -> a.end_ms <= b.start_ms })
        val deadline = started + value.snapshot.expires_in_ms
        val next = linkedMapOf<String, CaptionSegment>()
        var cueCount = 0; var cueBytes = 0
        for (ref in value.segments) {
            require(captionHash(ref.segment_id) && captionHash(ref.digest) && ref.start_ms in 0 until ref.end_ms &&
                ref.end_ms <= asset.duration_ms && ref.end_ms > low && ref.start_ms < high && clock() < deadline)
            val key = "${ref.segment_id}:${ref.digest}"
            val segment = cached[key] ?: verifiedSharedCaptionSegment(read(binding,
                "captions/${asset.asset_id}/segments/${ref.segment_id}", mapOf("lease" to value.snapshot.lease_id), 16_384), ref, asset)
            require(segment.asset_id == asset.asset_id && segment.provenance.profile_id == asset.profile_id &&
                segment.provenance.source_sha256 == asset.content_revision && segment.provenance.model_sha256 == asset.model_sha256)
            cueCount += segment.cues.size; cueBytes += segment.cues.sumOf { it.text.toByteArray().size + 128 }
            require(cueCount <= 1024 && cueBytes <= 524_288)
            next[key] = segment
        }
        require(clock() < deadline && currentPrincipal() == binding.identity)
        return SharedCaptionWindowRead(value, next)
    }
}
internal fun validateSharedCaptionAsset(asset: SharedCaptionAsset) {
    require(captionHash(asset.asset_id) && captionHash(asset.profile_id) && captionHash(asset.model_sha256) && captionHash(asset.content_revision) &&
        asset.model_id.length in 1..64 && asset.audio_index in 0..4095 && asset.duration_ms in 1..14_400_000 &&
        asset.spoken_language.matches(Regex("[a-z]{2,3}")) && asset.task in setOf("transcribe", "translate") &&
        asset.output_language == if (asset.task == "translate") "en" else asset.spoken_language)
}
internal fun verifiedSharedCaptionSegment(bytes: ByteArray, ref: CaptionSegmentRef, asset: SharedCaptionAsset): CaptionSegment {
    require(bytes.size <= 16_384 && captionDigest(bytes) == ref.digest) { "Shared caption checksum mismatch" }
    val value = Net.json.decodeFromString<CaptionSegment>(bytes.decodeToString(throwOnInvalidSequence = true))
    require(value.version == 1 && value.asset_id == asset.asset_id && value.segment_id == ref.segment_id &&
        value.start_ms == ref.start_ms && value.end_ms == ref.end_ms && value.cues.size <= 128 &&
        value.provenance.profile_id == asset.profile_id && value.provenance.model_sha256 == asset.model_sha256 &&
        value.provenance.source_sha256 == asset.content_revision)
    captionCounter(value.provenance.operation_epoch)
    require(value.cues.all { captionHash(it.cue_id) && it.start_ms >= value.start_ms && it.end_ms <= value.end_ms &&
        it.end_ms > it.start_ms && it.text.isNotBlank() && it.text.toByteArray().size <= 4096 &&
        it.text.none { char -> char.isISOControl() && char != '\n' && char != '\t' } })
    return value
}

/** One cue layer on the existing Shared owner; Media3, rather than a wall-clock watchdog, delivers boundaries. */
internal class SharedProgressiveCaptions(
    api: () -> PlurxApi, private val scope: CoroutineScope,
    private val currentBinding: () -> SharedCaptionReadBinding?, private val player: () -> ExoPlayer,
) {
    var assets: List<SharedCaptionAsset> by mutableStateOf(emptyList()); private set
    var selected: SharedCaptionAsset? by mutableStateOf(null); private set
    var nextAsset: String? by mutableStateOf(null); private set
    var text: String by mutableStateOf(""); private set
    var status: String by mutableStateOf("Shared captions read Source-cached text only."); private set
    var offsetMs: Long by mutableStateOf(0L); private set
    private var binding: SharedCaptionReadBinding? = null
    private var generation = 0L
    private var polling: Job? = null
    private var catalogJob: Job? = null
    private var boundary: PlayerMessage? = null
    private var cache = emptyMap<String, CaptionSegment>()
    private var revision: String? = null
    private val window = CaptionWindow()
    private val reader = SharedCaptionReader(RetrofitSharedCaptionReadTransport(api), { currentBinding()?.identity })
    private fun current(g: Long, read: SharedCaptionReadBinding) = generation == g && binding?.identity == read.identity && currentBinding()?.identity == read.identity
    fun bind(value: SharedCaptionReadBinding?) {
        val previous = binding
        if (previous?.identity == value?.identity && previous?.ordinarySubtitleSelected == value?.ordinarySubtitleSelected) return
        generation++; polling?.cancel(); catalogJob?.cancel(); clear()
        binding = value; revision = null; assets = emptyList(); nextAsset = null
        if (value == null || previous?.context?.sourceKey != value.context.sourceKey || previous.audioOrdinal != value.audioOrdinal) selected = null
        val chosen = selected
        if (chosen != null && value != null && !value.ordinarySubtitleSelected) select(chosen)
    }
    fun loadCatalog(more: Boolean = false) {
        val read = binding ?: return
        if (catalogJob?.isActive == true || more && nextAsset == null || assets.size >= 64) return
        val g = generation
        catalogJob = scope.launch {
            while (current(g, read)) {
                try {
                    val page = reader.catalog(read, if (more) nextAsset else null)
                    if (!current(g, read)) return@launch
                    assets = ((if (more) assets else emptyList()) + page.assets.filter { it.audio_index == read.audioOrdinal }).distinctBy { it.asset_id }.take(64)
                    nextAsset = page.next_asset
                    status = when { read.audioOrdinal == null -> "Choose a known source audio track before reading Shared captions."
                        assets.isEmpty() -> "No cached captions for this audio. Ask the Source administrator to generate them."
                        else -> "Choose cached captions. This viewer never starts Source caption work." }
                    return@launch
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) {
                    if (!current(g, read)) return@launch
                    clear()
                    if (!sharedCaptionReadVerifying(e)) { status = "Shared caption choices are unavailable. Refresh or ask the Source administrator."; return@launch }
                    status = "Source is verifying cached caption bytes. Retrying the read; no caption work is requested."
                    delay(3_000)
                }
            }
        }
    }
    fun select(asset: SharedCaptionAsset) {
        val read = binding ?: return
        validateSharedCaptionAsset(asset); require(read.audioOrdinal == asset.audio_index)
        selected = asset; generation++; polling?.cancel(); clear(); revision = null
        if (read.ordinarySubtitleSelected) { status = "Waiting for the existing owner to switch source subtitles Off."; return }
        val g = generation
        polling = scope.launch {
            var failures = 0
            while (current(g, read)) {
                var waitMs = 5_000L
                try {
                    val at = player().currentPosition.coerceIn(0, asset.duration_ms)
                    val value = reader.window(read, asset, at, cache)
                    if (!current(g, read)) return@launch
                    require(revision == null || captionCounter(value.manifest.manifest_revision) >= captionCounter(revision!!))
                    cache = value.segments; revision = value.manifest.manifest_revision
                    window.replace(cache.values.toList(), at, asset.duration_ms); reconcile(); failures = 0
                    status = if (value.manifest.coverage.any { at >= it.start_ms && at < it.end_ms })
                        if (value.manifest.state == "stopped") "Source generation stopped; cached captions remain available." else "Reading Source-cached captions."
                    else "No cached coverage here. Ask the Source administrator; this viewer cannot enqueue work."
                } catch (e: CancellationException) { throw e }
                catch (e: Exception) {
                    if (!current(g, read)) return@launch
                    clear()
                    if (sharedCaptionReadVerifying(e)) { waitMs = 3_000; status = "Source is verifying cached caption bytes. Retrying read-only access." }
                    else { failures = (failures + 1).coerceAtMost(3); waitMs = (5_000L shl failures).coerceAtMost(30_000); status = "Shared cached captions unavailable. Refresh or ask the Source administrator." }
                }
                delay(waitMs)
            }
        }
    }
    fun reconcile(seeked: Boolean = false) {
        if (binding?.identity != currentBinding()?.identity) { bind(currentBinding()); return }
        if (seeked) { selected?.let(::select); return }
        boundary?.cancel(); boundary = null
        if (selected == null || binding?.ordinarySubtitleSelected != false) { text = ""; return }
        val at = player().currentPosition - offsetMs
        text = window.textAt(at)
        val next = window.nextBoundary(at) ?: return
        val owner = player(); val g = generation
        boundary = owner.createMessage { _, _ -> if (g == generation && owner === player()) reconcile() }
            .setLooper(owner.applicationLooper).setPosition(next + offsetMs).setDeleteAfterDelivery(true).send()
    }
    fun setOffset(value: Long) { offsetMs = value.coerceIn(-15_000, 15_000); reconcile() }
    private fun clear() { boundary?.cancel(); boundary = null; cache = emptyMap(); window.clear(); text = "" }
    fun off() { generation++; polling?.cancel(); selected = null; clear(); status = "Shared generated captions Off" }
    fun release() { off(); catalogJob?.cancel(); binding = null; assets = emptyList(); nextAsset = null }
}
