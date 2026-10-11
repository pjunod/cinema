package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousPlaybackClockTest {
    @Test fun coalescedPauseResumeSamplesCannotChargeBackgroundTime() {
        val clock = ContinuousPlaybackClock()
        fun sample(now: Long, film: Long, active: Boolean, rate: Double = 1.0) =
            clock.sample(ContinuousObservationDeadline.Clock(now, film, rate, active))
        val first = sample(100, 0, true)
        val paused = sample(5100, 5_000_000, false)
        sample(1_005_100, 5_000_000, true)
        val resumed = sample(1_006_100, 6_000_000, true)
        assertEquals(0L, first.nowMs)
        assertEquals(6000L, resumed.nowMs)
        assertEquals(6_000_000L, resumed.positionUs)
        val deadline = ContinuousControlDeadline()
        assertFalse(deadline.sample(1, true, first.nowMs, first.active))
        assertFalse(deadline.sample(1, true, resumed.nowMs, resumed.active))
        val shortBudget = ContinuousControlDeadline(6000)
        assertFalse(shortBudget.sample(1, true, first.nowMs, first.active, first.activeElapsed))
        assertFalse(shortBudget.sample(1, true, paused.nowMs, paused.active, paused.activeElapsed))
        assertTrue(shortBudget.sample(1, true, resumed.nowMs, resumed.active, resumed.activeElapsed))
        val observation = ContinuousObservationDeadline()
        assertFalse(observation.sample(1, 5_000_000, first))
        assertFalse(observation.sample(1, 5_000_000, paused))
        assertFalse(observation.sample(1, 5_000_000, resumed))
        assertTrue(observation.sample(1, 5_000_000, resumed.copy(nowMs = 7000, positionUs = 7_000_000)))
        val invalid = sample(1_007_100, 7_000_000, true, Double.NaN)
        val valid = sample(1_107_100, 7_000_000, true)
        assertEquals(7000L, invalid.nowMs)
        assertFalse(invalid.active)
        assertEquals(7000L, valid.nowMs)
    }
}
