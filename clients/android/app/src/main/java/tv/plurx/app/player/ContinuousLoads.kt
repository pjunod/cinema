package tv.plurx.app.player

import java.io.IOException
import java.util.IdentityHashMap

/** Closing admission cancels owned I/O, but a loader remains active until its
 * own close finishes. Cancellation is never evidence of extractor retirement. */
internal class ContinuousLoads {
    private val active = IdentityHashMap<ContinuousReservedDataSource, Unit>()
    @Volatile private var accepting = true
    fun isAlive(): Boolean = accepting
    @Synchronized fun opened(source: ContinuousReservedDataSource) {
        if (!accepting) throw IOException("Continuous attachment closed")
        if (active.containsKey(source)) throw IOException("Continuous loader already active")
        if (active.size >= 32) throw IOException("Continuous active loader bound")
        active[source] = Unit
    }
    @Synchronized fun closed(source: ContinuousReservedDataSource) { active.remove(source) }
    @Synchronized fun isQuiescent(): Boolean = active.isEmpty()
    fun cancel() {
        val pending = synchronized(this) { accepting = false; active.keys.toList() }
        pending.forEach { it.cancelPending() }
    }
}
