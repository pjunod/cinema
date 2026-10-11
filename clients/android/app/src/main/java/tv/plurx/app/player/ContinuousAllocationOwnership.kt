package tv.plurx.app.player

import java.io.IOException
import java.util.IdentityHashMap

/** Allocation release is independent of sample metadata/front indices. A
 * write conservatively associates all this attachment's live allocations;
 * sharing an allocation can delay disposal, never prove it prematurely. */
internal class ContinuousAllocationOwnership {
    private data class Held(val owner: Any, val artifacts: MutableSet<String>)
    private val held = IdentityHashMap<Any, Held>()
    @Synchronized fun created(allocation: Any, owner: Any, artifact: String) {
        if (allocation in held || held.size >= 4096) throw IOException("Continuous allocation ownership bound")
        held[allocation] = Held(owner, linkedSetOf(artifact))
    }
    @Synchronized fun writing(owner: Any, artifact: String) {
        val current = held.values.filter { it.owner === owner }
        if (current.any { artifact !in it.artifacts && it.artifacts.size >= 128 }) throw IOException("Continuous allocation artifact bound")
        current.forEach { it.artifacts.add(artifact) }
    }
    @Synchronized fun contains(allocation: Any): Boolean = allocation in held
    @Synchronized fun released(allocation: Any) { held.remove(allocation) }
    @Synchronized fun artifactReleased(owner: Any, artifact: String): Boolean = held.values.none { it.owner === owner && artifact in it.artifacts }
    @Synchronized fun ownerReleased(owner: Any): Boolean = held.values.none { it.owner === owner }
}
