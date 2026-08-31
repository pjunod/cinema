package tv.plurx.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class MarkerLabelTest {
    @Test
    fun chapterDerivedMarkerShowsExactLabel() {
        val marker = Marker(
            kind = "credits",
            label = "Skip Credits",
            start_ms = 600_000,
            end_ms = 660_000,
            chapter = true,
        )

        assertEquals("Skip Credits", marker.displayLabel)
    }

    @Test
    fun estimatedMarkerShowsHedgedLabel() {
        val marker = Marker(
            kind = "credits",
            label = "Skip Credits",
            start_ms = 600_000,
            end_ms = 660_000,
            chapter = false,
            provenance = "estimated",
            confidence = 250,
        )

        assertEquals("Skip Credits (estimated)", marker.displayLabel)
        assertFalse(marker.isAutoSkipEligible)
    }

    @Test
    fun markerDefaultsToChapterTrue() {
        val marker = Marker(
            kind = "credits",
            label = "Skip Credits",
            start_ms = 600_000,
            end_ms = 660_000,
        )

        assertEquals(true, marker.chapter)
        assertEquals("Skip Credits", marker.displayLabel)
        assertTrue(marker.isAutoSkipEligible)
    }

    @Test
    fun provenanceControlsExactnessAndAutomaticEligibility() {
        val manual = Net.json.decodeFromString(
            Marker.serializer(),
            """{"kind":"credits","label":"Skip Credits","start_ms":600000,"end_ms":660000,"chapter":false,"provenance":"manual","confidence":1000,"generation":"g1","detector_version":"manual-v1"}""",
        )
        val detected = manual.copy(provenance = "detected")

        assertEquals("Skip Credits", manual.displayLabel)
        assertEquals(1_000, manual.confidence)
        assertEquals("g1", manual.generation)
        assertEquals("manual-v1", manual.detector_version)
        assertTrue(manual.isAutoSkipEligible)
        assertFalse(
            "M1-M4 has no configured detector confidence floor",
            detected.isAutoSkipEligible,
        )
    }
}
