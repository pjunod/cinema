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
        val retired = outputs.entries.filter { runCatching { it.value.released() }.getOrDefault(false) }
        val owners = retired.map { it.value.owner }.toSet()
        retired.forEach { outputs.remove(it.key) }
        return owners.filter { owner -> outputs.values.none { it.owner === owner } }.toSet()
    }
    @Synchronized fun allocationMark(): Long = allocation
    @Synchronized fun releasedThrough(owner: Any, mark: Long): Boolean =
        outputs.values.none { it.owner === owner && it.allocation <= mark }
    @Synchronized fun isReleased(owner: Any): Boolean = outputs.values.none { it.owner === owner }
}
