@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.app.ActivityManager
import android.content.Context
import androidx.media3.exoplayer.DefaultLoadControl

/**
 * Media3's default streaming A/V target is about 138 MiB per player. On a
 * 256 MiB heap that competes with extractors, artwork and a prepared successor.
 * Budget each pipeline independently so two players leave room for those users.
 * This is an allocator target, not a hard cap on all decoder/extractor memory.
 */
internal fun playbackBufferTargetBytes(memoryClassMb: Int): Int =
    (memoryClassMb.coerceAtLeast(16) / 8).coerceAtMost(64) * 1024 * 1024

internal fun playbackLoadControl(
    context: Context,
    live: Boolean = false,
): DefaultLoadControl {
    val memoryClass = (context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager)
        ?.memoryClass ?: 128
    return DefaultLoadControl.Builder()
        .setTargetBufferBytes(playbackBufferTargetBytes(memoryClass))
        // Stop loading at the byte target even when a high-bitrate stream has
        // not accumulated the usual time target. Local playback defaults to
        // prioritizing time, so both modes must explicitly obey this budget.
        .setPrioritizeTimeOverSizeThresholdsForStreaming(false)
        .setPrioritizeTimeOverSizeThresholdsForLocalPlayback(false)
        .apply {
            if (live) setBufferDurationsMsForStreaming(4_000, 12_000, 1_000, 2_000)
        }
        .build()
}
