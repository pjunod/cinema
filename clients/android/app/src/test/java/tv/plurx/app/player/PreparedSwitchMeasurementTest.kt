package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * M3's arithmetic, which is the half of measuring a switch that the device lane
 * cannot check. A hardware run can tell you the number it printed; only this can
 * tell you the number was the right one for the window it claimed.
 *
 * Each case pins one decision: the window edges, the two-sided sum, the counter
 * reset, events-are-not-counters, and the "not measured" that must never be
 * rendered as a zero. The PR names the mutation that each one catches.
 */
class PreparedSwitchMeasurementTest {
    private fun sample(atMs: Long, count: Long) = PreparedSwitchSample(atMs, count)

    @Test
    fun theTwoHalvesOfTheWindowAreAddedRatherThanCompared() {
        // The predecessor dropped two frames in its last two seconds and the
        // successor one in its first two. A prepared switch is both.
        val measured = preparedSwitchCounterDelta(
            before = listOf(sample(8_000, 5), sample(10_000, 7)),
            after = listOf(sample(10_000, 0), sample(12_000, 1)),
            commitAtMs = 10_000,
        )
        assertEquals(3L, measured.count)
        assertEquals(2_000L, measured.beforeMs)
        assertEquals(2_000L, measured.afterMs)
        assertEquals(4_000L, measured.spanMs)
        assertEquals(
            "3 dropped · ±2.0 s · 4.0 s of 4.0 s sampled",
            preparedSwitchFramesRow(measured),
        )
    }

    @Test
    fun dropsOutsideTheWindowAreNotTheSwitchs() {
        // Without the window filter the predecessor's whole session — forty
        // dropped frames from a bad stretch minutes ago — is attributed to the
        // switch, and no switch could ever pass.
        val measured = preparedSwitchCounterDelta(
            before = listOf(sample(1_000, 0), sample(7_999, 40), sample(8_500, 41), sample(10_000, 41)),
            after = emptyList(),
            commitAtMs = 10_000,
        )
        assertEquals(0L, measured.count)
        assertEquals(1_500L, measured.beforeMs)
    }

    @Test
    fun bothEdgesOfTheWindowAreInside() {
        val measured = preparedSwitchCounterDelta(
            before = listOf(sample(8_000, 0), sample(10_000, 2)),
            after = listOf(sample(10_000, 0), sample(12_000, 3)),
            commitAtMs = 10_000,
        )
        assertEquals(5L, measured.count)
    }

    @Test
    fun aCounterThatWentBackwardsWasResetAndNeverGoesNegative() {
        val measured = preparedSwitchCounterDelta(
            before = emptyList(),
            after = listOf(sample(10_100, 900), sample(11_000, 4)),
            commitAtMs = 10_000,
        )
        assertEquals(4L, measured.count)
    }

    @Test
    fun oneReadingPerSideIsNotAMeasurementAndIsNotAZero() {
        val measured = preparedSwitchCounterDelta(
            before = listOf(sample(9_000, 3)),
            after = emptyList(),
            commitAtMs = 10_000,
        )
        assertNull(measured.count)
        assertEquals("Not measured", preparedSwitchFramesRow(measured))
        assertEquals("Not measured", preparedSwitchFramesRow(null))
    }

    @Test
    fun underrunsAreEventsAndAnEmptyWindowIsAMeasuredZero() {
        // The distinction with teeth. Reduced as a counter, a clean switch has
        // one reading per side, no delta, and reports "not measured" — for
        // exactly the outcome §1's bar is about.
        assertEquals(0L, preparedSwitchEventCount(emptyList(), commitAtMs = 10_000))
        assertEquals(
            "0 audio underruns · ±2.0 s",
            preparedSwitchAudioUnderrunRow(preparedSwitchEventCount(emptyList(), 10_000)),
        )
        assertEquals(
            2L,
            preparedSwitchEventCount(listOf(9_000L, 10_500L, 13_000L), commitAtMs = 10_000),
        )
        assertEquals(
            "1 audio underrun · ±2.0 s",
            preparedSwitchAudioUnderrunRow(preparedSwitchEventCount(listOf(10_001L), 10_000)),
        )
        // The edges again, on the event path this time.
        assertEquals(2L, preparedSwitchEventCount(listOf(8_000L, 12_000L), 10_000))
        assertEquals(0L, preparedSwitchEventCount(listOf(7_999L, 12_001L), 10_000))
    }

    @Test
    fun tapToNewQualityRefusesTwoClocksThatDisagree() {
        assertEquals(412L, preparedSwitchVisibleInMs(1_000, 1_412))
        assertNull(preparedSwitchVisibleInMs(1_000, 999))
        assertNull(preparedSwitchVisibleInMs(null, 1_412))
        assertNull(preparedSwitchVisibleInMs(1_000, null))
        assertEquals("412 ms", preparedSwitchVisibleRow(412))
        assertEquals("Not measured", preparedSwitchVisibleRow(null))
    }

    @Test
    fun theRecorderKeepsThePipelinesApartAcrossTwoSwitches() {
        // A second directed change makes the first switch's successor the
        // second switch's predecessor. Keyed by a before/after flag captured at
        // prepare time, the second switch would add one pipeline to itself.
        val recorder = PreparedSwitchRecorder()
        val first = 1
        val second = 2
        val third = 3
        recorder.noteDroppedFrames(0, 1, first)
        recorder.noteDroppedFrames(2_000, 0, first)
        recorder.noteDroppedFrames(2_000, 0, second)
        recorder.noteDroppedFrames(4_000, 2, second)
        recorder.noteCommit(atMs = 2_000, tappedAtMs = 1_000, predecessor = first, successor = second)
        recorder.noteFirstFrame(2_400)
        val firstSwitch = recorder.reading()
        assertTrue(firstSwitch.frames.startsWith("2 dropped"))
        assertEquals("1400 ms", firstSwitch.visibleIn)

        recorder.noteDroppedFrames(6_000, 0, second)
        recorder.noteDroppedFrames(6_000, 0, third)
        recorder.noteDroppedFrames(7_500, 5, third)
        recorder.noteCommit(atMs = 6_000, tappedAtMs = 5_800, predecessor = second, successor = third)
        val secondSwitch = recorder.reading()
        assertTrue(
            "the second switch counts the second and third pipelines, not the first",
            secondSwitch.frames.startsWith("5 dropped"),
        )
        assertEquals("Not measured", secondSwitch.visibleIn)
    }

    @Test
    fun aRecorderWithNoCommitSaysNothingAtAll() {
        val recorder = PreparedSwitchRecorder()
        recorder.noteDroppedFrames(0, 12, 1)
        assertNull(recorder.commitAtMs)
        assertEquals(PreparedSwitchReading.unmeasured, recorder.reading())
    }

    @Test
    fun theSampleRingIsBounded() {
        val recorder = PreparedSwitchRecorder()
        // Ten times the cap of frame samples and of underrun events, plus four
        // pipelines: an eight-hour film must not grow any of them.
        repeat(PREPARED_SWITCH_SAMPLES_MAX * 10) { index ->
            recorder.noteDroppedFrames(index.toLong(), 1, 1)
            recorder.noteAudioUnderrun(index.toLong(), 1)
        }
        repeat(4) { recorder.noteDroppedFrames(0, 1, 100 + it) }
        recorder.noteCommit(atMs = 0, tappedAtMs = null, predecessor = 1, successor = 2)
        // Still answers, and answers from the retained tail rather than throwing.
        assertTrue(recorder.reading().frames.isNotEmpty())
    }

    @Test
    fun theAudioRequirementsAreAdvisoryAndSayWhy() {
        val requirements = preparedSwitchAudioRequirements(underrunCallbackAvailable = true)
        assertEquals(2, requirements.size)
        assertTrue(requirements[0].third)
        assertFalse(
            "a waveform gap detector would sit in the audible path, so it is not shipped",
            requirements[1].third,
        )
        requirements.forEach { (title, detail, _) ->
            assertTrue("$title says why", detail.length > 40)
        }
        // A requirement that is not met is still not a refusal: the reading
        // itself is unaffected by every word above.
        assertEquals(
            "0 audio underruns · ±2.0 s",
            preparedSwitchAudioUnderrunRow(preparedSwitchEventCount(emptyList(), 0)),
        )
    }

    // D1: the seam report. Only a committed switch whose window was sampled
    // end to end, with no drop and no audio underrun, is one of the twenty.

    @Test
    fun seamEvidenceIsCompleteOnlyForADecoderSampledWindow() {
        val recorder = PreparedSwitchRecorder()
        recorder.noteDecoderDropped(8_000, 3, pipeline = 1)
        recorder.noteDecoderDropped(10_000, 3, pipeline = 1, force = true)
        recorder.noteCommit(10_000, tappedAtMs = null, predecessor = 1, successor = 2)
        recorder.noteDecoderDropped(10_000, 0, pipeline = 2, force = true)
        assertFalse("the successor side is unsampled", recorder.seamEvidence(10_500).framesComplete)
        recorder.noteDecoderDropped(12_000, 0, pipeline = 2, force = true)
        val evidence = recorder.seamEvidence(12_250)
        assertTrue(evidence.framesComplete)
        assertEquals(0L, evidence.droppedFrames)
        assertTrue(evidence.audioObserved)
        recorder.noteAudioUnderrun(11_000, pipeline = 2)
        assertFalse("an underrun at the seam is not continuity", recorder.seamEvidence(12_250).audioObserved)
        assertFalse(PreparedSwitchRecorder().seamEvidence(0).framesComplete)
    }

    @Test
    fun unforcedDecoderReadingsAreSpacedSoTheBoundedListSpansTheWindow() {
        // The rendezvous polls far faster than the window needs. Unspaced, the
        // 32-sample bound would keep only the last 1.6 s of the predecessor.
        val recorder = PreparedSwitchRecorder()
        for (at in 0L until 4_000L step 50L) recorder.noteDecoderDropped(at, 0, pipeline = 1)
        recorder.noteDecoderDropped(4_000, 0, pipeline = 1, force = true)
        recorder.noteCommit(4_000, tappedAtMs = null, predecessor = 1, successor = 2)
        recorder.noteDecoderDropped(4_000, 0, pipeline = 2, force = true)
        recorder.noteDecoderDropped(6_000, 0, pipeline = 2, force = true)
        val evidence = recorder.seamEvidence(6_500)
        assertTrue("both sides span their two seconds", evidence.framesComplete)
        assertEquals(0L, evidence.droppedFrames)
    }

    @Test
    fun theSeamReportNamesEachOutcomeAndOnlyACommitCanCount() {
        val clean = PreparedSeamEvidence(framesComplete = true, droppedFrames = 0, audioObserved = true)
        val committed = preparedSeamReport("committed", clean)!!
        assertEquals("android", committed.platform)
        assertTrue(committed.framesComplete && committed.audioObserved && !committed.reopened)
        val refused = preparedSeamReport("refused", clean)!!
        assertTrue(refused.reopened && !refused.framesComplete && !refused.audioObserved)
        assertNull(refused.droppedFrames)
        assertEquals("switched_without_a_frame", preparedSeamReport("switched_without_a_frame", null)!!.outcome)
        assertNull(preparedSeamReport("teleported", clean))
    }

    /**
     * MediaSession.setPlayer follows the successor's proven frame, never the
     * commit itself: the system transport is rebound in the first-frame
     * settle, and a rollback rebinds the predecessor.
     */
    @Test
    fun mediaSessionMovesToTheSuccessorOnlyAfterItsFrame() {
        val module = generateSequence(java.io.File("").absoluteFile) { it.parentFile }
            .first { java.io.File(it, "src/main/java/tv/plurx/app/player/Controller.kt").isFile }
        val source = java.io.File(module, "src/main/java/tv/plurx/app/player/Controller.kt").readText()
        val commit = source.substringAfter("private fun commitPreparedReplacement(")
            .substringBefore("private fun settleCommitOnFirstFrame(")
        assertFalse("the commit does not hand MediaSession the successor", commit.contains("rebindMediaSessionTransport()"))
        assertTrue(commit.contains("mediaSessionRebindAwaitsFrame = true"))
        val settle = source.substringAfter("private fun settleCommitOnFirstFrame(")
            .substringBefore("private fun drainSeekDeferredBehindSwitch(")
        val proof = settle.indexOf("preparedSwitch.noteFirstFrame(")
        val rebind = settle.indexOf("rebindMediaSessionTransport()")
        assertTrue("rebound only after the frame", proof in 0 until rebind)
    }
}
