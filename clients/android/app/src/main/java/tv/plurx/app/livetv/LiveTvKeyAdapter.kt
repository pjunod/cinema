package tv.plurx.app.livetv

import androidx.compose.ui.input.key.Key
import androidx.compose.ui.input.key.KeyEvent
import androidx.compose.ui.input.key.KeyEventType
import androidx.compose.ui.input.key.key
import androidx.compose.ui.input.key.onPreviewKeyEvent
import androidx.compose.ui.input.key.type
import androidx.compose.ui.Modifier

/**
 * The one place Live TV knows what a key is called.
 *
 * Sibling of [tv.plurx.app.player.PlayerKeyAdapter], and here for the same
 * reason: every platform key spelling lives in a file that decodes and does
 * nothing else, so the screen can only express itself in the contract's
 * vocabulary and cannot quietly grow an opinion of its own. What each decoded
 * input then *does* is [LiveTvInputPolicy]'s answer, transcribed from
 * `tests/playback/player-input-contract.json`.
 */
internal object LiveTvKeyAdapter {
    /**
     * The contract input this press is, or `null` for a key Live TV does not
     * claim. Key-up is never an input: the contract is written in presses, and
     * routing both edges would double every outcome.
     */
    fun decode(event: KeyEvent): LiveTvContractInput? {
        if (event.type != KeyEventType.KeyDown) return null
        return when (event.key) {
            Key.DirectionLeft -> LiveTvContractInput.Left
            Key.DirectionRight -> LiveTvContractInput.Right
            Key.DirectionUp -> LiveTvContractInput.Up
            Key.DirectionDown -> LiveTvContractInput.Down
            Key.DirectionCenter, Key.Enter, Key.NumPadEnter -> LiveTvContractInput.Select
            Key.Back, Key.Escape -> LiveTvContractInput.Back
            Key.MediaPlayPause, Key.MediaPlay, Key.MediaPause, Key.Spacebar ->
                LiveTvContractInput.PlayPause
            else -> null
        }
    }
}

/**
 * Install Live TV's key handling. The modifier lives here rather than at the
 * call site for the same reason the decoding does: `scripts/player-input-fence`
 * treats the *installation* of key handling as input handling, and one adapter
 * per surface is the whole point of the fence — a second handler that happens
 * to agree with the reducer today is still a second handler.
 *
 * `enabled` is false while nothing is playing and while the activity is in
 * picture-in-picture, where the system owns the remote.
 */
internal fun Modifier.liveTvInputAdapter(
    enabled: Boolean,
    route: (LiveTvContractInput) -> Boolean,
): Modifier = this.onPreviewKeyEvent { event ->
    if (!enabled) return@onPreviewKeyEvent false
    val input = LiveTvKeyAdapter.decode(event) ?: return@onPreviewKeyEvent false
    route(input)
}
