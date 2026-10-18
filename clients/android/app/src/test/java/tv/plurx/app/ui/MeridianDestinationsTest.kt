package tv.plurx.app.ui

import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.Features

class MeridianDestinationsTest {
    @Test fun featureProjectionPreservesRoutePolicyAndTvDownloadsVisibility() {
        val enabled = meridianDestinations(Features(), false)
        assertEquals(enabled.size, enabled.map { it.id }.distinct().size)
        assertTrue(enabled.any { it.id == "downloads" })
        assertFalse(meridianDestinations(Features(), true).any { it.id == "downloads" })
        val restricted = meridianDestinations(Features(live_tv = false, recordings = false, library_channels = false, shared_libraries = false), false)
        assertEquals(listOf("home", "libraries", "search", "downloads", "settings"), restricted.map { it.id })
    }
}
