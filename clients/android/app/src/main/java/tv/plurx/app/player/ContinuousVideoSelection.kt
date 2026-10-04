@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.Timeline
import androidx.media3.common.TrackGroup
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.chunk.Chunk
import androidx.media3.exoplayer.source.chunk.MediaChunk
import androidx.media3.exoplayer.source.chunk.MediaChunkIterator
import androidx.media3.exoplayer.trackselection.AdaptiveTrackSelection
import androidx.media3.exoplayer.trackselection.BaseTrackSelection
import androidx.media3.exoplayer.trackselection.ExoTrackSelection
import androidx.media3.exoplayer.upstream.BandwidthMeter
import java.util.IdentityHashMap
import java.util.concurrent.atomic.AtomicReference
import kotlinx.serialization.json.*

/** One verified family for one media period. Only accepted reservations may
 * publish future-load intent. Existing buffered chunks remain Media3-owned. */
internal class ContinuousVideoSelection(family: JsonObject, private val protocol: ContinuousQualityProtocol) {
    private val family = Json.parseToJsonElement(family.toString()).jsonObject.also {
        require(ContinuousQualityWire.family(it) && it.text("mode") == "controlled")
    }
    private val rows = this.family.getValue("video").jsonArray.map { it.jsonObject }
    private val choice = AtomicReference<ReservedVideoChoice?>(null)
    private val supported = AtomicReference<Set<String>>(emptySet())

    fun supportedRenditions(): Set<String> = supported.get()
    private val selections = IdentityHashMap<TrackGroup, ContinuousRenditionSelection>()

    fun publishReserved(throughTick: Long, automatic: Boolean): Boolean {
        if (throughTick !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER) return false
        val ledger = protocol.ledger ?: return false
        if (ledger.obj("attachment")?.get("family_id") != family["family_id"]) return false
        val revision = ledger.number("latest_intent_revision") ?: return false
        val transactions = (ledger["transactions"] as? JsonArray) ?: return false
        val transaction = transactions.mapNotNull { it as? JsonObject }.lastOrNull { tx ->
            tx.number("intent_revision") == revision && tx["intent_superseded"]?.wireBoolean() == false &&
                tx["cancel_requested"]?.wireBoolean() == false && tx.text("state") in setOf("scheduled", "appended", "presented")
        } ?: return false
        val rendition = transaction.text("target_rendition_id") ?: return false
        val row = rows.singleOrNull { it.text("rendition_id") == rendition } ?: return false
        val disposed = transaction["disposed"]?.jsonArray ?: return false
        val reserved = transaction["reserved"]?.jsonArray ?: return false
        if (reserved.none { value ->
                val pin = value.jsonObject
                pin.text("rendition_id") == rendition && pin["timescale"] == row["timescale"] &&
                    pin.number("from_tick")!! <= throughTick && throughTick < pin.number("through_tick")!! &&
                    pin["artifact_id"] !in disposed
            }) return false
        val next = ReservedVideoChoice(rendition, revision, requireNotNull(transaction.text("transaction_id")), automatic)
        while (true) {
            val previous = choice.get()
            if (previous != null && (previous.intentRevision > revision ||
                    previous.intentRevision == revision && (previous.renditionId != rendition || previous.transactionId != next.transactionId))) return false
            if (choice.compareAndSet(previous, next)) return true
        }
    }

    /** Called only on Media3's selection thread. Repeated invalidations retain
     * the same object when the supported group and indices are unchanged. */
    fun selection(definition: ExoTrackSelection.Definition): ContinuousRenditionSelection? {
        val wanted = choice.get() ?: return null
        val group = definition.group
        if (group.type != C.TRACK_TYPE_VIDEO) return null
        val mapping = definition.tracks.associateWith { index ->
            val format = group.getFormat(index)
            rows.singleOrNull { matches(format, it) }?.text("rendition_id") ?: return null
        }
        if (mapping.values.toSet().size != mapping.size) return null
        supported.set(mapping.values.toSet())
        if (wanted.renditionId !in mapping.values) return null
        val old = selections[group]
        if (old != null && old.matches(definition.tracks, definition.type)) return old
        if (selections.size >= 8 && old == null) return null
        return ContinuousRenditionSelection(group, definition.tracks, definition.type, mapping, wanted) { choice.get() }
            .also { selections[group] = it }
    }

    private fun matches(format: Format, row: JsonObject): Boolean = format.sampleMimeType == MimeTypes.VIDEO_H264 &&
        format.codecs == row.text("codec") && format.width.toLong() == row.number("width") &&
        format.height.toLong() == row.number("height") && format.drmInitData == null
}

internal data class ReservedVideoChoice(val renditionId: String, val intentRevision: Long, val transactionId: String, val automatic: Boolean)

/** Changing the index inside this object avoids HLS selectTracks' buffer reset.
 * There is no bandwidth-driven second owner, buffer discard or load cancellation. */
internal class ContinuousRenditionSelection(
    group: TrackGroup, tracks: IntArray, type: Int,
    private val renditions: Map<Int, String>, initial: ReservedVideoChoice,
    private val reservedChoice: () -> ReservedVideoChoice?,
) : BaseTrackSelection(group, tracks, type) {
    private val definitionTracks = tracks.toSet()
    private var selected = initial
    private var selectedIndex = requireNotNull(renditions.entries.singleOrNull { it.value == initial.renditionId })
        .let { indexOf(it.key) }.also { require(it >= 0) }

    fun matches(tracks: IntArray, type: Int): Boolean = definitionTracks == tracks.toSet() && getType() == type
    override fun getSelectedIndex(): Int = selectedIndex
    override fun getSelectionReason(): Int = if (selected.automatic) C.SELECTION_REASON_ADAPTIVE else C.SELECTION_REASON_MANUAL
    override fun getSelectionData(): Any = selected.transactionId
    override fun updateSelectedTrack(playbackPositionUs: Long, bufferedDurationUs: Long, availableDurationUs: Long,
        queue: MutableList<out MediaChunk>, mediaChunkIterators: Array<out MediaChunkIterator>) {
        applyReservedChoice()
    }
    override fun evaluateQueueSize(playbackPositionUs: Long, queue: MutableList<out MediaChunk>): Int = queue.size
    override fun shouldCancelChunkLoad(playbackPositionUs: Long, loadingChunk: Chunk, queue: MutableList<out MediaChunk>): Boolean = false
    // A zero-byte stale load may yield to an already reserved newer choice.
    // No blacklist is installed: a later explicit reservation may return here.
    override fun excludeTrack(index: Int, exclusionDurationMs: Long): Boolean {
        if (index !in 0 until length) return false
        if (!applyReservedChoice()) return false
        return index != selectedIndex
    }

    private fun applyReservedChoice(): Boolean {
        val next = reservedChoice() ?: return false
        if (next.intentRevision < selected.intentRevision || next.intentRevision == selected.intentRevision && next != selected) return false
        val track = renditions.entries.singleOrNull { it.value == next.renditionId }?.key ?: return false
        val index = indexOf(track)
        if (index < 0) return false
        selectedIndex = index
        selected = next
        return true
    }
}

internal class ContinuousTrackSelectionFactory(
    private val bindingForPeriod: (MediaSource.MediaPeriodId, Timeline) -> ContinuousVideoSelection?,
) : ExoTrackSelection.Factory {
    private val ordinary = AdaptiveTrackSelection.Factory()
    override fun createTrackSelections(definitions: Array<out ExoTrackSelection.Definition?>,
        bandwidthMeter: BandwidthMeter, mediaPeriodId: MediaSource.MediaPeriodId, timeline: Timeline): Array<ExoTrackSelection?> {
        val selections = ordinary.createTrackSelections(definitions, bandwidthMeter, mediaPeriodId, timeline)
        val binding = bindingForPeriod(mediaPeriodId, timeline) ?: return selections
        definitions.forEachIndexed { index, definition ->
            if (definition?.group?.type == C.TRACK_TYPE_VIDEO) {
                selections[index] = checkNotNull(binding.selection(definition)) {
                    "Continuous video has no verified reserved track group"
                }
            }
        }
        return selections
    }
}
