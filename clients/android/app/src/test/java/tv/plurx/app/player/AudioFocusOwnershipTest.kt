package tv.plurx.app.player

import java.io.File
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

    private fun source(name: String): String = listOf(
        File("app/src/main/java/tv/plurx/app/player/$name"),
        File("src/main/java/tv/plurx/app/player/$name"),
        File("clients/android/app/src/main/java/tv/plurx/app/player/$name"),
    ).firstOrNull(File::isFile)?.readText() ?: error("$name source not found")

    /** The body of one function, up to the next top-level or member declaration. */
    private fun body(source: String, signature: String): String {
        val start = source.indexOf(signature)
        assertTrue(start >= 0, "$signature is not declared")
        val rest = source.substring(start + signature.length)
        val next = Regex("\n(    )?(private |internal |override |@)?fun ").find(rest)?.range?.first ?: rest.length
        return rest.substring(0, next)
    }

    /**
     * The handover lives in the controller, which no JVM test can construct
     * (no Robolectric here). Pin the four places a revert would reintroduce
     * the freeze: the builder honouring the role, the successor dropping focus
     * before it plays, the commit taking it, and the rollback giving it back.
     */
    @Test
    fun everySwapSiteMovesFocusToTheAudiblePlayer() {
        val builder = source("PlurxPlayerBuilder.kt")
        assertTrue(builder.contains(".setAudioAttributes(PLURX_MEDIA_AUDIO_ATTRIBUTES, handlesAudioFocus(role))"))
        assertTrue(builder.contains(".setHandleAudioBecomingNoisy(handlesAudioFocus(role))"))

        val controller = source("Controller.kt")
        val successor = body(controller, "fun buildSuccessorPlayer(")
        val drop = successor.indexOf("asAudioFocusOwner().ownAudioFocus(false)")
        val play = successor.indexOf("playWhenReady = true")
        assertTrue(drop in 0 until play, "a successor drops focus before it plays")

        val commit = body(controller, "private fun commitPreparedReplacement(")
        val take = commit.indexOf("handOverAudioFocus(previous.asAudioFocusOwner(), successor.asAudioFocusOwner())")
        assertTrue(take >= 0, "the commit hands focus to the successor")
        assertTrue(take < commit.indexOf("successor.playWhenReady = previousPlayWhenReady"))

        val rollback = body(controller, "private fun rollbackSwitchedReplacement(")
        val back = rollback.indexOf("handOverAudioFocus(failedSuccessor.asAudioFocusOwner(), predecessor.player.asAudioFocusOwner())")
        assertTrue(back >= 0, "a rollback hands focus back to the predecessor")
        assertTrue(back < rollback.indexOf("predecessor.player.playWhenReady = effectivePlayWhenReady()"))
    }

    @Test
    fun theOutgoingOwnerReleasesBeforeTheIncomingOwnerTakes() {
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
