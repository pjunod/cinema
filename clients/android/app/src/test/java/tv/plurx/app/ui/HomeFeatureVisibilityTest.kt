package tv.plurx.app.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Features
import tv.plurx.app.data.Net

/**
 * Home shows Live TV, Recordings, Library channels and Shared libraries only
 * while the server has each switched on (`GET /api/v1/features`). A server
 * that predates the route or omits a field must hide nothing.
 */
class HomeFeatureVisibilityTest {
    @Test
    fun featuresDecodeTheServerAnswer() {
        val features = Net.json.decodeFromString(
            Features.serializer(),
            """{"live_tv":false,"recordings":true,"shared_libraries":false,"library_channels":true}""",
        )
        assertEquals(Features(live_tv = false, recordings = true, shared_libraries = false, library_channels = true), features)
    }

    @Test
    fun missingFieldsDefaultToOn() {
        assertEquals(Features(), Net.json.decodeFromString(Features.serializer(), "{}"))
        val partial = Net.json.decodeFromString(Features.serializer(), """{"recordings":false,"future_switch":false}""")
        assertTrue(partial.live_tv)
        assertFalse(partial.recordings)
        assertTrue(partial.shared_libraries)
        assertTrue(partial.library_channels)
    }

    @Test
    fun everySectionShowsByDefault() {
        assertEquals(HomeHeaderSection.entries.toSet(), homeHeaderSections(Features()))
        assertEquals(listOf("home:search", "home:live", "home:shared"), homeHeaderRemoteKeys(Features()))
    }

    @Test
    fun eachSwitchHidesOnlyItsOwnSection() {
        val cases = mapOf(
            Features(live_tv = false) to HomeHeaderSection.LiveTv,
            Features(recordings = false) to HomeHeaderSection.Recordings,
            Features(library_channels = false) to HomeHeaderSection.LibraryChannels,
            Features(shared_libraries = false) to HomeHeaderSection.SharedLibraries,
        )
        cases.forEach { (features, hidden) ->
            assertEquals(HomeHeaderSection.entries.toSet() - hidden, homeHeaderSections(features))
        }
    }

    @Test
    fun hiddenSectionsLeaveTheRemoteOrder() {
        assertEquals(listOf("home:search", "home:shared"), homeHeaderRemoteKeys(Features(live_tv = false)))
        assertEquals(listOf("home:search", "home:live"), homeHeaderRemoteKeys(Features(shared_libraries = false)))
        assertEquals(
            listOf("home:search"),
            homeHeaderRemoteKeys(Features(live_tv = false, recordings = false, shared_libraries = false, library_channels = false)),
        )
    }
}
