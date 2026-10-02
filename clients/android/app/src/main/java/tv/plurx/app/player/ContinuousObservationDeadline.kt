package tv.plurx.app.player

/** Only an observed clock crossing the committed boundary starts the grace
 * budget. Future buffer, pause and background cannot consume that budget. */
internal class ContinuousObservationDeadline(private val graceMs: Long = 2000) {
    data class Clock(val nowMs: Long, val positionUs: Long, val rate: Double, val active: Boolean, val activeElapsed: Boolean = false)
    private var revision: Long? = null
    private var boundary: Long? = null
    private var previous: Clock? = null
    private var lateMs = 0.0
    private var fired = false

    fun sample(intentRevision: Long, boundaryUs: Long?, clock: Clock): Boolean {
        val current = clock.copy(active = clock.active && clock.rate.isFinite() && clock.rate > 0)
        val newIntent = revision != intentRevision
        if (newIntent || boundary != boundaryUs) {
            revision = intentRevision
            boundary = boundaryUs
            previous = current
            lateMs = 0.0
            if (newIntent) fired = false
            return false
        }
        val last = previous
        previous = current
        if (boundaryUs == null || current.positionUs < boundaryUs) {
            lateMs = 0.0
            return false
        }
        if (last != null && (last.active || current.activeElapsed) && !fired) {
            val elapsed = (current.nowMs - last.nowMs).coerceAtLeast(0).toDouble()
            val crossing = when {
                last.positionUs >= boundaryUs -> 0.0
                last.rate.isFinite() && last.rate > 0 -> (boundaryUs - last.positionUs).toDouble() / last.rate / 1000
                else -> elapsed // No earlier crossing can be derived from an invalid rate.
            }
            lateMs = (lateMs + (elapsed - crossing).coerceAtLeast(0.0)).coerceAtMost(graceMs.toDouble())
        }
        if (current.active && !fired && lateMs >= graceMs) { fired = true; return true }
        return false
    }
}
