@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.upstream.Allocation
import androidx.media3.exoplayer.upstream.Allocator

/** Keep the SDK allocator and byte-budget accounting unchanged; acknowledge
 * only releases that actually returned successfully from that allocator. */
internal class ContinuousAllocator(private val delegate: Allocator, private val ownership: ContinuousAllocationOwnership) : Allocator by delegate {
    @Synchronized override fun allocate(): Allocation {
        val allocation = delegate.allocate()
        val load = ContinuousLoadContext.current()
        if (load != null) try {
            ownership.created(allocation, load.owner, artifactKey(load))
        } catch (error: Exception) { delegate.release(allocation); throw error }
        return allocation
    }
    @Synchronized override fun release(allocation: Allocation) {
        delegate.release(allocation)
        ownership.released(allocation)
    }
    @Synchronized override fun release(allocationNode: Allocator.AllocationNode) {
        val retained = ArrayList<Allocation>()
        var node: Allocator.AllocationNode? = allocationNode
        while (node != null) {
            if (ownership.contains(node.allocation)) retained.add(node.allocation)
            node = node.next()
        }
        delegate.release(allocationNode)
        retained.forEach(ownership::released)
    }
    companion object {
        fun artifactKey(load: ContinuousLoadContext.Verified) = "${load.resource.rendition}:${load.authorized.interval.text("artifact_id")}" 
    }
}
