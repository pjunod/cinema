package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.ui.FormFactor

class PlayerSurfaceAndClampTest {
    @Test
    fun televisionFollowsTheTenFootTableAndEverythingElseFollowsTouch() {
        assertEquals(PlayerInputSurface.TenFoot, playerInputSurfaceFor(FormFactor.Television))
        assertEquals(PlayerInputSurface.Touch, playerInputSurfaceFor(FormFactor.Compact))
        assertEquals(PlayerInputSurface.Touch, playerInputSurfaceFor(FormFactor.Expanded))
    }

    @Test
    fun anUnknownDurationIsNoCeilingRatherThanAZeroOne() {
        // A download whose record carries no duration, before the player has
        // prepared: clamping to 0 sent every skip to the start of the film.
        assertEquals(25_000L, clampToKnownDuration(25_000L, 0L))
        assertEquals(0L, clampToKnownDuration(-4_000L, 0L))
        assertEquals(60_000L, clampToKnownDuration(75_000L, 60_000L))
        assertEquals(25_000L, clampToKnownDuration(25_000L, 60_000L))
    }
}
