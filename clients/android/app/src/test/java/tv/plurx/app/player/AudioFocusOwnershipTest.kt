package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

class AudioFocusOwnershipTest {
    @Test
    fun aPreparedSuccessorNeverHoldsAudioFocusBeforeItsSwitch() {
        // Measured on the Google TV Streamer (2026-10-02): the successor's
        // focus request paused the player on screen for 20 s, then both were
        // released and the session restarted cold.
        assertFalse(handlesAudioFocus(PlayerRole.Successor))
        for (role in PlayerRole.entries.filter { it != PlayerRole.Successor }) {
            assertTrue(handlesAudioFocus(role), "$role plays what the viewer hears")
        }
    }

    @Test
    fun focusIsReleasedByTheOldOwnerBeforeTheNewOwnerTakesIt() {
        val calls = mutableListOf<String>()
        val incumbent = AudioFocusOwner { owns -> calls += "incumbent:$owns" }
        val successor = AudioFocusOwner { owns -> calls += "successor:$owns" }
        handOverAudioFocus(from = incumbent, to = successor)
        assertEquals(listOf("incumbent:false", "successor:true"), calls)
        calls.clear()
        // A rollback hands it straight back the same way.
        handOverAudioFocus(from = successor, to = incumbent)
        assertEquals(listOf("successor:false", "incumbent:true"), calls)
    }
}
