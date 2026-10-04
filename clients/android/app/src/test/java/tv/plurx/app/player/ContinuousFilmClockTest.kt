package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousFilmClockTest {
    @Test fun actualMicrosecondOutputRoundsToTheVerifiedFrameAndFrontierGrid() {
        assertEquals(1L, ContinuousFilmClock.frameTick(41_666, 24, 1))
        assertEquals(2L, ContinuousFilmClock.frameTick(83_333, 24, 1))
        assertEquals(3003L, ContinuousFilmClock.frameTick(33_367, 90_000, 3003))
        assertEquals(0L, ContinuousFilmClock.frameTick(0, 24, 1))
        assertEquals(48L, ContinuousFilmClock.frontier(3999, 24, 48))
        assertEquals(96L, ContinuousFilmClock.frontier(4000, 24, 48))
        assertNull(ContinuousFilmClock.frameTick(-1, 24, 1))
        assertNull(ContinuousFilmClock.frameTick(Long.MAX_VALUE, 90_000, 3003))
        assertNull(ContinuousFilmClock.frameTick(1, 24, Long.MAX_VALUE))
        assertNull(ContinuousFilmClock.frameTick(1, 0, 1))
    }
}
