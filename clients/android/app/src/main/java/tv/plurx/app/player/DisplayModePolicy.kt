package tv.plurx.app.player

import kotlin.math.abs
import kotlin.math.round

internal data class DisplayModeCandidate(
    val id: Int,
    val width: Int,
    val height: Int,
    val refreshHz: Float,
)

private data class ModeMatch(val mode: DisplayModeCandidate, val multiple: Int, val error: Double)

/**
 * Chooses a same-resolution refresh rate at the source cadence or an integer
 * multiple. Fractional-family matches win; a nominal integer (23.976 -> 24)
 * is considered only when the display exposes no exact fractional match.
 */
internal fun chooseDisplayMode(
    supported: List<DisplayModeCandidate>,
    current: DisplayModeCandidate,
    sourceFps: Double,
): Int? {
    if (!sourceFps.isFinite() || sourceFps <= 0.0) return null
    val sameResolution = supported.filter {
        it.width == current.width && it.height == current.height &&
            it.refreshHz.isFinite() && it.refreshHz > 0f
    }

    fun matches(targetFps: Double): List<ModeMatch> = sameResolution.mapNotNull { candidate ->
        val multiple = round(candidate.refreshHz / targetFps).toInt()
        if (multiple < 1) return@mapNotNull null
        val error = abs(candidate.refreshHz.toDouble() - multiple * targetFps)
        ModeMatch(candidate, multiple, error).takeIf { error <= 0.01 }
    }

    val exact = matches(sourceFps)
    val nominalFps = round(sourceFps)
    val candidates = exact.ifEmpty {
        if (abs(nominalFps - sourceFps) <= 0.05) matches(nominalFps) else emptyList()
    }
    if (candidates.any { it.mode.id == current.id }) return null
    return candidates.minWithOrNull(
        compareBy<ModeMatch> { it.multiple }.thenBy { it.error }.thenBy { it.mode.id },
    )?.mode?.id
}
