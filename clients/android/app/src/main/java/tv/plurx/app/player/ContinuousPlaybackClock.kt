package tv.plurx.app.player

/** Record every transport/lifecycle sample before actor wakeups coalesce.
 * Network work must never turn an unobserved pause/resume pair into active time. */
internal class ContinuousPlaybackClock {
    private var previous: ContinuousObservationDeadline.Clock? = null
    private var activeMs = 0L
    @Synchronized fun sample(clock: ContinuousObservationDeadline.Clock): ContinuousObservationDeadline.Clock {
        val current = clock.copy(active = clock.active && clock.rate.isFinite() && clock.rate > 0)
        previous?.let { last ->
            if (last.active && current.nowMs > last.nowMs) {
                val elapsed = current.nowMs - last.nowMs
                activeMs = if (elapsed > Long.MAX_VALUE - activeMs) Long.MAX_VALUE else activeMs + elapsed
            }
        }
        previous = current
        return current.copy(nowMs = activeMs, activeElapsed = true)
    }
}
