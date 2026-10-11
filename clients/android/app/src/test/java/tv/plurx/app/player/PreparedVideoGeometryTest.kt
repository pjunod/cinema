package tv.plurx.app.player

import org.junit.Assert.*
import org.junit.Test

class PreparedVideoGeometryTest {
    @Test fun fourKFrameFillsPhoneWidthWithoutASecondDecoderScale() {
        val geometry = preparedVideoGeometry(1224, 688, 1224, 2160, 3840, 2160, 1f)!!
        assertEquals(1223.111f, 1224 * geometry.scaleX, 1f)
        assertEquals(688f, 2160 * geometry.scaleY, 0.01f)
        assertEquals(0f, geometry.top, 0.01f)
    }

    @Test fun rotationAndQualityChangeReuseTheOriginalSurfaceSpace() {
        for ((width, height) in listOf(3840 to 2160, 1920 to 1080, 1280 to 720)) {
            val g = preparedVideoGeometry(2992, 1224, 1224, 2160, width, height, 1f)!!
            assertEquals(2176f, 1224 * g.scaleX, 0.01f)
            assertEquals(1224f, 2160 * g.scaleY, 0.01f)
            assertEquals(408f, g.left, 0.01f)
            assertEquals(0f, g.top, 0.01f)
        }
    }

    @Test fun nonSquarePixelsPreserveDisplayAspect() {
        val g = preparedVideoGeometry(1920, 1080, 1280, 720, 720, 576, 16f / 15f)!!
        assertEquals(1440f, 1280 * g.scaleX, 0.01f)
        assertEquals(1080f, 720 * g.scaleY, 0.01f)
        assertEquals(240f, g.left, 0.01f)
    }

    @Test fun unknownOrInvalidGeometryIsNotPublished() {
        assertNull(preparedVideoGeometry(0, 1080, 1280, 720, 1920, 1080, 1f))
        assertNull(preparedVideoGeometry(1920, 1080, 1280, 0, 1920, 1080, 1f))
        for (ratio in listOf(0f, -1f, Float.NaN, Float.POSITIVE_INFINITY)) {
            assertNull(preparedVideoGeometry(1920, 1080, 1280, 720, 1920, 1080, ratio))
        }
    }
}
