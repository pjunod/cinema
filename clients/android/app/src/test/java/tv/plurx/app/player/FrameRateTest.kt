package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class FrameRateTest {
    @Test fun `rational parser preserves fractional cadence`() {
        assertEquals(24_000.0 / 1_001.0, parseFrameRateRational("24000/1001")!!, 0.000_001)
        assertNull(parseFrameRateRational("24000/0"))
        assertNull(parseFrameRateRational(null))
    }
}
