package tv.plurx.app.remote

import android.view.KeyEvent
import androidx.compose.ui.Modifier
import androidx.compose.ui.input.key.onPreviewKeyEvent

/** Observation only: retire phone credit before native routing; never consume,
 * synthesize keys, or decide player behavior. */
internal object RemotePhysicalInput {
    fun retiresCredits(action: Int, keyCode: Int) = action == KeyEvent.ACTION_DOWN && keyCode != KeyEvent.KEYCODE_UNKNOWN
    fun observe(event: KeyEvent, physicalInput: () -> Unit) {
        if (retiresCredits(event.action, event.keyCode)) physicalInput()
    }
    fun observePreview(@Suppress("UNUSED_PARAMETER") event: androidx.compose.ui.input.key.KeyEvent,
                       physicalInput: () -> Unit): Boolean {
        physicalInput()
        return false
    }
}

internal fun Modifier.observeRemotePhysicalKeys(physicalInput: () -> Unit): Modifier =
    onPreviewKeyEvent { RemotePhysicalInput.observePreview(it, physicalInput) }
