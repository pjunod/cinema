package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PlaybackBufferBudgetTest {
    @Test
    fun actualGrantedHeapClampsEveryRoleWithoutRaisingTheExistingCeiling() {
        for (memoryClass in listOf(16, 128, 256, 512, Int.MAX_VALUE)) {
            for (grantMb in listOf(16, 64, 128, 256, 512, 1024)) {
                val grant = grantMb.toLong() * PLAYBACK_MIB
                for (role in BufferRole.entries) {
                    val target = playbackBufferTargetBytes(memoryClass, grant, role)
                    assertTrue(target > 0)
                    assertTrue(target <= playbackBufferTargetBytes(memoryClass))
                    assertTrue(2L * target <= grant / 4)
                }
            }
        }
        assertEquals(16 * PLAYBACK_MIB,
            playbackBufferTargetBytes(512, 128L * PLAYBACK_MIB, BufferRole.Incumbent))
        assertEquals(32 * PLAYBACK_MIB,
            playbackBufferTargetBytes(256, 512L * PLAYBACK_MIB, BufferRole.Successor))
    }

    @Test(expected = IllegalArgumentException::class)
    fun anUnknownGrantCannotMasqueradeAsARealHeap() {
        playbackBufferTargetBytes(256, 0, BufferRole.Incumbent)
    }

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
