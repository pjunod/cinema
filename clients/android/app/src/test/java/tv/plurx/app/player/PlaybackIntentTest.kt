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
import tv.plurx.app.data.ClientInfo
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.DeviceCaps
import tv.plurx.app.data.DisplayCaps
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.PresentationTarget
import tv.plurx.app.data.VideoEntry

class PlaybackIntentTest {
    private val caps = DeviceCaps(
        v = 2, client = ClientInfo("android", "test", "owned test device"),
        video = listOf(VideoEntry(codec = "h264", present = listOf("sdr"))),
        audio = listOf("aac"), containers = listOf("mp4"), transports = listOf("hls"),
        display = DisplayCaps(hdr = false, dolby_vision = false),
    )
    private fun selection(quality: QualitySelection) = ClientSelection(
        quality = quality, audioTrack = 0, subtitle = SubtitleSelection(SubtitleMode.OFF),
        audioOffsetMs = 0, codec = CodecPolicy.AUTO, dynamicRange = DynamicRangePolicy.AUTO,
    )

    @Test fun manualSessionCarriesIntentWithDisplayAwareAutoDisabled() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Q720)
        intent.setPlaybackRequested(false)
        intent.noteViewerDestination()
        val body = CreateSessionReq(playback_id = intent.playbackId, height = 720, caps = caps)
        val request = intent.bindSessionIntent(body, caps, "route-v1", "route-v1", false,
            selection(QualitySelection.Manual(720)), PresentationTarget(1920, 1080, 1))
        assertEquals(720, request.height)
        assertEquals(caps, request.caps)
        assertNull(request.caps?.display?.presentation_target)
        assertEquals(intent.playbackId, request.intent?.lifetime_id)
        assertEquals(2L, request.intent?.destination_revision)
        assertEquals(QualitySelection.Manual(720), request.intent?.selection?.quality)
        assertEquals(intent.mediaIntent(selection(QualitySelection.Manual(720))), request.intent)
    }

    @Test fun unnegotiatedSessionKeepsTheLegacyBodyWithoutMintingIntent() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Q720)
        val body = CreateSessionReq(playback_id = intent.playbackId, height = 720, caps = caps)
        for ((server, plan) in listOf(null to "route-v1", "route-v1" to null, "old" to "route-v1")) {
            assertSame(body, intent.bindSessionIntent(body, caps, server, plan, false,
                selection(QualitySelection.Manual(720)), null))
        }
        assertNull(body.intent)
        assertEquals(1L, intent.mediaIntent(selection(QualitySelection.Original)).recipe_revision)
    }

    @Test fun onlyEnabledAutoRebindsTheCandidateHeightAndMeasuredDisplay() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val id = "a".repeat(32)
        intent.requestAutomaticCandidate(id, 720)
        val body = CreateSessionReq(playback_id = intent.playbackId, height = 1080, caps = caps)
        val target = PresentationTarget(1280, 720, 2)
        val plain = intent.bindSessionIntent(body, caps, "route-v1", "route-v1", false,
            selection(QualitySelection.Auto), target)
        assertEquals(1080, plain.height)
        assertEquals(QualitySelection.Auto, plain.intent?.selection?.quality)
        assertNull(plain.caps?.display?.presentation_target)
        val automatic = intent.bindSessionIntent(body, caps, "route-v1", "route-v1", true,
            selection(QualitySelection.AutoCandidate(720, id)), target)
        assertEquals(720, automatic.height)
        assertEquals(target, automatic.caps?.display?.presentation_target)
        assertEquals(QualitySelection.AutoCandidate(720, id), automatic.intent?.selection?.quality)
        assertEquals(plain.intent?.lifetime_id, automatic.intent?.lifetime_id)
        assertEquals((plain.intent?.recipe_revision ?: 0) + 1, automatic.intent?.recipe_revision)
    }

    @Test fun aPresentedContinuousQualityKeepsLaterSeeksOnTheRetainedPlan() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val replacement = PlaybackPlanReplacement(PlaybackQuality.Auto)
        val routed = mutableListOf<Pair<Long, PlaybackQuality>>()
        replacement.retain { target, quality -> routed += target to quality }
        val quality = PlaybackQuality.Q720
        intent.beginQualityChange(quality, 0)
        replacement.presented(quality)
        intent.beginSeek(50_000, 10_000)
        assertFalse(replacement.route(intent))
        assertTrue(routed.isEmpty())
        assertTrue(replacement.route(intent, force = true))
        assertEquals(listOf(50_000L to quality), routed)
    }

    @Test
    fun failedQualityRetainsWireAndMediaWithoutLosingSavedPreferenceOrPause() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val first = intent.beginQualityChange(PlaybackQuality.Q1080)
        intent.setPlaybackRequested(false)
        val wire = QualitySelection.AutoCandidate(720, "a".repeat(32))
        assertTrue(intent.retainFailedQuality(first, wire))
        assertEquals(PlaybackQuality.Q1080, intent.desiredQuality)
        assertEquals(PlaybackQuality.Auto, intent.qualityForMedia())
        assertEquals(wire, intent.retainedControlQuality)
        assertFalse(intent.playbackRequested)
        var replans = 0
        val replacement = PlaybackPlanReplacement(PlaybackQuality.Auto)
        replacement.retain { _, _ -> replans++ }
        intent.beginSeek(90_000, 10_000)
        assertFalse(replacement.route(intent))
        assertEquals("a later seek must not implicitly apply the failed saved choice", 0, replans)
        val retry = intent.beginQualityChange(PlaybackQuality.Q1080)
        assertNull(intent.retainedControlQuality)
        assertFalse(intent.retainFailedQuality(first, QualitySelection.Original))
        assertSame(retry, intent.pendingQualityChange)
        assertTrue(intent.retainFailedQuality(retry, wire))
        assertEquals(PlaybackQuality.Auto, intent.qualityForMedia())
        intent.adoptQuality(PlaybackQuality.Q1080)
        assertNull(intent.retainedControlQuality)
        assertEquals(PlaybackQuality.Q1080, intent.qualityForMedia())
    }

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
    fun remuxPrerollSettlesOnlyAfterRenderedVideoCrossesTheTarget() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(3_413_400, 1_000_000)

        assertFalse(intent.presentedVideoProgress(3_413_500, pending.sequence))
        assertTrue(intent.markExecuted(pending.sequence))
        assertFalse(intent.presentedVideoFrame(3_413_118, pending.sequence))
        assertFalse(intent.presentedVideoProgress(3_413_300, pending.sequence))
        assertFalse(intent.presentedVideoProgress(3_413_500, pending.sequence + 1))
        assertTrue(intent.presentedVideoProgress(3_413_500, pending.sequence))
        assertNull(intent.pendingSeek)
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
    fun hlsCopyPrerollSettlesBeforeTheDestinationDeadline() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(1_291_132, 1_200_000)
        val deadline = PlaybackTargetDeadline()
        val first = Triple(pending.sequence, 1_290_664L, 1)
        assertTrue(intent.markExecuted(pending.sequence))
        assertNull(deadline.sample(intent.pendingSeek, true, true, 0))
        assertFalse(intent.presentedVideoFrame(first.second, pending.sequence))
        // The live-copy session's first keyframe precedes the seek by 468 ms.
        // Later decoded output and a clock past the target prove arrival.
        assertTrue(copyPrerollHasRenderedProgress(
            PlaybackMediaTransport.HlsCopy, pending, first, true, true, true, 30,
        ))
        assertFalse(intent.presentedVideoProgress(1_291_000, pending.sequence))
        assertTrue(intent.presentedVideoProgress(1_291_800, pending.sequence))
        assertNull(intent.pendingSeek)
        assertNull(deadline.sample(intent.pendingSeek, true, true, 8_000))
    }

    @Test
    fun copyPrerollCannotSettleFromStaleOrUnrenderedOutput() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val pending = intent.beginSeek(1_291_132, 1_200_000)
        val first = Triple(pending.sequence, 1_290_664L, 1)
        fun allowed(transport: PlaybackMediaTransport? = PlaybackMediaTransport.HlsCopy,
                    frame: Triple<Long, Long, Int> = first, foreground: Boolean = true,
                    playing: Boolean = true, selectionReady: Boolean = true, count: Int = 2) =
            copyPrerollHasRenderedProgress(transport, pending, frame, foreground, playing, selectionReady, count)
        assertTrue(allowed(PlaybackMediaTransport.ProgressiveRemux))
        assertFalse(allowed(PlaybackMediaTransport.Direct))
        assertFalse(allowed(PlaybackMediaTransport.HlsTranscode))
        assertFalse(allowed(null))
        assertFalse(allowed(frame = first.copy(first = pending.sequence - 1)))
        assertFalse(allowed(frame = first.copy(second = pending.targetMs - 2_001)))
        assertFalse(allowed(foreground = false))
        assertFalse(allowed(playing = false))
        assertFalse(allowed(selectionReady = false))
        assertFalse(allowed(count = 1))
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
