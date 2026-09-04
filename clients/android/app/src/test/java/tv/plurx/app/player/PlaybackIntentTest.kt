package tv.plurx.app.player

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
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
    fun audioSubtitleOffsetAndQualityChangesRetainAnUnpresentedSeekTarget() {
        val intent = PlaybackIntent("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", PlaybackQuality.Auto)
        var pending = intent.beginSeek(90_000, 10_000)
        assertTrue(intent.markExecuted(pending.sequence))

        for (quality in listOf(
            PlaybackQuality.Auto,
            PlaybackQuality.Q720,
            PlaybackQuality.Original,
        )) {
            val inherited = intent.positionForPlaybackIntent(10_000)
            assertEquals(90_000L, inherited)
            pending = intent.beginSeek(inherited, 10_000, quality)
            assertEquals(90_000L, pending.targetMs)
            assertTrue(intent.markExecuted(pending.sequence))
        }
    }

    @Test
    fun delayedQualityBPublicationCannotReloadAfterQualityC() = runBlocking {
        val intent = PlaybackIntent("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", PlaybackQuality.Auto)
        val first = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q720)
        val firstStarted = CompletableDeferred<Unit>()
        val releaseFirst = CompletableDeferred<Unit>()
        val stale = async {
            publishReplacementIntent(intent, first, PlaybackQuality.Q720) {
                firstStarted.complete(Unit)
                releaseFirst.await()
                7L
            }
        }
        firstStarted.await()

        val newest = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q1080)
        assertTrue(
            "the newest immutable quality owns the reload",
            publishReplacementIntent(intent, newest, PlaybackQuality.Q1080) { 8L },
        )
        releaseFirst.complete(Unit)

        assertFalse("the delayed older publication cannot reload", stale.await())
        assertEquals(PlaybackQuality.Q1080, intent.desiredQuality)
        assertEquals(8L, intent.controlSequenceFloor)
    }

    @Test
    fun delayedControlBootstrapBelongsOnlyToItsCurrentSession() {
        val fence = PlaybackControlBootstrapFence()
        val sessionA = fence.claim("session-a")
        val sessionB = fence.claim("session-b")

        assertFalse(fence.isCurrent(sessionA, "session-b"))
        assertTrue(fence.isCurrent(sessionB, "session-b"))

        fence.invalidate()
        assertFalse("departure invalidates a pending probe", fence.isCurrent(sessionB, "session-b"))
        val reopenedSameSession = fence.claim("session-b")
        assertFalse(fence.isCurrent(sessionB, "session-b"))
        assertTrue(fence.isCurrent(reopenedSameSession, "session-b"))
        fence.release()
        assertFalse("release invalidates a pending probe", fence.isCurrent(reopenedSameSession, "session-b"))
        assertFalse("a released controller cannot bootstrap again", fence.isCurrent(fence.claim("session-c"), "session-c"))
    }

    @Test
    fun delayedQualityPublicationCannotReloadAReleasedController() = runBlocking {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val lifetime = PlaybackControlBootstrapFence()
        val pending = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q720)
        val started = CompletableDeferred<Unit>()
        val releasePublication = CompletableDeferred<Unit>()
        val publication = async {
            publishReplacementIntent(intent, pending, PlaybackQuality.Q720, lifetime::isActive) {
                started.complete(Unit)
                releasePublication.await()
                7L
            }
        }
        started.await()
        lifetime.release()
        releasePublication.complete(Unit)

        assertFalse(publication.await())
        assertSame("intent survives for the new controller", pending, intent.pendingSeek)
        assertNull("a retired publisher cannot change ordering", intent.controlSequenceFloor)
    }

    @Test
    fun commandsDuringQualityPublicationReloadTheLatestTargetAndSelections() = runBlocking {
        data class Reload(val targetMs: Long, val quality: PlaybackQuality, val audio: Long, val subtitle: Long?, val offsetMs: Long)
        for (command in listOf("seek", "audio", "subtitle", "offset")) {
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val replacement = PlaybackPlanReplacement(PlaybackQuality.Auto)
            val reloads = mutableListOf<Reload>()
            var audio = 0L
            var subtitle: Long? = null
            var offset = 0L
            replacement.retain { target, quality -> reloads += Reload(target, quality, audio, subtitle, offset) }
            val qualityCommand = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q720)
            val publicationStarted = CompletableDeferred<Unit>()
            val finishPublication = CompletableDeferred<Unit>()
            val old = async {
                if (publishReplacementIntent(intent, qualityCommand, PlaybackQuality.Q720) {
                        publicationStarted.complete(Unit)
                        finishPublication.await()
                        7L
                    }
                ) replacement.route(intent, force = true)
            }
            publicationStarted.await()

            when (command) {
                "audio" -> audio = 2
                "subtitle" -> subtitle = 3
                "offset" -> offset = 250
            }
            val target = if (command == "seek") 120_000L else intent.positionForPlaybackIntent(10_000)
            val newest = intent.beginSeek(target, 10_000)
            assertTrue(publishReplacementIntent(intent, newest, PlaybackQuality.Q720) { 8L })
            assertTrue("the old plan cannot execute the new quality", replacement.route(intent))
            finishPublication.complete(Unit)
            old.await()

            assertEquals(listOf(Reload(target, PlaybackQuality.Q720, audio, subtitle, offset)), reloads)
            assertTrue("repeat callbacks consume the same reload", replacement.route(intent))
            assertEquals(1, reloads.size)
            replacement.release()
            assertFalse("a released controller cannot reload", replacement.route(intent))
        }
    }

    @Test
    fun transportIntentDuringPublicationSurvivesQualityAndControllerReplacement() = runBlocking {
        for ((before, after) in listOf(true to false, false to true, false to false)) {
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            intent.setPlaybackRequested(before)
            val pending = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q720)
            val publicationStarted = CompletableDeferred<Unit>()
            val finishPublication = CompletableDeferred<Unit>()
            val publication = async {
                publishReplacementIntent(intent, pending, PlaybackQuality.Q720) {
                    publicationStarted.complete(Unit)
                    finishPublication.await()
                    7L
                }
            }
            publicationStarted.await()
            intent.setPlaybackRequested(after)
            finishPublication.complete(Unit)
            assertTrue("pause/play does not discard the requested media command", publication.await())
            // New Controller construction adopts its immutable plan but keeps
            // the presentation's newest transport demand for the actual attach.
            intent.adoptQuality(PlaybackQuality.Q720)
            assertEquals(after, intent.playbackRequested)
            assertEquals(90_000L, intent.pendingSeek?.targetMs)
        }
    }

    @Test
    fun targetTimeoutReallyRetriesTheSamePendingScreenReplacement() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val replacement = PlaybackPlanReplacement(PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000, PlaybackQuality.Q720)
        val reloads = mutableListOf<Pair<Long, PlaybackQuality>>()
        replacement.retain { target, quality -> reloads += target to quality }
        assertTrue(replacement.route(intent))
        assertTrue(replacement.route(intent))
        assertEquals(1, reloads.size)

        val deadline = PlaybackTargetDeadline()
        deadline.sample(pending, true, true, 0)
        val expired = deadline.sample(pending, true, true, 8_000)!!
        assertTrue(deadline.recover(expired, 8_000))
        assertTrue(replacement.route(intent, retry = true))
        assertEquals("a spent retry must issue another plan request", 2, reloads.size)
        assertEquals(listOf(90_000L to PlaybackQuality.Q720, 90_000L to PlaybackQuality.Q720), reloads)
        assertTrue(deadline.sample(pending, true, true, 16_000)!!.terminal)
    }

    @Test
    fun delayedSubtitleRetryCannotMutateAReplacementOrReleasedSession() {
        val lifetime = PlaybackControlBootstrapFence()
        lifetime.claim("session-a")
        val delayedRetry = lifetime.snapshot("session-a")
        assertTrue(lifetime.isCurrent(delayedRetry, "session-a"))

        lifetime.invalidate()
        assertFalse(lifetime.isCurrent(delayedRetry, null))
        lifetime.claim("session-a")
        assertFalse("reusing an id does not reuse callback ownership", lifetime.isCurrent(delayedRetry, "session-a"))
        val replacementRetry = lifetime.snapshot("session-a")
        lifetime.release()
        assertFalse(lifetime.isCurrent(replacementRetry, "session-a"))
    }

    @Test
    fun inPlaceSubtitleChangeMustExecuteAndPresentItsInheritedSeek() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        assertFalse(intent.needsSeekForInPlaceSelection())
        val oldSeek = intent.beginSeek(90_000, 10_000)
        assertTrue(intent.needsSeekForInPlaceSelection())
        val selection = intent.beginSeek(intent.positionForPlaybackIntent(10_000), 10_000)
        assertFalse("the old coalesced seek is superseded", intent.isCurrent(oldSeek.sequence))
        assertFalse("track selection cannot settle before execution", intent.presentedInPlace(selection.sequence))
        intent.markExecuted(selection.sequence)
        assertFalse("departing output cannot acknowledge the inherited target", intent.presentedVideoFrame(10_000, selection.sequence))
        assertTrue(intent.presentedVideoFrame(90_000, selection.sequence))
        assertFalse(intent.needsSeekForInPlaceSelection())
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
        assertTrue(intent.markExecuted(pending.sequence, observedAtMs = 0))
        assertFalse("READY at the target is not clock movement", intent.presentedAudio(45_000, pending.sequence, observedAtMs = 0))
        assertFalse("a frozen clock must retain intent", intent.presentedAudio(45_000, pending.sequence, observedAtMs = 500))
        assertFalse("the audio clock must advance, not retreat", intent.presentedAudio(44_999, pending.sequence, observedAtMs = 500))
        assertTrue(
            "the next one-second monitor sample proves audible presentation",
            intent.presentedAudio(46_000, pending.sequence, observedAtMs = 1_000),
        )
        assertNull(intent.pendingSeek)
        assertNull(intent.controlSequenceFloor)
    }

    @Test
    fun firstAudioSampleAtProductionCadenceCanLandAtEachPlaybackRate() {
        for (rate in listOf(0.5, 1.0, 2.0)) {
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            val pending = intent.beginSeek(90_000, 10_000)
            intent.markExecuted(pending.sequence, observedAtMs = 0, playbackRate = rate)

            assertFalse("one stale source-clock sample cannot land", intent.presentedAudio(10_000, pending.sequence, 1_000, playbackRate = rate))
            val firstPosition = 90_000L + (rate * 1_000).toLong()
            assertFalse("one late landing sample is not presentation", intent.presentedAudio(firstPosition, pending.sequence, 1_000, playbackRate = rate))
            assertTrue("the next cadence sample proves playback", intent.presentedAudio(firstPosition + (rate * 1_000).toLong(), pending.sequence, 2_000, playbackRate = rate))
        }
    }

    @Test
    fun pausedTimeCannotWidenAudioLandingAndResumeNeedsFreshClockAdvance() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(90_000, 10_000)
        intent.markExecuted(pending.sequence, observedAtMs = 0, playbackActive = false)
        assertFalse(intent.presentedAudio(90_000, pending.sequence, 20_000, playbackActive = false))
        assertFalse("a long pause cannot admit a departed clock", intent.presentedAudio(100_000, pending.sequence, 20_000))
        assertFalse(intent.presentedAudio(91_000, pending.sequence, 21_000))
        assertFalse("pause clears the landing sample", intent.presentedAudio(91_000, pending.sequence, 21_000, playbackActive = false))
        assertFalse("resume still needs a later sample", intent.presentedAudio(91_000, pending.sequence, 40_000))
        assertTrue(intent.presentedAudio(92_000, pending.sequence, 41_000))
    }

    @Test
    fun audioLandingAllowanceIsBoundedAndOldGenerationCannotSeedSuccessor() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val first = intent.beginSeek(90_000, 10_000)
        intent.markExecuted(first.sequence, observedAtMs = 0)
        assertFalse("elapsed time beyond the deadline cannot widen landing", intent.presentedAudio(120_000, first.sequence, 30_000))

        val second = intent.beginSeek(45_000, 90_000)
        intent.markExecuted(second.sequence, observedAtMs = 30_000)
        assertFalse(intent.presentedAudio(91_000, first.sequence, 31_000))
        assertFalse(intent.presentedAudio(46_000, second.sequence, 31_000))
        assertTrue(intent.presentedAudio(47_000, second.sequence, 32_000))
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
