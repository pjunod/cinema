package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousAllocationOwnershipTest {
    @Test fun sharedAllocationsRemainOwnedUntilActualReleaseAndOwnersStaySeparate() {
        val ownership = ContinuousAllocationOwnership()
        val owner = Any(); val other = Any(); val first = Any(); val second = Any(); val foreign = Any()
        ownership.created(first, owner, "video:a")
        ownership.created(second, owner, "audio:b")
        ownership.created(foreign, other, "video:a")
        ownership.writing(owner, "video:c")
        ownership.released(first)
        assertTrue(ownership.artifactReleased(owner, "video:a"))
        assertFalse(ownership.artifactReleased(other, "video:a"))
        assertFalse(ownership.artifactReleased(owner, "video:c"))
        assertFalse(ownership.ownerReleased(owner))
        ownership.released(second)
        assertTrue(ownership.artifactReleased(owner, "video:c"))
        assertTrue(ownership.ownerReleased(owner))
        assertFalse(ownership.ownerReleased(other))
        ownership.released(foreign)
        ownership.created(first, owner, "video:new")
        assertTrue(ownership.artifactReleased(owner, "video:a"))
        assertFalse(ownership.artifactReleased(owner, "video:new"))
    }
}
