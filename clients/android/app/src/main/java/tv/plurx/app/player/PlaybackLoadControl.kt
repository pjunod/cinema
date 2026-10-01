@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.app.ActivityManager
import android.content.Context
import android.util.Log
import androidx.media3.exoplayer.DefaultLoadControl
import coil.imageLoader

/**
 * Media3's default streaming A/V target is about 138 MiB per player. On a
 * 256 MiB heap that competes with extractors, artwork and a prepared successor.
 * Budget each pipeline independently so two players leave room for those users.
 * This is an allocator target, not a hard cap on all decoder/extractor memory.
 */
internal fun playbackLoadControl(
    context: Context,
    live: Boolean = false,
    role: BufferRole = if (live) BufferRole.Live else BufferRole.Incumbent,
): DefaultLoadControl {
    val manager = context.getSystemService(Context.ACTIVITY_SERVICE) as? ActivityManager
    val memoryClass = manager?.memoryClass ?: 128
    val grantedHeap = Runtime.getRuntime().maxMemory()
    val target = playbackBufferTargetBytes(memoryClass, grantedHeap, role)
    // This is the actual configured Coil bound, not a guessed cache allowance.
    // The line is local and content/account-free; it does not qualify M0's PSS.
    val imageCache = context.imageLoader.memoryCache?.maxSize ?: 0
    Log.i("PlurxBufferBudget", "role=$role memory_class_mb=$memoryClass " +
        "large_memory_class_mb=${manager?.largeMemoryClass} granted_heap_bytes=$grantedHeap " +
        "image_cache_max_bytes=$imageCache target_bytes=$target policy=existing_ceiling_actual_heap_clamp")
    return DefaultLoadControl.Builder()
        .setTargetBufferBytes(target)
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
