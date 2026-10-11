package tv.plurx.app.player

import java.io.IOException
import java.nio.ByteBuffer
import org.junit.Assert.*
import org.junit.Test

class ContinuousFragmentClockTest {
    private fun box(type: String, bytes: ByteArray): ByteArray = ByteBuffer.allocate(8 + bytes.size)
        .putInt(8 + bytes.size).put(type.toByteArray(Charsets.US_ASCII)).put(bytes).array()
    private fun clock(tick: Long, version: Int = 1): ByteArray {
        val payload = ByteBuffer.allocate(if (version == 0) 8 else 12).put(version.toByte()).put(byteArrayOf(0, 0, 0))
        if (version == 0) payload.putInt(tick.toInt()) else payload.putLong(tick)
        return box("tfdt", payload.array())
    }
    private fun fragment(clock: ByteArray): ByteArray = box("moof", box("traf", clock)) + box("mdat", byteArrayOf(1))

    @Test fun actualAacDecodeClockChoosesItsContainingVideoEntry() {
        for (version in listOf(0, 1)) {
            assertEquals(192_512L, ContinuousFragmentClock.firstDecodeTick(fragment(clock(192_512, version))))
        }
        assertEquals(96L, ContinuousFragmentClock.videoEntry(192_512, 48_000, 24, 48))
        assertEquals(48L, ContinuousFragmentClock.videoEntry(191_488, 48_000, 24, 48))
        assertEquals(0L, ContinuousFragmentClock.videoEntry(95_744, 48_000, 24, 48))
    }

    @Test fun duplicateTruncatedAndUnsafeClockBoxesAreRefused() {
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(fragment(clock(1) + clock(2))) }
        val twoTracks = box("moof", box("traf", clock(1)) + box("traf", clock(1)))
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(twoTracks) }
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(fragment(clock(-1))) }
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(fragment(clock(ContinuousQualityWire.MAX_SAFE_INTEGER + 1))) }
        val valid = fragment(clock(1))
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(valid.copyOf(valid.size - 1)) }
        val zeroLength = ByteBuffer.allocate(8).putInt(4).put("moof".toByteArray()).array()
        assertThrows(IOException::class.java) { ContinuousFragmentClock.firstDecodeTick(zeroLength) }
    }

    @Test fun clockScalingAvoidsOverflowAndRefusesAnUnrepresentableFrontier() {
        val largest = ContinuousQualityWire.MAX_SAFE_INTEGER
        val entry = ContinuousFragmentClock.videoEntry(largest, 1_000_000, 1_000_000, 48)
        assertEquals(largest - largest % 48, entry)
        assertThrows(IOException::class.java) { ContinuousFragmentClock.videoEntry(largest, 1, 1_000_000, 48) }
        assertThrows(IOException::class.java) { ContinuousFragmentClock.videoEntry(1, 0, 24, 48) }
    }
}
