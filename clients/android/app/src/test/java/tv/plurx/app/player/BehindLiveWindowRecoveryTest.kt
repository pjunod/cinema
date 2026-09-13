package tv.plurx.app.player

import androidx.media3.common.PlaybackException
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * M5 addition 3 at its own seam.
 *
 * `Controller` is not constructible in a JVM unit test — it takes an
 * `ExoPlayer`, a `MediaSession` and the app's view model — so the recovery is
 * an object with the player behind three lambdas and this drives it directly.
 * What survives the extraction is exactly the addition's claim: one seek and
 * one prepare for the first qualifying error, nothing for the second on the
 * same attach, a live item untouched, and a fresh budget when a new item takes
 * the screen.
 *
 * UNRUN: there is no Android toolchain on the machine this was written on. See
 * the PR's "Needs an Android toolchain or a device" list.
 */
class BehindLiveWindowRecoveryTest {
    private class FakePlayer {
        val seeks = mutableListOf<Long>()
        var prepares = 0
    }

    private fun run(
        recovery: BehindLiveWindowRecovery,
        player: FakePlayer,
        errorCode: Int = ERROR_CODE_BEHIND_LIVE_WINDOW,
        live: Boolean = false,
        target: Long = 412_500,
    ): Boolean = recovery.recover(
        errorCode = errorCode,
        live = live,
        seekTargetMs = { target },
        seekTo = { player.seeks += it },
        prepare = { player.prepares += 1 },
    )

    @Test
    fun theFirstFiniteBehindLiveWindowSeeksBackOnceAndPreparesOnce() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        assertTrue(run(recovery, player))
        assertEquals(listOf(412_500L), player.seeks)
        assertEquals(1, player.prepares)
        assertTrue(recovery.spent)
    }

    @Test
    fun theSecondOneOnTheSameAttachDoesNothingAtAll() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        assertTrue(run(recovery, player))
        assertFalse(
            "a second 1002 on the same attach is the recovery not having worked",
            run(recovery, player, target = 999_000),
        )
        assertEquals(listOf(412_500L), player.seeks)
        assertEquals(1, player.prepares)
    }

    @Test
    fun aNewItemOnTheScreenGetsItsOwnSingleRecovery() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        assertTrue(run(recovery, player))
        assertFalse(run(recovery, player))
        // `attachRecipe` and `commitPreparedReplacement` both call this: a
        // prepared successor is a different ExoPlayer with its own item, and it
        // must not inherit the predecessor's spent budget.
        recovery.attached()
        assertFalse(recovery.spent)
        assertTrue(run(recovery, player, target = 900_000))
        assertEquals(listOf(412_500L, 900_000L), player.seeks)
        assertEquals(2, player.prepares)
    }

    @Test
    fun aLiveItemIsNeverSeekedAndNeverPrepared() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        assertFalse(
            "seekToDefaultPosition is a live-edge policy the contract forbids, " +
                "and inventing a last real position on a live window is the same skip",
            run(recovery, player, live = true),
        )
        assertTrue(player.seeks.isEmpty())
        assertEquals(0, player.prepares)
        assertFalse("a refused live error does not spend the budget", recovery.spent)
    }

    @Test
    fun noOtherErrorCodeTouchesThePlayer() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        for (code in listOf(
            PlaybackException.ERROR_CODE_IO_BAD_HTTP_STATUS,
            PlaybackException.ERROR_CODE_DECODING_FAILED,
            PlaybackException.ERROR_CODE_PARSING_CONTAINER_MALFORMED,
            PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
            PlaybackException.ERROR_CODE_TIMEOUT,
        )) {
            assertFalse("code $code", run(recovery, player, errorCode = code))
        }
        assertTrue(player.seeks.isEmpty())
        assertEquals(0, player.prepares)
        assertFalse(recovery.spent)
    }

    @Test
    fun theSeekTargetIsReadOnlyOnThePathThatSeeks() {
        val recovery = BehindLiveWindowRecovery()
        val player = FakePlayer()
        var reads = 0
        fun attempt(live: Boolean) = recovery.recover(
            errorCode = ERROR_CODE_BEHIND_LIVE_WINDOW,
            live = live,
            seekTargetMs = { reads += 1; 1_000 },
            seekTo = { player.seeks += it },
            prepare = { player.prepares += 1 },
        )
        assertFalse(attempt(live = true))
        assertEquals(0, reads)
        assertTrue(attempt(live = false))
        assertEquals(1, reads)
    }
}
