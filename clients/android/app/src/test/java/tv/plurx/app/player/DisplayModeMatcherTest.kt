package tv.plurx.app.player

import org.junit.Assert.assertTrue
import org.junit.Test

class DisplayModeMatcherTest {
    @Test fun `telemetry detail is bounded and labels late fallback`() {
        val detail = DisplayModeMatchResult(
            outcome = "matched",
            sourceFps = 24_000.0 / 1_001.0,
            fromHz = 60f,
            toHz = 23.976f,
            waitMs = 41,
            late = true,
        ).detail()
        assertTrue(detail.startsWith("outcome=late result=matched "))
        assertTrue("source_fps=23.976" in detail)
        assertTrue("wait_ms=41" in detail)
    }
}
