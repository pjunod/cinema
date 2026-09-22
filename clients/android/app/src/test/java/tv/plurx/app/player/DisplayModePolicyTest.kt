package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class DisplayModePolicyTest {
    private val current = DisplayModeCandidate(1, 3840, 2160, 60f)
    private fun mode(id: Int, hz: Float, width: Int = 3840, height: Int = 2160) =
        DisplayModeCandidate(id, width, height, hz)

    @Test fun `fractional 24 family wins when exposed`() {
        assertEquals(4, chooseDisplayMode(listOf(current, mode(2, 59.94f), mode(3, 24f), mode(4, 23.976f)), current, 24_000.0 / 1_001.0))
    }

    @Test fun `nominal 24 is the bounded fallback`() {
        assertEquals(3, chooseDisplayMode(listOf(current, mode(2, 50f), mode(3, 24f)), current, 24_000.0 / 1_001.0))
    }

    @Test fun `unrelated refresh rates do not match 24 family`() {
        assertNull(chooseDisplayMode(listOf(current, mode(2, 50f)), current, 24_000.0 / 1_001.0))
    }

    @Test fun `smallest exact integer multiple wins`() {
        assertEquals(2, chooseDisplayMode(listOf(current, mode(2, 50f)), current, 25.0))
    }

    @Test fun `fractional 30 family beats nominal 60`() {
        assertEquals(2, chooseDisplayMode(listOf(current, mode(2, 59.94f)), current, 30_000.0 / 1_001.0))
    }

    @Test fun `resolution never changes`() {
        assertNull(chooseDisplayMode(listOf(current, mode(2, 23.976f, 1920, 1080)), current, 24_000.0 / 1_001.0))
    }

    @Test fun `already matching current mode returns no request`() {
        val matching = mode(7, 23.976f)
        assertNull(chooseDisplayMode(listOf(matching, current), matching, 24_000.0 / 1_001.0))
    }

}
