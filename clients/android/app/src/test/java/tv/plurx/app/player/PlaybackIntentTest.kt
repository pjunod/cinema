package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class PlaybackIntentTest {
    @Test
    fun rapidSeeksRetainOnlyTheNewestDestination() {
        val intent = PlaybackIntent("11111111-1111-4111-8111-111111111111", PlaybackQuality.Auto)
        val first = intent.beginSeek(30_000, 10_000)
        val final = intent.beginSeek(90_000, 10_000)

        assertFalse(
            "a superseded render cannot clear intent",
            intent.presentedVideoFrame(30_000, first.sequence),
        )
        assertSame(final, intent.pendingSeek)
        assertTrue(intent.markExecuted(final.sequence))
        assertFalse("the old loose tolerance cannot settle", intent.presentedVideoFrame(90_400, final.sequence))
        assertTrue(intent.presentedVideoFrame(90_200, final.sequence))
        assertNull(intent.pendingSeek)
    }

    @Test
    fun qualityAndIdentitySurviveAControllerReplacement() {
        val intent = PlaybackIntent("22222222-2222-4222-8222-222222222222", PlaybackQuality.Auto)
        intent.beginSeek(45_000, 44_000, PlaybackQuality.Q720)

        assertEquals("22222222-2222-4222-8222-222222222222", intent.playbackId)
        assertEquals(PlaybackQuality.Q720, intent.desiredQuality)
        assertEquals(45_000L, intent.pendingSeek?.targetMs)
    }

    @Test
    fun controllerAdoptsTheQualityOfItsExactPlan() {
        val intent = PlaybackIntent("33333333-3333-4333-8333-333333333333", PlaybackQuality.Auto)

        intent.adoptQuality(PlaybackQuality.Original)

        assertEquals(PlaybackQuality.Original, intent.desiredQuality)
    }

    @Test
    fun departingAndPreExecutionFramesCannotSettleANewSeek() {
        val intent = PlaybackIntent("44444444-4444-4444-8444-444444444444", PlaybackQuality.Auto)
        val pending = intent.beginSeek(10_100, 10_000)

        assertFalse("an outgoing frame can be near a short seek", intent.presentedVideoFrame(10_000))
        assertTrue(intent.markExecuted(pending.sequence))
        assertTrue("the first post-execution target frame settles", intent.presentedVideoFrame(10_100))
    }

    @Test
    fun replacementRetainsTheControlOrderingFloorUntilPresentation() {
        val intent = PlaybackIntent("55555555-5555-4555-8555-555555555555", PlaybackQuality.Auto)
        val pending = intent.beginSeek(45_000, 10_000, PlaybackQuality.Q720)
        intent.retainControlSequence(9)

        assertEquals(9L, intent.orderedControlSequence(4))
        assertTrue(intent.markExecuted(pending.sequence))
        assertTrue(intent.presentedVideoFrame(45_000, pending.sequence))
        assertNull(intent.controlSequenceFloor)
    }
}
