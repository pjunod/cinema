package tv.plurx.app.player

/** A committed successor retains its priming load control until replacement. */
internal enum class BufferRole { Incumbent, Successor, Live }

internal const val PLAYBACK_MIB = 1024 * 1024

/** Existing measured-OOM containment ceiling, not a proposed larger allocation. */
internal fun playbackBufferTargetBytes(memoryClassMb: Int): Int =
    (memoryClassMb.coerceAtLeast(16) / 8).coerceAtMost(64) * PLAYBACK_MIB

/**
 * Runtime.maxMemory is the actual process grant; largeMemoryClass is not.
 * Until the three-device idle/playing/primed matrix exists, every role retains
 * its existing ceiling. A smaller actual grant can only reduce that ceiling.
 * No image-cache reserve or larger incumbent share is inferred from this clamp.
 */
internal fun playbackBufferTargetBytes(
    memoryClassMb: Int,
    grantedHeapBytes: Long,
    role: BufferRole,
): Int {
    require(grantedHeapBytes > 0) { "A granted process heap must be positive" }
    val existingCeiling = when (role) {
        BufferRole.Incumbent, BufferRole.Successor, BufferRole.Live ->
            playbackBufferTargetBytes(memoryClassMb)
    }
    return minOf(existingCeiling.toLong(), (grantedHeapBytes / 8).coerceAtLeast(1)).toInt()
}
