@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.exoplayer.upstream.Allocation
import androidx.media3.exoplayer.upstream.Allocator
import java.io.IOException
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class ContinuousAllocatorTest {
    @Test fun failedReleaseCannotRetireOwnedBytesAndBatchReleaseNeedsItsWholeAcknowledgment() {
        val owner = Any()
        val ownership = ContinuousAllocationOwnership()
        val first = Allocation(ByteArray(32), 0)
        val second = Allocation(ByteArray(32), 0)
        val available = ArrayDeque(listOf(first, second))
        var fail = false
        var released = 0
        val delegate = object : Allocator {
            override fun allocate(): Allocation = available.removeFirst()
            override fun release(allocation: Allocation) {
                if (fail) throw IOException("allocator release failed")
                released++
            }
            override fun release(allocationNode: Allocator.AllocationNode) {
                var node: Allocator.AllocationNode? = allocationNode
                while (node != null) { release(node.allocation); node = node.next() }
            }
            override fun trim() = Unit
            override fun getTotalBytesAllocated() = 64
            override fun getIndividualAllocationLength() = 32
        }
        val load = ContinuousLoadContext.Verified(owner,
            ContinuousQualityMedia.Resource("video", buildJsonObject { put("rendition_id", "b".repeat(64)) }, false, 0),
            ContinuousQualityMedia.Authorized(buildJsonObject { put("artifact_id", "c".repeat(64)) }, emptySet()))
        val allocator = ContinuousAllocator(delegate, ownership)
        try {
            ContinuousLoadContext.bind(load)
            assertSame(first, allocator.allocate())
            assertSame(second, allocator.allocate())
        } finally { ContinuousLoadContext.bind(null) }
        assertEquals(64, allocator.totalBytesAllocated)
        assertEquals(32, allocator.individualAllocationLength)
        fail = true
        assertTrue(runCatching { allocator.release(first) }.isFailure)
        assertFalse(ownership.ownerReleased(owner))
        fun node(allocation: Allocation, next: Allocator.AllocationNode? = null) = object : Allocator.AllocationNode {
            override fun getAllocation() = allocation
            override fun next() = next
        }
        val chain = node(first, node(second))
        assertTrue(runCatching { allocator.release(chain) }.isFailure)
        assertEquals(0, released)
        assertFalse(ownership.ownerReleased(owner))
        fail = false
        allocator.release(chain)
        assertEquals(2, released)
        assertTrue(ownership.ownerReleased(owner))
    }
}
