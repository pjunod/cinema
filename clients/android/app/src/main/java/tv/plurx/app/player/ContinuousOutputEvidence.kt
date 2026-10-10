@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.Format
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

/** Actual hardware frame and sink-head observations. Subscription belongs to
 * one attachment; neither a scheduled release nor the player clock is proof. */
internal class ContinuousOutputEvidence {
    sealed interface Event {
        data class Frame(val positionUs: Long, val format: Format, val observedAtMs: Long) : Event
        data class NetworkEof(val facts: ContinuousNetworkEof) : Event
        data class AudioHead(val positionUs: Long) : Event
        data object VideoFreed : Event
        data object AudioSinkFlushed : Event
        data object AudioDecoderOwned : Event
        data object AudioDecoderFreed : Event
        data object AudioOutputOwned : Event
        data object AudioOutputFreed : Event
        data object VideoOwned : Event
    }
    private data class Subscription(val owner: Any, val observe: (Event) -> Unit)
    val audioOutputs = ContinuousAudioOutputs()
    val allocations = ContinuousAllocationOwnership()
    private val subscription = AtomicReference<Subscription?>(null)
    fun subscribe(owner: Any, observe: (Event) -> Unit) { check(subscription.compareAndSet(null, Subscription(owner, observe))) }
    fun unsubscribe(owner: Any) {
        while (true) {
            val old = subscription.get() ?: return
            if (old.owner !== owner || subscription.compareAndSet(old, null)) return
        }
    }
    fun owner(): Any? = subscription.get()?.owner
    fun emit(event: Event, owner: Any?) {
        val current = subscription.get()
        if (current != null && current.owner === owner) current.observe(event)
    }
}

/** Playback-thread timestamp provenance survives format changes but not a
 * codec/reset epoch. A callback consumes exactly its queued frame's format. */
internal class ContinuousCodecFrames {
    data class Frame(val positionUs: Long, val format: Format, val owner: Any)
    var epoch = 0L
        private set
    private val frames = LinkedHashMap<Long, Frame>()
    fun reset(): Long { frames.clear(); epoch++; return epoch }
    fun queued(codecTimeUs: Long, offsetUs: Long, format: Format, owner: Any, skippedFlushOffsetUs: Long = 0) {
        val position = preparedItemFramePositionUs(codecTimeUs, offsetUs) ?: return
        val raw = try { Math.addExact(codecTimeUs, skippedFlushOffsetUs) } catch (_: ArithmeticException) { return }
        if (raw < 0) return
        frames[raw] = Frame(position, format, owner)
        while (frames.size > 512) frames.remove(frames.keys.first())
    }
    fun rendered(codecTimeUs: Long, observedEpoch: Long): Frame? = if (observedEpoch == epoch) frames.remove(codecTimeUs) else null
}

/** Diagnostic snapshot of an already accepted hardware presentation. It grants
 * no reservation or presentation authority and never mutates the ledger. */
internal data class ContinuousAcceptedPresentation(
    val candidateId: String, val familyId: String, val renditionId: String,
    val artifactId: String, val transactionId: String, val revision: Long,
    val filmTick: Long, val timescale: Long, val width: Int, val height: Int,
    val observedAtMs: Long,
) {
    fun automaticDetail(): String = "mode=auto route=continuous candidate_id=$candidateId family_id=$familyId " +
        "rendition_id=$renditionId artifact_id=$artifactId transaction_id=$transactionId " +
        "intent_revision=$revision latest_intent_revision=$revision film_tick=$filmTick " +
        "timescale=$timescale width=$width height=$height observed_at_ms=$observedAtMs intent_superseded=false"
}

internal fun continuousAcceptedPresentation(
    row: JsonObject, family: JsonObject, ledger: JsonObject?, transaction: JsonObject,
    frame: ContinuousOutputEvidence.Event.Frame, artifactId: String, filmTick: Long,
    deliveredRevision: Long,
): ContinuousAcceptedPresentation? {
    val current = ledger ?: return null
    val revision = transaction.number("intent_revision") ?: return null
    if (revision <= deliveredRevision || revision != current.number("latest_intent_revision") ||
        transaction["intent_superseded"]?.wireBoolean() != false ||
        transaction.number("first_presented_tick") != filmTick ||
        transaction.text("state") != "presented") return null
    val candidate = row.text("candidate_id")?.takeIf { Regex("[0-9a-f]{32}").matches(it) } ?: return null
    val familyId = family.text("family_id")?.takeIf { Regex("[0-9a-f]{64}").matches(it) } ?: return null
    val rendition = row.text("rendition_id")?.takeIf { Regex("[0-9a-f]{64}").matches(it) } ?: return null
    val id = transaction.text("transaction_id")?.takeIf {
        Regex("[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}").matches(it)
    } ?: return null
    if (!Regex("[0-9a-f]{64}").matches(artifactId) ||
        current.obj("attachment")?.text("family_id") != familyId ||
        transaction.text("target_rendition_id") != rendition ||
        (transaction["appended"] as? JsonArray)?.none { (it as? JsonObject)?.text("artifact_id") == artifactId } != false ||
        frame.format.width !in 1..16384 || frame.format.height !in 1..16384 ||
        frame.format.width.toLong() != row.number("width") || frame.format.height.toLong() != row.number("height")) return null
    val timescale = row.number("timescale") ?: return null
    if (timescale !in 1..ContinuousQualityWire.MAX_SAFE_INTEGER ||
        filmTick !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER ||
        revision !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER ||
        frame.observedAtMs !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER) return null
    return ContinuousAcceptedPresentation(candidate, familyId, rendition, artifactId, id, revision,
        filmTick, timescale, frame.format.width, frame.format.height, frame.observedAtMs)
}

/** Whole HTTP body consumed to upstream EOF and already immutably authorized.
 * A controlled-family object has family membership, not a muxed-candidate nonce. */
internal data class ContinuousNetworkEof(
    val familyId: String, val originSha256: String, val renditionId: String,
    val artifactId: String, val role: String, val bytes: Long,
    val status: Int?, val cacheAbsent: Boolean?, val paced: Boolean?,
    val bodyDurationMs: Long?, val completedAtMs: Long?,
)

internal fun continuousNetworkEof(
    readEof: Boolean, currentOwner: Boolean, familyId: String, originSha256: String,
    resource: ContinuousQualityMedia.Resource?, authorized: ContinuousQualityMedia.Authorized?,
    bytes: Long, network: Boolean, status: Int?, cacheAbsent: Boolean?, paced: Boolean?,
    bodyDurationMs: Long?, completedAtMs: Long?,
): ContinuousNetworkEof? {
    if (!readEof || !currentOwner || !network || resource == null || authorized == null ||
        resource.initialization || resource.role !in setOf("video", "audio") ||
        bytes !in 1..ContinuousQualityMedia.MAX_MEDIA_BYTES.toLong() ||
        (resource.role == "video" && authorized.transactionIds.isEmpty()) ||
        completedAtMs?.let { it !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER } == true) return null
    val artifact = authorized.interval.text("artifact_id") ?: return null
    val rendition = resource.row.text("rendition_id") ?: return null
    if (listOf(familyId, originSha256, artifact, rendition).any { !Regex("[0-9a-f]{64}").matches(it) } ||
        authorized.interval.text("rendition_id") != rendition) return null
    return ContinuousNetworkEof(familyId, originSha256, rendition, artifact, resource.role, bytes,
        status?.takeIf { it in 100..599 }, cacheAbsent, paced, bodyDurationMs?.takeIf { it in 1..120_000L }, completedAtMs)
}

/** Saturating absolute counters belong to one attachment. No timer or player
 * clock invents a frame, and a transport close does not invent EOF. */
internal class ContinuousReadOnlyProbe {
    private var frames = 0L
    private var eofCount = 0L
    private var eofBytes = 0L
    private var videoEofCount = 0L
    private var videoEofBytes = 0L
    private var audioEofCount = 0L
    private var audioEofBytes = 0L
    private var qualifiedVideoEofCount = 0L
    private var qualifiedVideoEofBytes = 0L
    private var lastQualifiedVideo: ContinuousNetworkEof? = null
    private var lastFrameAtMs: Long? = null
    private var lastFramePositionUs: Long? = null
    private var nonprogressingFrames = 0L
    private var maximumFrameGapMs = 0L
    private var longFrameGaps = 0L
    private var counterBoundReached = false
    private var lastEof: ContinuousNetworkEof? = null
    private var lastVideoEof: ContinuousNetworkEof? = null
    private var lastAudioEof: ContinuousNetworkEof? = null
    private fun sum(a: Long, b: Long): Long {
        if (a > ContinuousQualityWire.MAX_SAFE_INTEGER - b) { counterBoundReached = true; return ContinuousQualityWire.MAX_SAFE_INTEGER }
        return a + b
    }
    @Synchronized fun frame(observedAtMs: Long, positionUs: Long) {
        lastFramePositionUs?.let { if (positionUs <= it) nonprogressingFrames = sum(nonprogressingFrames, 1) }
        lastFramePositionUs = positionUs
        frames = sum(frames, 1)
        lastFrameAtMs?.let { previous ->
            if (observedAtMs >= previous) {
                val gap = observedAtMs - previous
                maximumFrameGapMs = maxOf(maximumFrameGapMs, gap)
                if (gap >= 100) longFrameGaps = sum(longFrameGaps, 1)
            }
        }
        lastFrameAtMs = observedAtMs
    }
    @Synchronized fun eof(value: ContinuousNetworkEof) {
        eofCount = sum(eofCount, 1); eofBytes = sum(eofBytes, value.bytes)
        if (value.role == "video") { videoEofCount = sum(videoEofCount, 1); videoEofBytes = sum(videoEofBytes, value.bytes) }
        else { audioEofCount = sum(audioEofCount, 1); audioEofBytes = sum(audioEofBytes, value.bytes) }
        if (value.role == "video" && value.status == 200 && value.cacheAbsent == true && value.paced == false && value.bodyDurationMs != null && value.completedAtMs != null) {
            qualifiedVideoEofCount = sum(qualifiedVideoEofCount, 1); qualifiedVideoEofBytes = sum(qualifiedVideoEofBytes, value.bytes)
            lastQualifiedVideo = value
        }
        if (value.role == "video") lastVideoEof = value
        else if (value.role == "audio") lastAudioEof = value
        lastEof = value
    }
    @Synchronized fun detail(): String = buildString {
        append("frame_count=$frames eof_count=$eofCount eof_bytes=$eofBytes ")
        append("video_eof_count=$videoEofCount video_eof_bytes=$videoEofBytes audio_eof_count=$audioEofCount audio_eof_bytes=$audioEofBytes ")
        append("qualified_video_eof_count=$qualifiedVideoEofCount qualified_video_eof_bytes=$qualifiedVideoEofBytes ")
        append("maximum_frame_gap_ms=$maximumFrameGapMs frame_gaps_ge100ms=$longFrameGaps nonprogressing_frame_count=$nonprogressingFrames counter_bound_reached=$counterBoundReached")
        lastQualifiedVideo?.let {
            append(" qualified_eof_rendition_id=${it.renditionId} qualified_eof_artifact_id=${it.artifactId} qualified_eof_last_bytes=${it.bytes}")
            append(" qualified_eof_body_duration_ms=${it.bodyDurationMs} qualified_eof_completed_monotonic_ms=${it.completedAtMs}")
        }
        fun appendRoleEof(role: String, value: ContinuousNetworkEof?) {
            value?.let {
                append(" ${role}_eof_family_id=${it.familyId} ${role}_eof_origin_sha256=${it.originSha256}")
                append(" ${role}_eof_rendition_id=${it.renditionId} ${role}_eof_artifact_id=${it.artifactId}")
                append(" ${role}_eof_last_bytes=${it.bytes} ${role}_eof_status=${it.status ?: "unknown"}")
                append(" ${role}_eof_cache=${when(it.cacheAbsent) { true -> "absent"; false -> "configured"; null -> "unknown" }}")
                append(" ${role}_eof_paced=${it.paced ?: "unknown"} ${role}_eof_scope=controlled_family")
                append(" ${role}_eof_body_duration_ms=${it.bodyDurationMs ?: "unknown"}")
                append(" ${role}_eof_completed_monotonic_ms=${it.completedAtMs ?: "unknown"}")
            }
        }
        appendRoleEof("video", lastVideoEof)
        appendRoleEof("audio", lastAudioEof)
        lastEof?.let {
            append(" eof_family_id=${it.familyId} eof_origin_sha256=${it.originSha256} eof_rendition_id=${it.renditionId} eof_artifact_id=${it.artifactId}")
            append(" eof_role=${it.role} eof_last_bytes=${it.bytes} eof_status=${it.status ?: "unknown"}")
            append(" eof_cache=${when(it.cacheAbsent) { true -> "absent"; false -> "configured"; null -> "unknown" }}")
            append(" eof_paced=${it.paced ?: "unknown"} eof_scope=controlled_family")
        }
    }
}
