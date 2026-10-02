package tv.plurx.app.player

/** Scheduling/revalidation has an active-time budget independent of a future
 * presentation boundary. A committed target cannot use retained-current. */
internal class ContinuousControlDeadline(private val budgetMs: Long = 30_000) {
    private var revision: Long? = null
    private var previousMs: Long? = null
    private var previousActive = false
    private var elapsedMs = 0L
    private var fired = false
    fun sample(intent: Long, eligible: Boolean, nowMs: Long, active: Boolean): Boolean {
        if (revision != intent) {
            revision = intent; previousMs = null; elapsedMs = 0; fired = false
        }
        val previous = previousMs
        if (eligible && previousActive && previous != null && nowMs > previous)
            elapsedMs = minOf(budgetMs, elapsedMs + minOf(budgetMs, nowMs - previous))
        previousMs = nowMs
        previousActive = eligible && active
        if (!eligible || fired || elapsedMs < budgetMs) return false
        fired = true
        return true
    }
}
