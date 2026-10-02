package tv.plurx.app.player

import java.io.IOException

/** Publication and cancellation share a monitor: a target can be retained
 * only before any loader has exposed its bytes, not merely before an ACK. */
internal class ContinuousExposure {
    private val exposed = LinkedHashSet<String>()
    private val cancelled = LinkedHashSet<String>()
    @Synchronized fun publish(owners: Set<String>) {
        if (owners.any { it in cancelled }) throw ContinuousStaleVideoLoad()
        if ((exposed + owners).size > 16) throw IOException("Continuous exposure owner bound")
        exposed.addAll(owners)
    }
    @Synchronized fun cancelUnexposed(owner: String, physicallyAbsent: () -> Boolean): Boolean {
        if (owner in exposed || !physicallyAbsent()) return false
        if ((cancelled + owner).size > 16) throw IOException("Continuous cancelled owner bound")
        cancelled.add(owner)
        return true
    }
    @Synchronized fun isCancelled(owner: String): Boolean = owner in cancelled
    @Synchronized fun retain(live: Set<String>) { exposed.retainAll(live); cancelled.retainAll(live) }
}
