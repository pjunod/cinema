package tv.plurx.app.player

import org.junit.Assert.assertFalse
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
        assertTrue(detail.endsWith(" withdrawn=false"))
    }

    @Test fun `telemetry detail records a withdrawn request`() {
        val detail = DisplayModeMatchResult(
            outcome = "timeout",
            sourceFps = 25.0,
            fromHz = 60f,
            toHz = 50f,
            waitMs = 2_000,
            withdrawn = true,
        ).detail()
        assertTrue(detail.startsWith("outcome=timeout result=timeout "))
        assertTrue(detail.endsWith(" withdrawn=true"))
    }

    @Test fun timedOutPrepareWaitWithdrawsTheRequestOnlyWhileItStillOwnsTheWindow() {
        assertTrue(
            "a timeout under the current owner must clear preferredDisplayModeId",
            clearsDisplayModeRequestAfterWait(matched = false, stillOwner = true, late = false),
        )
        assertFalse(
            "a new owner may have set its own request",
            clearsDisplayModeRequestAfterWait(matched = false, stillOwner = false, late = false),
        )
        assertFalse(clearsDisplayModeRequestAfterWait(matched = true, stillOwner = true, late = false))
        assertFalse(clearsDisplayModeRequestAfterWait(matched = true, stillOwner = false, late = false))
    }

    @Test fun lateWaitNeverWithdrawsTheRequest() {
        // §5.6: the onTracksChanged fallback switches mid-play by design, so a
        // late wait that outlives the 2 s bound keeps its request in every case.
        for (matched in listOf(true, false)) {
            for (stillOwner in listOf(true, false)) {
                assertFalse(
                    "late wait matched=$matched stillOwner=$stillOwner must not withdraw",
                    clearsDisplayModeRequestAfterWait(matched = matched, stillOwner = stillOwner, late = true),
                )
            }
        }
    }
}
