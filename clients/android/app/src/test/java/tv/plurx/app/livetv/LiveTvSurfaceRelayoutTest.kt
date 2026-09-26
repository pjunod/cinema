package tv.plurx.app.livetv

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class LiveTvSurfaceRelayoutTest {
    @Test
    fun theSurfaceRelaysOutWhenItsHostBoxChangesSize() {
        // Inline box (a 260 dp strip) to the fullscreen box on a tablet.
        assertTrue(LiveTvSurfaceRelayout.hostResized(1600, 650, 2000, 1200))
        // And back again.
        assertTrue(LiveTvSurfaceRelayout.hostResized(2000, 1200, 1600, 650))
        // The first placement is a resize from nothing.
        assertTrue(LiveTvSurfaceRelayout.hostResized(0, 0, 1600, 650))
    }

    @Test
    fun anUnchangedOrEmptyHostBoxDoesNotForceALayoutPass() {
        assertFalse(LiveTvSurfaceRelayout.hostResized(2000, 1200, 2000, 1200))
        assertFalse(LiveTvSurfaceRelayout.hostResized(1600, 650, 0, 0))
        assertFalse(LiveTvSurfaceRelayout.hostResized(1600, 650, 2000, 0))
    }
}
