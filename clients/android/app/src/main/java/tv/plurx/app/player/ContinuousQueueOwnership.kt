@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.source.SampleQueue
import java.io.IOException
import java.util.IdentityHashMap
import kotlinx.serialization.json.JsonObject

/** Loader-thread inventory. An artifact is appended only after extraction
 * finishes and every expected sample actually advanced its owning queue.
 * Queue retirement and decoder/sink retirement remain separate observations. */
internal class ContinuousQueueOwnership(private val owner: Any) {
    data class Appended(val interval: JsonObject, val transactions: Set<String>, val video: Boolean)
    private data class Span(val queue: SampleQueue, var from: Int, var through: Int, var samples: Long)
    private data class Record(val load: ContinuousLoadContext.Verified, val expected: Long,
        val spans: IdentityHashMap<SampleQueue, Span> = IdentityHashMap(), var accepted: Long = 0,
        var contiguous: Boolean = true, var complete: Boolean = false, var credited: Boolean = false)
    private val records = LinkedHashMap<String, Record>()

    @Synchronized fun accepted(sample: ContinuousHlsExtractorFactory.AcceptedSample) {
        if (sample.load.owner !== owner) return
        val record = record(sample.load)
        val span = record.spans[sample.queue]
        if (span == null) {
            if (record.spans.size >= 8) throw IOException("Continuous artifact queue owner bound")
            record.spans[sample.queue] = Span(sample.queue, sample.before, sample.after, 1)
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
        return record.complete && record.spans.isNotEmpty() && record.spans.values.all { span ->
            synchronized(span.queue) { span.queue.firstIndex >= span.through && span.queue.writeIndex >= span.through }
        }
    }

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
