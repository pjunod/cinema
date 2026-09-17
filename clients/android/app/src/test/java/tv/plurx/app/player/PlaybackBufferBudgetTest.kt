package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PlaybackBufferBudgetTest {
    @Test
    fun lenovoHeapLeavesRoomForExtractorsAndPreparedHandoff() {
        val heap = 256 * 1024 * 1024
        val perPlayer = playbackBufferTargetBytes(256)
        assertEquals(32 * 1024 * 1024, perPlayer)
        assertTrue("Two media buffers leave at least 75% of the heap", 2 * perPlayer <= heap / 4)
    }

    @Test
    fun budgetsStayPositiveAndBoundedAcrossDeviceHeapClasses() {
        for (heapMb in listOf(16, 32, 64, 128, 192, 256, 384, 512, 1024, Int.MAX_VALUE)) {
            val bytes = playbackBufferTargetBytes(heapMb)
            assertTrue(bytes > 0)
            assertTrue(bytes <= 64 * 1024 * 1024)
            assertTrue(2L * bytes <= heapMb.toLong() * 1024 * 1024 / 4)
        }
    }
}
