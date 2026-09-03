@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.datasource.DataSource
import androidx.media3.datasource.DataSpec
import androidx.media3.datasource.HttpDataSource
import androidx.media3.datasource.TransferListener
import tv.plurx.app.data.HlsStart
import kotlin.math.roundToLong

internal const val MEDIA_ORIGIN_HEADER = "X-Plurx-Media-Origin-Ms"

/** Resolve an HLS item's local-time-zero point onto the source timeline. */
internal fun sessionMediaOriginMs(hls: HlsStart): Long = when {
    hls.vod -> 0L
    hls.media_origin_ms != null -> hls.media_origin_ms.coerceAtLeast(0L)
    hls.start_seconds.isFinite() ->
        (hls.start_seconds * 1_000.0).roundToLong().coerceAtLeast(0L)
    else -> 0L
}

internal data class SessionPlaybackTimeline(
    val baseMs: Long,
    val attachPositionMs: Long,
)

/** Map a freshly-created HLS session onto ExoPlayer's local timeline. */
internal fun sessionPlaybackTimeline(
    hls: HlsStart,
    requestedStartMs: Long,
): SessionPlaybackTimeline = SessionPlaybackTimeline(
    baseMs = sessionMediaOriginMs(hls),
    attachPositionMs = if (hls.vod) requestedStartMs.coerceAtLeast(0L) else 0L,
)

/** Resolve the player's local clock through the active delivery regime. */
internal fun realMediaPositionMs(
    playerPositionMs: Long,
    directTransport: Boolean,
    sessionIsVod: Boolean,
    progressiveTransport: Boolean,
    progressiveOriginMs: Long,
    sessionBaseMs: Long,
): Long {
    val local = playerPositionMs.coerceAtLeast(0L)
    return when {
        directTransport || sessionIsVod -> local
        progressiveTransport -> progressiveOriginMs.coerceAtLeast(0L) + local
        else -> sessionBaseMs.coerceAtLeast(0L) + local
    }
}

internal fun mediaOriginMsFromHeaders(headers: Map<String, List<String>>): Long? =
    headers.entries
        .firstOrNull { (name, _) -> name.equals(MEDIA_ORIGIN_HEADER, ignoreCase = true) }
        ?.value
        ?.firstNotNullOfOrNull { value -> value.toLongOrNull()?.takeIf { it >= 0L } }

/**
 * Tracks the true source origin of the currently requested progressive remux.
 *
 * Media3 opens the response on a loader thread, so the value is volatile. The
 * expected URI is an epoch: a late response from the stream replaced by a seek
 * cannot overwrite the new item's fallback or resolved origin.
 */
internal class ProgressiveMediaOrigin : TransferListener {
    @Volatile
    private var expectedUri: String? = null

    @Volatile
    private var originMs: Long = 0L

    @Volatile
    private var observedBitsPerSecond: Long? = null
    private var rateWindowStartedNanos: Long = 0L
    private var rateWindowBytes: Long = 0L

    fun begin(uri: String, requestedOriginMs: Long) {
        synchronized(this) {
            expectedUri = uri
            originMs = requestedOriginMs.coerceAtLeast(0L)
            // The rate belongs to the stream that produced it. This object
            // lives for the controller's life while `begin` runs on every
            // stream change, so without this reset the first exchange of a
            // replacement — the only exchange the server's replacement seam
            // reads — reports the *predecessor's* rate, and the first window
            // to close after the switch divides leftover old-stream bytes plus
            // new-stream bytes by a span that includes the reconnect gap,
            // producing a number belonging to neither.
            observedBitsPerSecond = null
            rateWindowStartedNanos = 0L
            rateWindowBytes = 0L
        }
    }

    fun currentOriginMs(): Long = originMs

    fun currentObservedBitsPerSecond(): Long? = observedBitsPerSecond

    internal fun acceptResponse(uri: String, headers: Map<String, List<String>>): Boolean {
        val resolved = mediaOriginMsFromHeaders(headers) ?: return false
        synchronized(this) {
            if (uri != expectedUri) return false
            originMs = resolved
        }
        return true
    }

    override fun onTransferInitializing(
        source: DataSource,
        dataSpec: DataSpec,
        isNetwork: Boolean,
    ) = Unit

    override fun onTransferStart(
        source: DataSource,
        dataSpec: DataSpec,
        isNetwork: Boolean,
    ) {
        if (!isNetwork) return
        synchronized(this) {
            if (rateWindowStartedNanos == 0L) rateWindowStartedNanos = System.nanoTime()
        }
        val http = source as? HttpDataSource ?: return
        acceptResponse(dataSpec.uri.toString(), http.responseHeaders)
    }

    override fun onBytesTransferred(
        source: DataSource,
        dataSpec: DataSpec,
        isNetwork: Boolean,
        bytesTransferred: Int,
    ) {
        if (!isNetwork || bytesTransferred <= 0) return
        synchronized(this) {
            val now = System.nanoTime()
            if (rateWindowStartedNanos == 0L) rateWindowStartedNanos = now
            rateWindowBytes += bytesTransferred
            val elapsed = now - rateWindowStartedNanos
            if (elapsed >= 500_000_000L) {
                observedBitsPerSecond = (rateWindowBytes * 8.0 / (elapsed / 1_000_000_000.0))
                    .toLong().coerceAtLeast(0L)
                rateWindowStartedNanos = now
                rateWindowBytes = 0L
            }
        }
    }

    override fun onTransferEnd(
        source: DataSource,
        dataSpec: DataSpec,
        isNetwork: Boolean,
    ) = Unit
}
