package tv.plurx.app.player

import android.view.KeyEvent
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.composed
import androidx.compose.ui.input.key.onPreviewKeyEvent
import tv.plurx.app.ui.FormFactor

/** The player's only Android-key-code boundary. */
internal object PlayerKeyAdapter {
    fun contractInput(event: KeyEvent): PlayerContractInput? = when (event.keyCode) {
        KeyEvent.KEYCODE_DPAD_LEFT -> PlayerContractInput.Left
        KeyEvent.KEYCODE_DPAD_RIGHT -> PlayerContractInput.Right
        KeyEvent.KEYCODE_DPAD_UP -> PlayerContractInput.Up
        KeyEvent.KEYCODE_DPAD_DOWN -> PlayerContractInput.Down
        KeyEvent.KEYCODE_DPAD_CENTER,
        KeyEvent.KEYCODE_ENTER,
        -> PlayerContractInput.Select
        KeyEvent.KEYCODE_BACK -> PlayerContractInput.Back
        KeyEvent.KEYCODE_MEDIA_PLAY_PAUSE,
        KeyEvent.KEYCODE_MEDIA_PLAY,
        KeyEvent.KEYCODE_MEDIA_PAUSE,
        -> PlayerContractInput.PlayPause
        KeyEvent.KEYCODE_MEDIA_REWIND -> PlayerContractInput.SkipBack
        KeyEvent.KEYCODE_MEDIA_FAST_FORWARD -> PlayerContractInput.SkipForward
        else -> null
    }
}

/**
 * Which routing table this device follows. A television is a ten-foot surface
 * whatever is plugged into it; a phone or tablet is touch, keyboard attached
 * or not.
 */
internal fun playerInputSurfaceFor(formFactor: FormFactor): PlayerInputSurface =
    if (formFactor == FormFactor.Television) PlayerInputSurface.TenFoot else PlayerInputSurface.Touch

/** The shipped preview-phase bridge used by online and offline players. */
internal fun Modifier.playerInputAdapter(
    surface: PlayerInputSurface = PlayerInputSurface.TenFoot,
    state: () -> PlayerInputState,
    apply: (PlayerInputOutcome, PlayerContractInput, Int) -> Boolean,
): Modifier = composed {
    var repeatedInput by remember { mutableStateOf<PlayerContractInput?>(null) }
    var repeatCount by remember { mutableIntStateOf(0) }
    this.onPreviewKeyEvent { event ->
        val native = event.nativeKeyEvent
        val input = PlayerKeyAdapter.contractInput(native) ?: return@onPreviewKeyEvent false
        if (native.action == KeyEvent.ACTION_UP) {
            repeatedInput = null
            repeatCount = 0
            return@onPreviewKeyEvent false
        }
        if (native.action != KeyEvent.ACTION_DOWN) return@onPreviewKeyEvent false
        if (repeatedInput != input) {
            repeatedInput = input
            repeatCount = 0
        } else {
            repeatCount = native.repeatCount
        }
        apply(PlayerInputPolicy.route(surface, state(), input), input, repeatCount)
    }
}
