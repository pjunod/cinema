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
    fun rapidRelativeSeeksAccumulateAgainstTheOptimisticDestination() {
        val intent = PlaybackIntent("77777777-7777-4777-8777-777777777777", PlaybackQuality.Auto)

        assertEquals(20_000L, intent.beginRelativeSeek(10_000, 10_000, 120_000).targetMs)
        assertEquals(30_000L, intent.beginRelativeSeek(10_000, 10_000, 120_000).targetMs)
        assertEquals(40_000L, intent.beginRelativeSeek(10_000, 10_000, 120_000).targetMs)
        assertEquals(30_000L, intent.beginRelativeSeek(-10_000, 10_000, 120_000).targetMs)
        assertEquals(30_000L, intent.pendingSeek?.targetMs)
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
    fun capturedFrameGenerationCannotSettleItsExecutedSuccessor() {
        val intent = PlaybackIntent("99999999-9999-4999-8999-999999999999", PlaybackQuality.Auto)
        val predecessor = intent.beginSeek(20_000, 10_000)
        assertTrue(intent.markExecuted(predecessor.sequence))
        assertEquals(predecessor.sequence, intent.executedSequence())

        val successor = intent.beginSeek(30_000, 10_000)
        assertNull(intent.executedSequence())
        assertFalse(intent.presentedVideoFrame(20_000, predecessor.sequence))
        assertSame(successor, intent.pendingSeek)

        assertTrue(intent.markExecuted(successor.sequence))
        assertEquals(successor.sequence, intent.executedSequence())
        assertTrue(intent.presentedVideoFrame(30_000, successor.sequence))
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

    @Test
    fun audioSettlementRequiresLandingThenPostExecutionClockAdvance() {
        val intent = PlaybackIntent("66666666-6666-4666-8666-666666666666", PlaybackQuality.Auto)
        val pending = intent.beginSeek(45_000, 10_000)
        intent.retainControlSequence(11)

        assertFalse("pre-execution state cannot present", intent.presentedAudio(45_000, pending.sequence))
        assertTrue(intent.markExecuted(pending.sequence))
        assertFalse("READY at the target is not clock movement", intent.presentedAudio(45_000, pending.sequence))
        assertFalse("a frozen clock must retain intent", intent.presentedAudio(45_000, pending.sequence))
        assertFalse("the audio clock must advance, not retreat", intent.presentedAudio(44_999, pending.sequence))
        assertTrue(
            "the next one-second monitor sample proves audible presentation",
            intent.presentedAudio(46_000, pending.sequence),
        )
        assertNull(intent.pendingSeek)
        assertNull(intent.controlSequenceFloor)
    }

    @Test
    fun anExecutedInPlaceTrackChangeSettlesWithoutInventingAFrameGeneration() {
        val intent = PlaybackIntent("88888888-8888-4888-8888-888888888888", PlaybackQuality.Auto)
        val pending = intent.beginSeek(45_000, 45_000)
        intent.retainControlSequence(12)

        assertFalse(intent.presentedInPlace(pending.sequence))
        assertTrue(intent.markExecuted(pending.sequence))
        assertTrue(intent.presentedInPlace(pending.sequence))
        assertNull(intent.pendingSeek)
        assertNull(intent.controlSequenceFloor)
    }
}
