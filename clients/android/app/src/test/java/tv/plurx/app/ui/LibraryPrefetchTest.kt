package tv.plurx.app.ui

import org.junit.Assert.assertEquals
import org.junit.Test

class LibraryPrefetchTest {
    @Test fun lastVisibleCardAndTwoCompleteRowsUseAnExclusiveBoundary() {
        assertEquals(7, libraryPrefetchExclusive(0, 3))
        assertEquals(208, libraryPrefetchExclusive(199, 4))
        assertEquals(200, libraryPrefetchExclusive(197, 1))
    }

    @Test fun columnChangesFollowTheMeasuredGridInsteadOfAFixedTwelve() {
        assertEquals(22, libraryPrefetchExclusive(19, 1))
        assertEquals(28, libraryPrefetchExclusive(19, 4))
        assertEquals(36, libraryPrefetchExclusive(19, 8))
    }

    @Test fun anUnmeasuredOrEmptyGridRequestsNoAdditionalItems() {
        assertEquals(0, libraryPrefetchExclusive(-1, 4))
        assertEquals(0, libraryPrefetchExclusive(19, 0))
    }

    @Test fun largeIndicesSaturateWithoutWrappingTheDemandNegative() {
        assertEquals(Int.MAX_VALUE, libraryPrefetchExclusive(Int.MAX_VALUE, 4))
        assertEquals(Int.MAX_VALUE, libraryPrefetchExclusive(0, Int.MAX_VALUE))
    }
}
