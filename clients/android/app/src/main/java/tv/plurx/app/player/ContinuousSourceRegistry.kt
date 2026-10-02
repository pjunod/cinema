@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.source.MediaPeriod
import androidx.media3.exoplayer.source.MediaSource
import androidx.media3.exoplayer.source.WrappingMediaSource
import androidx.media3.exoplayer.upstream.Allocator
import java.util.IdentityHashMap
import java.util.concurrent.ConcurrentHashMap

/** Selection binding follows the real Media3 period identity, including its
 * window sequence, rather than a process-wide current family. */
internal class ContinuousSourceRegistry {
    private val bindings = ConcurrentHashMap<MediaSource.MediaPeriodId, ContinuousVideoSelection>()
    fun binding(id: MediaSource.MediaPeriodId): ContinuousVideoSelection? = bindings[id]

    fun source(delegate: MediaSource, selection: ContinuousVideoSelection, periodReleaseRequested: () -> Unit): MediaSource =
        object : WrappingMediaSource(delegate) {
            private val periods = IdentityHashMap<MediaPeriod, MediaSource.MediaPeriodId>()
            override fun createPeriod(id: MediaSource.MediaPeriodId, allocator: Allocator, startPositionUs: Long): MediaPeriod {
                check(bindings.putIfAbsent(id, selection) == null) { "Continuous period identity already owned" }
                try {
                    return mediaSource.createPeriod(id, allocator, startPositionUs).also { periods[it] = id }
                } catch (error: Exception) { bindings.remove(id, selection); throw error }
            }
            override fun releasePeriod(mediaPeriod: MediaPeriod) {
                val id = checkNotNull(periods.remove(mediaPeriod)) { "Continuous period release owner" }
                try { mediaSource.releasePeriod(mediaPeriod) } finally { bindings.remove(id, selection) }
                // HLS queue release happens asynchronously after this request.
                if (periods.isEmpty()) periodReleaseRequested()
            }
        }
}
