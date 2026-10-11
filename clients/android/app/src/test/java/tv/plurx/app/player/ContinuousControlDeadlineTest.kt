package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousControlDeadlineTest {
    @Test fun pauseAndCommittedTargetsCannotConsumeOrReuseOptionalControlBudgets() {
        val deadline = ContinuousControlDeadline()
        assertFalse(deadline.sample(1, true, 0, true))
        assertFalse(deadline.sample(1, true, 20_000, false))
        assertFalse(deadline.sample(1, true, 120_000, true))
        assertTrue(deadline.sample(1, true, 130_000, true))
        assertFalse(deadline.sample(1, true, 140_000, true))
        assertFalse(deadline.sample(2, true, 140_000, true))
        assertFalse(deadline.sample(2, false, 170_000, true))
        assertFalse(deadline.sample(2, false, 200_000, true))
        assertFalse(deadline.sample(3, true, 200_000, true))
        assertTrue(deadline.sample(3, true, 230_000, true))
    }
}
