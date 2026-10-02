@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.source.SampleQueue
import java.io.IOException
import java.util.IdentityHashMap
import kotlinx.serialization.json.JsonObject

/** Loader-thread inventory. An artifact is appended only after extraction
 * finishes and every expected sample actually advanced its owning queue.
 * Queue retirement and decoder/sink retirement remain separate observations. */
internal class ContinuousQueueOwnership(private val owner: Any, private val releaseEpochs: (String) -> Pair<Long, Long> = { 0L to 0L }) {
    data class Appended(val interval: JsonObject, val transactions: Set<String>, val video: Boolean)
    private data class Span(val queue: SampleQueue, val epoch: Long, var from: Int, var through: Int, var samples: Long)
    private data class Record(val load: ContinuousLoadContext.Verified, val expected: Long,
        val spans: IdentityHashMap<SampleQueue, Span> = IdentityHashMap(), var accepted: Long = 0,
        var contiguous: Boolean = true, var complete: Boolean = false, var credited: Boolean = false,
        var decoderEpoch: Long = 0, var sinkEpoch: Long = 0)
    private val records = LinkedHashMap<String, Record>()
    private val epochs = ContinuousQueueEpochs()

    @Synchronized fun accepted(sample: ContinuousHlsExtractorFactory.AcceptedSample) {
        if (sample.load.owner !== owner) return
        val epoch = epochs.accepted(sample.queue, sample.before, sample.after)
        val record = record(sample.load)
        if (record.spans.isNotEmpty() && record.spans.values.all { it.epoch < epochs.current(it.queue) }) {
            record.spans.clear()
            record.accepted = 0
            record.contiguous = true
            record.complete = false
            record.credited = false
        }
        val release = releaseEpochs(sample.load.resource.role)
        record.decoderEpoch = maxOf(record.decoderEpoch, release.first)
        record.sinkEpoch = maxOf(record.sinkEpoch, release.second)
        val span = record.spans[sample.queue]
        if (span == null) {
            if (record.spans.size >= 8) throw IOException("Continuous artifact queue owner bound")
            record.spans[sample.queue] = Span(sample.queue, epoch, sample.before, sample.after, 1)
        } else {
            if (span.through != sample.before) record.contiguous = false
            span.through = sample.after
            span.samples++
        }
        record.accepted++
    }

    /** Returns once for a complete physical append. A truncated extraction,
     * skipped sample, duplicate sample or discontinuous queue span cannot
     * authorize a whole-interval acknowledgement. */
    @Synchronized fun completed(load: ContinuousLoadContext.Verified): Appended? {
        if (load.owner !== owner) return null
        val record = records[key(load)] ?: return null
        record.complete = true
        if (record.credited || !record.contiguous || record.accepted != record.expected || record.spans.size != 1) return null
        record.credited = true
        return Appended(record.load.authorized.interval, record.load.authorized.transactionIds, record.load.resource.role == "video")
    }

    /** Actual front removal is observed under the queue monitor. This is only
     * queue release: callers still owe rendered/sink ownership or an actual
     * pipeline release before they may send a disposed receipt. */
    @Synchronized fun queueRetired(rendition: String, artifact: String): Boolean {
        val record = records["$rendition:$artifact"] ?: return false
        return record.spans.isNotEmpty() && record.spans.values.all { span ->
            span.epoch < epochs.current(span.queue) || record.complete && synchronized(span.queue) { span.queue.firstIndex >= span.through && span.queue.writeIndex >= span.through }
        }
    }

    /** A reset retires old queue spans, including partial extraction. Codec
     * and sink release remain independent facts supplied by the caller. */
    @Synchronized fun retiredByReset(load: ContinuousLoadContext.Verified): Pair<Long, Long>? {
        val record = records[key(load)] ?: return null
        return if (record.spans.isNotEmpty() && record.spans.values.all { it.epoch < epochs.current(it.queue) })
            record.decoderEpoch to record.sinkEpoch else null
    }

    @Synchronized fun observeResets() {
        val known = IdentityHashMap<SampleQueue, Unit>()
        records.values.forEach { record -> record.spans.keys.forEach { known[it] = Unit } }
        known.keys.forEach { queue ->
            synchronized(queue) { epochs.empty(queue, queue.firstIndex, queue.readIndex, queue.writeIndex) }
        }
    }

    @Synchronized fun wasAppended(rendition: String, artifact: String): Boolean =
        records["$rendition:$artifact"]?.credited == true

    @Synchronized fun queuedArtifacts(): List<ContinuousLoadContext.Verified> = records.values.map { it.load }

    /** After admission is fenced and loaders are quiescent, queue reset may
     * prove physical removal even for a partially extracted artifact. */
    @Synchronized fun queuesEmpty(): Boolean = records.values.all { record ->
        record.spans.values.all { span -> synchronized(span.queue) {
            span.queue.firstIndex == span.queue.writeIndex && span.queue.readIndex == span.queue.writeIndex
        } }
    }

    /** Forget provenance only after the caller has acknowledged actual disposal. */
    @Synchronized fun disposed(rendition: String, artifact: String) { records.remove("$rendition:$artifact") }

    /** Call only after the source, renderers and audio sink actually release. */
    @Synchronized fun pipelineReleased(): List<ContinuousLoadContext.Verified> = records.values.map { it.load }.also { records.clear() }

    private fun record(load: ContinuousLoadContext.Verified): Record {
        val key = key(load)
        records[key]?.let {
            if (it.load.authorized.interval != load.authorized.interval) throw IOException("Continuous artifact interval changed")
            return it
        }
        if (records.size >= 128) throw IOException("Continuous queue provenance bound")
        val interval = load.authorized.interval
        val extent = requireNotNull(interval.number("through_tick")) - requireNotNull(interval.number("from_tick"))
        val cadence = if (load.resource.role == "video") requireNotNull(load.resource.row.number("frame_ticks")) else 1024L
        val expected = if (load.resource.role == "video") {
            if (extent % cadence != 0L) throw IOException("Continuous video sample grid extent")
            extent / cadence
        } else (extent + cadence - 1) / cadence
        if (expected !in 1..1_000_000) throw IOException("Continuous artifact sample count bound")
        return Record(load, expected).also { records[key] = it }
    }
    private fun key(load: ContinuousLoadContext.Verified) = "${load.resource.rendition}:${load.authorized.interval.text("artifact_id")}" 
}
