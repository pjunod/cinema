package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlin.test.assertEquals
import tv.plurx.app.data.PlaybackQuality

class AutoQualityLifecycleFenceTest {
    @Test fun briefOwnerPauseBetweenTicksRequiresANewForegroundMeasurement() {
        val state = AutoQualityState()
        fun eligible(now: Long) = state.eligible(true, PlaybackQuality.Auto, true, true, false, now, 5_000)
        assertFalse(eligible(0))
        assertTrue(eligible(5_000))
        state.blockedHeights.add(720.0)
        state.decodeStepConsumed = true
        state.suspendMeasurements(5_001)
        assertFalse(eligible(5_002))
        assertEquals(5_002L, state.measurementFloorMs)
        assertFalse(eligible(10_001))
        assertTrue(eligible(10_002))
        assertEquals(setOf(720.0), state.blockedHeights)
        assertTrue(state.decodeStepConsumed)
    }
}
