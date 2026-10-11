package tv.plurx.app.player

import java.io.IOException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class ContinuousVerifiedRangeTest {
    @Test fun retrySlicesStayWithinTheFullyVerifiedArtifact() {
        assertEquals(0 to 100, ContinuousVerifiedRange.resolve(100, 0, -1))
        assertEquals(40 to 100, ContinuousVerifiedRange.resolve(100, 40, -1))
        assertEquals(40 to 60, ContinuousVerifiedRange.resolve(100, 40, 20))
        assertEquals(100 to 100, ContinuousVerifiedRange.resolve(100, 100, -1))
        assertEquals(40 to 40, ContinuousVerifiedRange.resolve(100, 40, 0))
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, 101, -1) }
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, 40, 61) }
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, -1, 1) }
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, 40, -2) }
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, Long.MAX_VALUE, 1) }
        assertThrows(IOException::class.java) { ContinuousVerifiedRange.resolve(100, 40, Long.MAX_VALUE) }
    }
}
