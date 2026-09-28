@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import androidx.media3.common.PlaybackException
import androidx.media3.datasource.HttpDataSource
import androidx.media3.exoplayer.ExoPlaybackException
import androidx.media3.exoplayer.hls.playlist.HlsPlaylistTracker
import java.io.IOException
import java.net.SocketTimeoutException
import java.net.UnknownHostException
import tv.plurx.app.player.PlaybackClientLog

/** Static labels only: exception names and messages are not trusted payloads. */
internal fun liveTvDiagnosticCauseTypes(error: Throwable?): List<String> {
    val seen = java.util.IdentityHashMap<Throwable, Boolean>()
    val types = mutableListOf<String>()
    var next = error
    while (next != null && types.size < 4 && seen.put(next, true) == null) {
        types += when (next.javaClass) {
            PlaybackException::class.java -> "PlaybackException"
            ExoPlaybackException::class.java -> "ExoPlaybackException"
            HlsPlaylistTracker.PlaylistStuckException::class.java -> "HlsPlaylistTracker.PlaylistStuckException"
            HlsPlaylistTracker.PlaylistResetException::class.java -> "HlsPlaylistTracker.PlaylistResetException"
            HttpDataSource.InvalidResponseCodeException::class.java -> "HttpDataSource.InvalidResponseCodeException"
            SocketTimeoutException::class.java -> "SocketTimeoutException"
            UnknownHostException::class.java -> "UnknownHostException"
            IOException::class.java -> "IOException"
            else -> "other"
        }
        next = next.cause
    }
    return types
}

internal enum class LiveTvTerminalTrigger { PLAYER_ERROR, PLAYER_ENDED, NO_PROGRESS, LEASE_ERROR }
internal enum class LiveTvTerminalAction { STOPPED, COMPATIBILITY_RETRY }

/** One attachment's observations, using the device's elapsedRealtime clock. */
internal class LiveTvTerminalDiagnostics(private val now: () -> Long) {
    private val attachedAt = now()
    private var firstFrameAt: Long? = null
    private var decoderProgressAt: Long? = null
    private var decoderSampleAt: Long? = null
    private var frames = 0
    private var emitted = false

    fun firstFrame() {
        if (firstFrameAt == null) firstFrameAt = now()
    }

    /** Reuses the existing five-second heartbeat, not a new sampling loop. */
    fun decoderSample(renderedFrames: Int, playing: Boolean) {
        val observedAt = now()
        if (playing && renderedFrames > 0 && renderedFrames != frames) decoderProgressAt = observedAt
        decoderSampleAt = observedAt
        frames = renderedFrames.coerceAtLeast(0)
    }

    fun terminal(
        trigger: LiveTvTerminalTrigger,
        action: LiveTvTerminalAction,
        observedAt: Long,
        playing: Boolean,
        playbackState: Int,
        positionMs: Long,
        error: Throwable? = null,
    ): PlaybackClientLog? {
        if (emitted) return null
        emitted = true
        val mediaError = error as? PlaybackException
        val errorName = mediaError?.let { PlaybackException.getErrorCodeName(it.errorCode) } ?: "none"
        // Report a recognized cause, never claim this establishes the root cause.
        val cause = if (mediaError != null) mediaError.cause else error
        val knownType = liveTvDiagnosticCauseTypes(cause).lastOrNull { it != "other" }
            ?: if (cause == null) "none" else "other"
        return PlaybackClientLog(
            level = "error",
            event = "live_tv_terminal_diagnostic",
            message = "Android Live TV ${trigger.name} ${action.name} error=$errorName known_type=$knownType",
            method = "live",
            code = mediaError?.errorCode,
            // Both fields stay below the existing server's 200-character caps,
            // even for Long.MAX_VALUE clocks. All times use the same device
            // elapsedRealtime clock (er_ms): t=terminal callback/observation,
            // a=attach, f=first-frame callback, p=decoder progress observation,
            // s=last decoder sample. This does not establish server ordering.
            // The wire ms field is a duration, so it is deliberately omitted.
            detail = listOf(
                "clock=er_ms", "t=$observedAt", "a=$attachedAt", "f=${firstFrameAt ?: "unknown"}",
                "p=${decoderProgressAt ?: "unknown"}", "s=${decoderSampleAt ?: "unknown"}",
                "frames=$frames", "playing=$playing", "state=$playbackState",
                "pos=${positionMs.takeIf { it >= 0 } ?: "unknown"}",
            ).joinToString(" "),
            ua = "Android Media3",
        )
    }
}
