package tv.plurx.app.data

/** Short-lived client floor. Never substitute a node's own write fence. */
internal class ReadAfterReceipt(private val nowNanos: () -> Long = System::nanoTime) {
    data class Request(val generation: Long, val epoch: Long, val index: String?)

    private var generation: Long? = null
    private var epoch = 0L
    private var index: ULong? = null
    private var observedAt = 0L

    /** Captured credentials are checked under the receipt lock so a delayed
     * old request cannot reset a newer login's floor. */
    @Synchronized
    fun request(captured: Long, currentGeneration: () -> Long): Request? {
        if (captured != currentGeneration()) return null
        if (generation != captured) {
            generation = captured
            forget()
        }
        // Subtraction works across nanoTime's signed wrap; no wall clock.
        if (index != null && nowNanos() - observedAt >= 60_000_000_000L) index = null
        return Request(captured, epoch, index?.toString())
    }

    @Synchronized
    fun observe(value: String?, mutating: Boolean, request: Request, currentGeneration: () -> Long) {
        if (request.generation != currentGeneration() || generation != request.generation) return
        if (value == null) {
            if (mutating) forget()
            return
        }
        val parsed = value.toULongOrNull()
        if (parsed == null || parsed == 0uL || parsed.toString() != value) {
            forget()
            return
        }
        if (request.epoch != epoch) return
        if (index == null || parsed >= index!!) {
            index = parsed
            observedAt = nowNanos()
        }
    }

    @Synchronized
    fun transportFailed(mutating: Boolean, request: Request, currentGeneration: () -> Long) {
        if (!mutating || request.generation != currentGeneration() || generation != request.generation) return
        forget()
    }

    private fun forget() {
        epoch++
        index = null
    }
}
