package tv.plurx.app.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class SettingsStoreTest {

    @Test
    fun pointingAtADifferentServerDropsThePreviousToken() {
        // Connect to A, log in, then connect to B and get killed before the
        // login screen. On relaunch the record must not offer A's bearer to B
        // — over plain HTTP, and A's session dies when B answers 401.
        val afterA = StoredCredentials("http://a:32400", "token-a", "paul")
        val afterB = credentialsForNewOrigin(afterA, "http://b:32400")

        assertEquals("http://b:32400", afterB.origin)
        assertNull(afterB.token)
        assertNull(afterB.username)
    }

    @Test
    fun reconnectingToTheSameServerKeepsTheSession() {
        val stored = StoredCredentials("http://a:32400", "token-a", "paul")
        assertEquals(stored, credentialsForNewOrigin(stored, "http://a:32400"))
    }

    @Test
    fun aFirstConnectionHasNothingToKeep() {
        assertEquals(
            StoredCredentials("http://a:32400", null, null),
            credentialsForNewOrigin(StoredCredentials(null, null, null), "http://a:32400"),
        )
        // A token with no origin behind it cannot be proven to belong here.
        assertNull(
            credentialsForNewOrigin(StoredCredentials("", "orphan", "paul"), "http://a:32400").token,
        )
    }

    @Test
    fun subtitleSwitchingDefaultsToAfterAShortPause() {
        // A device that has never seen the preference, or holds a value a
        // later build no longer knows, direct-plays until a subtitle is chosen
        // — the same default the Apple client settled on (2026-08-02).
        assertEquals(SubtitleReadiness.OnDemand, ViewerPreferences().subtitleReadiness)
        assertEquals(SubtitleReadiness.OnDemand, SubtitleReadiness.fromStorage(null))
        assertEquals(SubtitleReadiness.OnDemand, SubtitleReadiness.fromStorage("unknown"))
    }

    @Test
    fun subtitleSwitchingRoundTripsThroughStorage() {
        SubtitleReadiness.entries.forEach {
            assertEquals(it, SubtitleReadiness.fromStorage(it.storageValue))
        }
        // The raw values are the Apple client's `SubtitleReadiness` cases, so
        // the two settings screens describe one preference in one vocabulary.
        assertEquals("instant", SubtitleReadiness.Instant.storageValue)
        assertEquals("onDemand", SubtitleReadiness.OnDemand.storageValue)
        assertEquals("Instant", SubtitleReadiness.Instant.label)
        assertEquals("After a short pause", SubtitleReadiness.OnDemand.label)
    }
}
