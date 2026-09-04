package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class PlaybackTargetDeadlineTest {
    @Test
    fun advancingWrongTimelineCannotHideADestinationTimeoutAndOnlyOneRecoveryIsAllowed() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        val deadline = PlaybackTargetDeadline()
        val clockStall = OpenPlaybackStallTracker()
        for (second in 0L..7) {
            // The predecessor keeps progressing, which is precisely why the
            // ordinary no-progress watchdog cannot own presentation recovery.
            assertNull(clockStall.sample(true, false, true, 10_000 + second * 1_000, second * 1_000))
            assertNull(deadline.sample(pending, true, true, second * 1_000))
        }
        val recovery = deadline.sample(pending, true, true, 8_000)!!
        assertEquals(pending.sequence, recovery.sequence)
        assertEquals(90_000L, recovery.targetMs)
        assertFalse(recovery.terminal)
        assertTrue(deadline.recover(recovery, 8_000))
        intent.markExecuted(pending.sequence, observedAtMs = 8_000)
        assertNull(deadline.sample(intent.pendingSeek, true, true, 15_999))
        val terminal = deadline.sample(intent.pendingSeek, true, true, 16_000)!!
        assertTrue(terminal.terminal)
        assertFalse(deadline.recover(terminal, 16_000))
        assertNull("terminal is emitted only once", deadline.sample(intent.pendingSeek, true, true, 30_000))
    }

    @Test
    fun pausedAndBackgroundTimeDoesNotConsumeThePresentationDeadline() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        val deadline = PlaybackTargetDeadline()
        assertNull(deadline.sample(pending, true, true, 0))
        assertNull(deadline.sample(pending, false, true, 3_000))
        assertNull(deadline.sample(pending, false, true, 30_000))
        assertNull(deadline.sample(pending, true, false, 40_000))
        assertNull(deadline.sample(pending, true, true, 60_000))
        assertNull(deadline.sample(pending, true, true, 64_999))
        assertNotNull(deadline.sample(pending, true, true, 65_000))
    }

    @Test
    fun supersedingCommandAndReleaseInvalidateAnOldTimeout() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val first = intent.beginSeek(90_000, 10_000)
        val deadline = PlaybackTargetDeadline()
        deadline.sample(first, true, true, 0)
        val expired = deadline.sample(first, true, true, 8_000)!!

        val newer = intent.beginSeek(120_000, 10_000)
        assertNull(deadline.sample(newer, true, true, 8_000))
        assertFalse("a delayed old event cannot restart the new destination", deadline.recover(expired, 8_000))
        assertNull(deadline.sample(newer, true, true, 15_999))
        val next = deadline.sample(newer, true, true, 16_000)!!
        assertFalse("new viewer intent owns a fresh budget", next.terminal)
        deadline.reset()
        assertFalse("release invalidates an already-delivered callback", deadline.recover(next, 16_000))
    }

    @Test
    fun presentationClearsTheDeadlineAndMonotonicRemainingTimeDrivesTheNextWakeup() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        val deadline = PlaybackTargetDeadline()
        deadline.sample(pending, true, true, 17)
        assertEquals(517L, deadline.nextSampleDelayMs(7_500))
        intent.markExecuted(pending.sequence)
        assertTrue(intent.presentedVideoFrame(90_000, pending.sequence))
        assertNull(deadline.sample(intent.pendingSeek, true, true, 8_017))
        assertEquals(1_000L, deadline.nextSampleDelayMs(8_017))
    }

    @Test
    fun targetRecoveryInvalidatesDelayedPublicationForTheSameViewerGeneration() = runBlocking {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        val deadline = PlaybackTargetDeadline()
        deadline.sample(pending, true, true, 0)
        var mediaMutationEpoch = 0L
        val publicationEpoch = mediaMutationEpoch
        val started = CompletableDeferred<Unit>()
        val finishPublication = CompletableDeferred<Unit>()
        val publication = async {
            publishReplacementIntent(intent, pending, PlaybackQuality.Auto,
                isActive = { mediaMutationEpoch == publicationEpoch },
            ) {
                started.complete(Unit)
                finishPublication.await()
                7L
            }
        }
        started.await()
        val event = deadline.sample(pending, true, true, 8_000)!!
        mediaMutationEpoch += 1
        assertTrue(deadline.recover(event, 8_000))
        finishPublication.complete(Unit)

        assertFalse("the older publication cannot restart after timeout recovery", publication.await())
        assertTrue("the viewer's destination survives recovery", intent.isCurrent(pending.sequence))
        assertNull(intent.controlSequenceFloor)
    }
}
