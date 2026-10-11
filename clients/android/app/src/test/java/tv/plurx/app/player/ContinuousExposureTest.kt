package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class ContinuousExposureTest {
    @Test fun publicationAndPhysicalAliasingPreventFalseRetention() {
        val exposure = ContinuousExposure()
        exposure.publish(setOf("visible"))
        assertFalse(exposure.cancelUnexposed("visible") { true })
        assertFalse(exposure.cancelUnexposed("alias") { false })
        exposure.publish(setOf("alias"))
        assertTrue(exposure.cancelUnexposed("cold") { true })
        assertTrue(runCatching { exposure.publish(setOf("cold", "new")) }.exceptionOrNull() is ContinuousStaleVideoLoad)
        assertTrue(exposure.cancelUnexposed("new") { true })
        exposure.publish(setOf("independent"))
        exposure.retain(setOf("cold", "independent"))
        assertTrue(runCatching { exposure.publish(setOf("cold")) }.exceptionOrNull() is ContinuousStaleVideoLoad)
    }
}
