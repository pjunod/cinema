package tv.plurx.app.player

/** Parses the server's exact ffprobe rational without rounding its cadence. */
internal fun parseFrameRateRational(raw: String?): Double? {
    val (numerator, denominator) = raw?.trim()?.split('/', limit = 2)
        ?.takeIf { it.size == 2 } ?: return null
    val top = numerator.toDoubleOrNull() ?: return null
    val bottom = denominator.toDoubleOrNull() ?: return null
    return (top / bottom).takeIf { top > 0.0 && bottom > 0.0 && it.isFinite() }
}
