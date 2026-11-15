package tv.plurx.app.player

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** The codec successfully initialized by one player pipeline, not its input Format. */
internal class VideoDecoderEvidence {
    class Claim(val name: String, val codecMimeType: String)

    private var current: Claim? by mutableStateOf(null)
    val codecMimeType: String? get() = current?.codecMimeType

    @Synchronized
    fun initialized(name: String, codecMimeType: String): Claim =
        Claim(name, codecMimeType).also { current = it }

    /** A delayed release belonging to an older codec cannot clear its replacement. */
    @Synchronized
    fun released(claim: Claim?) {
        if (current === claim) current = null
    }
}
