@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Timeline
import androidx.media3.exoplayer.source.MediaPeriod
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.WrappingMediaSource
import androidx.media3.exoplayer.upstream.Allocator
import java.util.IdentityHashMap
import java.util.concurrent.ConcurrentHashMap

/** Selection binding follows the real Media3 period identity, including its
 * window sequence, rather than a process-wide current family. */
internal class ContinuousSourceRegistry {
    private val bindings = ContinuousPeriodBindings<ContinuousVideoSelection>()
    fun owns(selection: ContinuousVideoSelection): Boolean = bindings.owns(selection)
    fun binding(id: MediaSource.MediaPeriodId, timeline: Timeline): ContinuousVideoSelection? {
        val index = timeline.getIndexOfPeriod(id.periodUid)
        if (index == C.INDEX_UNSET) return null
        val period = timeline.getPeriod(index, Timeline.Period())
        val sourceKey = timeline.getWindow(period.windowIndex, Timeline.Window()).mediaItem.mediaId
        return bindings.binding(id, sourceKey)
    }

    fun source(delegate: MediaSource, selection: ContinuousVideoSelection, periodReleaseRequested: () -> Unit): MediaSource =
        object : WrappingMediaSource(delegate) {
            private val sourceKey = delegate.mediaItem.mediaId.also { require(it.startsWith("cq-")) }
            private val periods = IdentityHashMap<MediaPeriod, MediaSource.MediaPeriodId>()
            override fun createPeriod(id: MediaSource.MediaPeriodId, allocator: Allocator, startPositionUs: Long): MediaPeriod {
                bindings.bind(id, sourceKey, selection)
                try {
                    return mediaSource.createPeriod(id, allocator, startPositionUs).also { periods[it] = id }
                } catch (error: Exception) { bindings.release(id, selection); throw error }
            }
            override fun releasePeriod(mediaPeriod: MediaPeriod) {
                val id = checkNotNull(periods.remove(mediaPeriod)) { "Continuous period release owner" }
                try { mediaSource.releasePeriod(mediaPeriod) } finally { bindings.release(id, selection) }
                // HLS queue release happens asynchronously after this request.
                if (periods.isEmpty()) periodReleaseRequested()
            }
        }
}

/** MediaSourceList wraps/masks child UIDs. The source's unique media ID and
 * complete period sequence/ad coordinates bind its public callback identity. */
internal class ContinuousPeriodBindings<T : Any> {
    private data class Binding<T>(val sourceKey: String, val owner: T)
    private val entries = ConcurrentHashMap<MediaSource.MediaPeriodId, Binding<T>>()
    @Synchronized fun bind(id: MediaSource.MediaPeriodId, sourceKey: String, owner: T) {
        check(entries.size < 16) { "Continuous active period bound" }
        check(entries.putIfAbsent(id, Binding(sourceKey, owner)) == null) { "Continuous period identity already owned" }
    }
    fun binding(id: MediaSource.MediaPeriodId, sourceKey: String): T? = entries.entries.singleOrNull {
        it.value.sourceKey == sourceKey && it.key.copyWithPeriodUid(id.periodUid) == id
    }?.value?.owner
    fun owns(owner: T): Boolean = entries.values.any { it.owner === owner }
    fun release(id: MediaSource.MediaPeriodId, owner: T) {
        entries.computeIfPresent(id) { _, value -> if (value.owner === owner) null else value }
    }
}
