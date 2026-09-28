@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.Player
import androidx.media3.ui.PlayerView

/** The finite screen's M3 listener. Suppliers follow the controller's prepared
 * player swap; its addPlayerListener/removePlayerListener retain ownership. */
internal class PlayerScreenOn(
    private val view: () -> PlayerView?,
    private val player: () -> Player,
    private val isVideo: Boolean,
) : Player.Listener {
    fun sync(target: PlayerView? = view()) {
        val current = player()
        target?.keepScreenOn = isVideo &&
            (current.isPlaying || (current.playWhenReady && current.playbackState == Player.STATE_BUFFERING))
    }

    override fun onIsPlayingChanged(isPlaying: Boolean) = sync()
    override fun onPlaybackStateChanged(playbackState: Int) = sync()
    override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) = sync()
}
