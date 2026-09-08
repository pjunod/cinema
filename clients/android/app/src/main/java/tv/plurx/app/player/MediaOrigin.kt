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

/**
 * Map a session onto ExoPlayer's local timeline, from its origin alone.
 *
 * A media origin is a media origin whether it came from a create response or
 * from a `prepare` action's `media_origin_ms`, and a prepared successor has no
 * [HlsStart] to read — it has the one number the server sent. Taking the raw
 * origin is what lets both callers share this rather than compute it twice and
 * eventually disagree.
 */
internal fun sessionPlaybackTimeline(
    mediaOriginMs: Long,
    isVod: Boolean,
    requestedStartMs: Long,
): SessionPlaybackTimeline = SessionPlaybackTimeline(
    baseMs = mediaOriginMs.coerceAtLeast(0L),
    attachPositionMs = if (isVod) requestedStartMs.coerceAtLeast(0L) else 0L,
)

/** Map a freshly-created HLS session onto ExoPlayer's local timeline. */
internal fun sessionPlaybackTimeline(
    hls: HlsStart,
    requestedStartMs: Long,
): SessionPlaybackTimeline = sessionPlaybackTimeline(
    mediaOriginMs = sessionMediaOriginMs(hls),
    isVod = hls.vod,
    requestedStartMs = requestedStartMs,
)

/**
 * Where to attach a prepared successor so its picture is the incumbent's.
 *
 * The successor's session-relative zero maps to [mediaOriginMs] on the source
 * timeline, and [filmPositionMs] is where the viewer actually is — so the
 * difference is the local position to prime, and a successor whose origin is
 * already past the playhead starts at zero rather than negative.
 */
internal fun successorAttachPositionMs(
    mediaOriginMs: Long,
    filmPositionMs: Long,
): Long = (filmPositionMs - mediaOriginMs.coerceAtLeast(0L)).coerceAtLeast(0L)

/**
 * The successor's contiguous runway, back in film time.
 *
 * The inverse of [successorAttachPositionMs]: the player reports its own local
 * clock, and every number the protocol carries is film time.
 */
internal fun successorFilmPositionMs(
    mediaOriginMs: Long,
    playerPositionMs: Long,
): Long = mediaOriginMs.coerceAtLeast(0L) + playerPositionMs.coerceAtLeast(0L)

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
 * What this client can pull off the wire, measured over the time it is actually
 * pulling.
 *
 * Separated from [ProgressiveMediaOrigin] because it has no Media3 in it, and
 * the arithmetic is the whole of what was wrong: the previous window divided
 * bytes by *wall clock*, and an HLS player with a full buffer fetches a segment
 * in a burst and then idles for seconds. A window opened during one burst is
 * closed by the first byte of the next, so its denominator is mostly idle time
 * — the number that reached the server oscillated between roughly the link
 * speed and near zero, and the low readings are the ones a floor sees.
 *
 * The server's floor wants `observed >= 2 * delivered`
 * (`headroom_refusal`, `playback_control.rs:1168`), which is a question about
 * *headroom*: could this link carry a second pipeline as well as this one. An
 * average that includes the idle between segments answers a different question
 * — roughly "what is this stream's bitrate" — and answers it with a number that
 * can never be twice itself. So idle time is not counted at all: the window
 * closes after a second of transfer, however long that second takes to
 * accumulate.
 *
 * Not thread-safe on its own; [ProgressiveMediaOrigin] holds its monitor,
 * because Media3 calls a transfer listener from its loader threads.
 */
internal class ThroughputWindow(private val windowNanos: Long = 1_000_000_000L) {
    /**
     * How many transfers are open. Media3 fetches a playlist and a segment
     * concurrently, so this is a count rather than a flag: the link is busy
     * while any of them is, and the clock must not start twice or stop early.
     */
    private var inFlight = 0
    private var busySinceNanos = 0L
    private var activeNanos = 0L
    private var bytes = 0L
    private var bitsPerSecond: Long? = null

    /** A new stream. The rate belongs to the one that produced it. */
    fun begin() {
        inFlight = 0
        busySinceNanos = 0L
        activeNanos = 0L
        bytes = 0L
        bitsPerSecond = null
    }

    fun transferStarted(nowNanos: Long) {
        if (inFlight == 0) busySinceNanos = nowNanos
        inFlight += 1
    }

    fun bytesTransferred(count: Int, nowNanos: Long) {
        if (count <= 0) return
        // A byte arriving with no open transfer is a listener call this class
        // did not see the start of. Counting it against a clock that was never
        // started would divide by zero-or-nonsense, so the transfer is adopted
        // from here instead.
        if (inFlight == 0) transferStarted(nowNanos)
        bytes += count
        close(nowNanos)
    }

    fun transferEnded(nowNanos: Long) {
        if (inFlight == 0) return
        inFlight -= 1
        if (inFlight > 0) return
        activeNanos += (nowNanos - busySinceNanos).coerceAtLeast(0L)
        busySinceNanos = 0L
        close(nowNanos)
    }

    /**
     * The last completed window's rate, or null before one has completed.
     *
     * Null is not a placeholder: the server reads a client that reports no
     * throughput as one it may not offer a preparation to, which is the honest
     * answer before anything has been measured.
     */
    fun bitsPerSecond(): Long? = bitsPerSecond

    private fun close(nowNanos: Long) {
        val open = if (inFlight > 0) (nowNanos - busySinceNanos).coerceAtLeast(0L) else 0L
        val elapsed = activeNanos + open
        if (elapsed < windowNanos) return
        bitsPerSecond = (bytes * 8.0 / (elapsed / 1_000_000_000.0))
            .toLong()
            .coerceIn(0L, PlaybackControl.MAX_OBSERVED_DOWNLOAD_BPS)
        // The next window starts now and inherits nothing. An open transfer
        // keeps its clock running from this instant rather than from its own
        // start, or its first second would be counted twice.
        bytes = 0L
        activeNanos = 0L
        if (inFlight > 0) busySinceNanos = nowNanos
    }
}

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

    private val throughput = ThroughputWindow()

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
            throughput.begin()
        }
    }

    fun currentOriginMs(): Long = originMs

    fun currentObservedBitsPerSecond(): Long? = synchronized(this) { throughput.bitsPerSecond() }

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
        synchronized(this) { throughput.transferStarted(System.nanoTime()) }
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
        synchronized(this) { throughput.bytesTransferred(bytesTransferred, System.nanoTime()) }
    }

    override fun onTransferEnd(
        source: DataSource,
        dataSpec: DataSpec,
        isNetwork: Boolean,
    ) {
        if (!isNetwork) return
        synchronized(this) { throughput.transferEnded(System.nanoTime()) }
    }

    /** Test seam: the window, drivable without Media3's types or a clock. */
    internal fun throughputWindowForTest(): ThroughputWindow = throughput
}
