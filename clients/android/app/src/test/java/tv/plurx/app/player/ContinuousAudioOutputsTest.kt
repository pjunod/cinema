package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousAudioOutputsTest {
    @Test fun releaseCallbacksCannotRetireAnInitializedOrUnknownOutput() {
        val outputs = ContinuousAudioOutputs()
        val owner = Any()
        val first = Any(); val second = Any()
        var firstReleased = false
        var secondReleased = false
        outputs.allocated(first, owner) { firstReleased }
        outputs.allocated(second, owner) { if (!secondReleased) throw IllegalStateException("unknown output state") else true }
        assertFalse(outputs.isReleased(owner))
        assertTrue(outputs.collectReleased().isEmpty())
        firstReleased = true
        assertTrue(outputs.collectReleased().isEmpty())
        assertFalse(outputs.isReleased(owner))
        secondReleased = true
        assertEquals(setOf(owner), outputs.collectReleased())
        assertTrue(outputs.isReleased(owner))
        assertTrue(outputs.collectReleased().isEmpty())
    }
    @Test fun pendingResourcesRemainBoundedUntilTheirStateConfirmsRelease() {
        val outputs = ContinuousAudioOutputs()
        val owner = Any()
        var released = false
        repeat(16) { outputs.allocated(Any(), owner) { released } }
        assertThrows(IllegalStateException::class.java) { outputs.requireCapacity() }
        assertTrue(outputs.collectReleased().isEmpty())
        released = true
        assertEquals(setOf(owner), outputs.collectReleased())
        outputs.requireCapacity()
        val other = Any()
        outputs.allocated(Any(), other) { false }
        assertTrue(outputs.isReleased(owner))
        assertFalse(outputs.isReleased(other))
    }
}
