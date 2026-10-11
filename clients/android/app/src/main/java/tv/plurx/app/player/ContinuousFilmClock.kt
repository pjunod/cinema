package tv.plurx.app.player

/** Convert actual item-local output to the declared film grid. Rounding to a
 * frame, rather than flooring microseconds, preserves a 24 fps boundary. */
internal object ContinuousFilmClock {
    fun frontier(ms: Long, timescale: Long, segmentTicks: Long): Long {
        require(ms >= 0 && timescale in 1..1_000_000 && segmentTicks > 0)
        val tick = Math.multiplyExact(ms, timescale) / 1000
        return (tick / segmentTicks * segmentTicks).also { require(it <= ContinuousQualityWire.MAX_SAFE_INTEGER) }
    }
    fun frameTick(us: Long, timescale: Long, frameTicks: Long): Long? {
        if (us < 0 || timescale !in 1..1_000_000 || frameTicks <= 0) return null
        return try {
            val numerator = Math.multiplyExact(us, timescale)
            val denominator = Math.multiplyExact(frameTicks, 1_000_000)
            val frames = numerator / denominator + if (numerator % denominator >= denominator / 2 + denominator % 2) 1 else 0
            Math.multiplyExact(frames, frameTicks).takeIf { it <= ContinuousQualityWire.MAX_SAFE_INTEGER }
        } catch (_: ArithmeticException) { null }
    }
}
