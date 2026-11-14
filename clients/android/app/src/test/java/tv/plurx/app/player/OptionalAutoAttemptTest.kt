package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class OptionalAutoAttemptTest {
    private class Fixture {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val player = Any()
        val executing = QualitySelection.AutoCandidate(2160, "frozen-processed-hdr10")
        val attempt: OptionalAutoAttempt
        init {
            intent.requestAutomaticCandidate("copy", 2160)
            assertTrue(intent.adoptCommittedAutomaticCandidate(executing.candidateId, 2160, true))
            attempt = OptionalAutoAttempt.capture(intent, player, "owned-session", 7,
                executing.candidateId, executing, "ordinary-hdr10-1080", 1080)!!
        }
        fun restore(player: Any = this.player, session: String = "owned-session", epoch: Long = 7,
            active: String = executing.candidateId): Boolean =
            attempt.restoreIfCurrent(intent, player, session, epoch, active)
        fun wireQuality(): QualitySelection.AutoCandidate =
            QualitySelection.AutoCandidate(intent.automaticCandidateHeight, intent.automaticCandidateId!!)
        fun selection(quality: QualitySelection) = ClientSelection(quality, 0,
            SubtitleSelection(SubtitleMode.OFF), 0, CodecPolicy.AUTO, DynamicRangePolicy.AUTO)
    }

    @Test fun failedOptionalTargetRestoresExecutingFrozenRecipeOnTheNextControlEnvelope() {
        val f = Fixture()
        val standing = f.selection(f.executing)
        val failed = f.intent.mediaIntent(f.selection(f.wireQuality()))
        val seek = f.intent.beginSeek(1_500_000L, 1_480_000L)
        assertTrue(f.restore())
        val restored = f.intent.mediaIntent(f.selection(f.wireQuality()))
        assertEquals(standing.quality, restored.selection.quality)
        assertEquals(2160, f.intent.automaticCandidateHeight)
        assertTrue(restored.recipe_revision > failed.recipe_revision)
        assertEquals(failed.lifetime_id, restored.lifetime_id)
        assertSame(seek, f.intent.pendingSeek)
        assertEquals(PlaybackQuality.Auto, f.intent.desiredQuality)
        assertFalse(f.restore()) // settlement consumed this revision
    }

    @Test fun failedOptionalTargetCannotOverwriteANewerRequestEvenForTheSameCandidate() {
        for (id in listOf("ordinary-hdr10-1080", "new-target")) {
            val f = Fixture()
            f.intent.requestAutomaticCandidate(id, 720)
            assertFalse(f.restore())
            assertEquals(QualitySelection.AutoCandidate(720, id), f.wireQuality())
        }
    }

    @Test fun failedOptionalTargetCannotOverwriteManualOrPendingViewerQuality() {
        val manual = Fixture()
        manual.intent.adoptQuality(PlaybackQuality.Q720)
        assertFalse(manual.restore())
        assertEquals(PlaybackQuality.Q720, manual.intent.desiredQuality)
        val pending = Fixture()
        val change = pending.intent.beginQualityChange(PlaybackQuality.Auto, 50L)
        assertFalse(pending.restore())
        assertSame(change, pending.intent.pendingQualityChange)
    }

    @Test fun failedOptionalTargetCannotOverwriteARetainedRecipeOrAnAutoRoundTrip() {
        val retained = Fixture()
        val change = retained.intent.beginQualityChange(PlaybackQuality.Auto, 50L)
        val quality = QualitySelection.Manual(720)
        assertTrue(retained.intent.retainFailedQuality(change, quality))
        assertFalse(retained.restore())
        assertEquals(quality, retained.intent.retainedControlQuality)
        val roundTrip = Fixture()
        roundTrip.intent.adoptQuality(PlaybackQuality.Q720)
        roundTrip.intent.adoptQuality(PlaybackQuality.Auto)
        assertFalse(roundTrip.restore())
    }

    @Test fun failedOptionalTargetRequiresTheSameExecutingPlayerSessionMutationAndCandidate() {
        for (change in 0..3) {
            val f = Fixture()
            assertFalse(f.restore(player = if (change == 0) Any() else f.player,
                session = if (change == 1) "replacement-session" else "owned-session",
                epoch = if (change == 2) 8 else 7,
                active = if (change == 3) "replacement-candidate" else f.executing.candidateId))
            assertEquals(QualitySelection.AutoCandidate(1080, "ordinary-hdr10-1080"), f.wireQuality())
        }
    }

    @Test fun acceptedPreparedOwnershipCannotBeRolledBackByItsOldOptionalFailure() {
        val f = Fixture()
        assertTrue(f.intent.adoptCommittedAutomaticCandidate("new-committed", 1080, true))
        assertFalse(f.restore(active = "new-committed"))
        assertEquals(QualitySelection.AutoCandidate(1080, "new-committed"), f.wireQuality())
    }
    @Test fun missingOrMismatchedExecutingReceiptDoesNotPublishAnOptionalTarget() {
        val known = QualitySelection.AutoCandidate(2160, "processed")
        for ((active, standing) in listOf(
            "processed" to QualitySelection.Auto,
            "processed" to QualitySelection.AutoCandidate(1080, "different"),
            null to known,
            "processed" to QualitySelection.Manual(720),
        )) {
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            intent.requestAutomaticCandidate("processed", 2160)
            val owner = intent.automaticCandidateOwner()
            assertEquals(null, OptionalAutoAttempt.capture(intent, Any(), "session", 1,
                active, standing, "target", 1080))
            assertEquals(owner, intent.automaticCandidateOwner())
            assertEquals("processed", intent.automaticCandidateId)
            assertEquals(2160, intent.automaticCandidateHeight)
        }
    }

    @Test fun genuinelyUnnamedAutoExecutionCanRestoreItsUnnamedStandingChoice() {
        val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val player = Any()
        val attempt = OptionalAutoAttempt.capture(intent, player, "session", 1,
            null, QualitySelection.Auto, "target", 1080)!!
        assertEquals("target", intent.automaticCandidateId)
        assertTrue(attempt.restoreIfCurrent(intent, player, "session", 1, null))
        assertEquals(null, intent.automaticCandidateId)
        assertEquals(null, intent.automaticCandidateHeight)
    }

    @Test fun optionalCaptureCannotPublishOverManualPendingOrRetainedIntent() {
        for (mode in 0..2) {
            val intent = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
            intent.requestAutomaticCandidate("processed", 2160)
            when (mode) {
                0 -> intent.adoptQuality(PlaybackQuality.Q720)
                1 -> intent.beginQualityChange(PlaybackQuality.Auto, 50L)
                else -> {
                    val change = intent.beginQualityChange(PlaybackQuality.Auto, 50L)
                    intent.retainFailedQuality(change, QualitySelection.AutoCandidate(2160, "processed"))
                }
            }
            val owner = intent.automaticCandidateOwner()
            assertEquals(null, OptionalAutoAttempt.capture(intent, Any(), "session", 1, "processed",
                QualitySelection.AutoCandidate(2160, "processed"), "target", 1080))
            assertEquals(owner, intent.automaticCandidateOwner())
            assertEquals("processed", intent.automaticCandidateId)
        }
    }

}
