@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Player
import androidx.media3.common.Tracks
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import kotlin.coroutines.resume

/**
 * Wait for the playback thread to actually deselect text before reselecting.
 * Merely yielding the UI coroutine lets Media3 coalesce off/on and retain
 * already consumed empty HLS chunks. The item, video and audio stay attached.
 * Call on the player's application thread; a newer intent owns its own restore.
 */
internal suspend fun retryNativeTextRendition(
    player: Player,
    isCurrent: () -> Boolean,
    restore: () -> Unit,
) {
    if (!isCurrent()) return
    var listener: Player.Listener? = null
    try {
        withTimeoutOrNull(2_000) {
            suspendCancellableCoroutine<Unit> { continuation ->
                listener = object : Player.Listener {
                    override fun onTracksChanged(tracks: Tracks) {
                        if (!tracks.isTypeSelected(C.TRACK_TYPE_TEXT) && continuation.isActive) {
                            continuation.resume(Unit)
                        }
                    }
                }.also(player::addListener)
                player.trackSelectionParameters = player.trackSelectionParameters.buildUpon()
                    .clearOverridesOfType(C.TRACK_TYPE_TEXT)
                    .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
                    .build()
                if (!player.currentTracks.isTypeSelected(C.TRACK_TYPE_TEXT) && continuation.isActive) {
                    continuation.resume(Unit)
                }
            }
        }
    } finally {
        listener?.let(player::removeListener)
        // Timeout or cancellation must not strand the current intent disabled.
        if (isCurrent()) restore()
    }
}
