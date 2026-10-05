package tv.plurx.app.player

import java.util.IdentityHashMap

/** A release notification alone is insufficient. Keep the actual resource
 * until its public state confirms release, including asynchronous callbacks. */
internal class ContinuousAudioOutputs {
    private data class Output(val owner: Any, val allocation: Long, val released: () -> Boolean)
    private val outputs = IdentityHashMap<Any, Output>()
    private var allocation = 0L
    @Synchronized fun requireCapacity() { check(outputs.size < 16) { "Continuous audio output ownership bound" } }
    @Synchronized fun allocated(resource: Any, owner: Any, released: () -> Boolean) {
        requireCapacity()
        check(!outputs.containsKey(resource))
        allocation = Math.addExact(allocation, 1)
        outputs[resource] = Output(owner, allocation, released)
    }
    @Synchronized fun collectReleased(): Set<Any> {
        // Copy key and value out before removing anything: IdentityHashMap's
        // entry views are slot positions, and every remove() shifts later
        // slots, so a removal through a retained entry view misses (or hits
        // the wrong) resource and released outputs were never retired.
        val retired = outputs.entries
            .filter { runCatching { it.value.released() }.getOrDefault(false) }
            .map { it.key to it.value.owner }
        retired.forEach { (resource, _) -> outputs.remove(resource) }
        val owners = retired.map { it.second }
        return owners.filter { owner -> outputs.values.none { it.owner === owner } }.toSet()
    }
    @Synchronized fun allocationMark(): Long = allocation
    @Synchronized fun releasedThrough(owner: Any, mark: Long): Boolean =
        outputs.values.none { it.owner === owner && it.allocation <= mark }
    @Synchronized fun isReleased(owner: Any): Boolean = outputs.values.none { it.owner === owner }
}
