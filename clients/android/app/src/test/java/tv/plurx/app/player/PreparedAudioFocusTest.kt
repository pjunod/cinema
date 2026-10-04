package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Test

class PreparedAudioFocusTest {
    private val events = mutableListOf<String>()
    private fun player(name: String) = AudioFocusHandling { events += "$name:${if (it) "request" else "release"}" }

    @Test fun aStagedSuccessorNeverRequestsFocusAndCommitReleasesBeforeRequesting() {
        // The successor used to request focus while muted; the incumbent got a
        // focus loss and paused while it was still the visible picture.
        val incumbent = player("incumbent"); val successor = player("successor")
        PreparedAudioFocus.stage(successor)
        assertEquals(listOf("successor:release"), events)
        PreparedAudioFocus.move(incumbent, successor)
        assertEquals(listOf("successor:release", "incumbent:release", "successor:request"), events)
    }

    @Test fun rollbackReturnsFocusToTheRestoredPlayer() {
        val incumbent = player("incumbent"); val successor = player("successor")
        PreparedAudioFocus.move(successor, incumbent)
        assertEquals(listOf("successor:release", "incumbent:request"), events)
    }

    @Test fun rollbackSilencesTheFailedSuccessorBeforeFocusMovesBack() {
        // The failed successor stays parked until collected (up to the overlap
        // bound). It used to lose focus but keep playing, audible over the
        // restored player.
        val incumbent = player("incumbent"); val successor = player("successor")
        PreparedAudioFocus.rollback(successor, PlaybackSilencing { events += "successor:silence" }, incumbent)
        assertEquals(listOf("successor:silence", "successor:release", "incumbent:request"), events)
    }
}
