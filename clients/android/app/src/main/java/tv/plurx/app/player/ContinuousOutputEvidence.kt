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
