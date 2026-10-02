package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousObservationDeadlineTest {
    @Test fun aFarFutureAppendAndPauseDoNotBecomeAFalsePresentationTimeout() {
        val deadline = ContinuousObservationDeadline()
        fun sample(at: Long, position: Long, active: Boolean = true, rate: Double = 1.0) =
            deadline.sample(1, 40_000_000, ContinuousObservationDeadline.Clock(at, position, rate, active))
        assertFalse(sample(0, 0))
        assertFalse(sample(30_000, 30_000_000))
        assertFalse(sample(40_000, 40_000_000))
        assertFalse(sample(41_000, 41_000_000))
        assertFalse(sample(41_500, 41_500_000, false))
        assertFalse(sample(90_000, 41_500_000, false))
        assertFalse(sample(90_000, 41_500_000))
        assertFalse(sample(90_499, 41_999_000))
        assertTrue(sample(90_500, 42_000_000))
        assertFalse(sample(91_000, 42_500_000))
        assertFalse(deadline.sample(1, 50_000_000, ContinuousObservationDeadline.Clock(100_000, 51_000_000, 1.0, true)))
        assertFalse(deadline.sample(1, 50_000_000, ContinuousObservationDeadline.Clock(103_000, 54_000_000, 1.0, true)))
    }

    @Test fun rateChangesSeeksAndNewIntentRevisionsKeepTheirOwnBoundaryClock() {
        val deadline = ContinuousObservationDeadline()
        fun sample(revision: Long, at: Long, position: Long, rate: Double = 1.0) =
            deadline.sample(revision, 10_000_000, ContinuousObservationDeadline.Clock(at, position, rate, true))
        assertFalse(sample(1, 0, 0, 2.0))
        assertFalse(sample(1, 4000, 8_000_000, 0.5))
        assertFalse(sample(1, 8000, 10_000_000, 0.5))
        assertFalse(sample(1, 9000, 10_500_000, 0.5))
        assertFalse(sample(1, 9001, 0, 1.0))
        assertFalse(sample(1, 19_001, 10_000_000))
        assertTrue(sample(1, 21_001, 12_000_000))
        assertFalse(sample(2, 21_001, 12_000_000))
        assertFalse(sample(2, 22_001, 13_000_000))
        assertTrue(sample(2, 23_001, 14_000_000))
    }
}
