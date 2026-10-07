package tv.plurx.app.player

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class ContinuousQualityOwnershipTest {
    private val ladder = setOf(720, 1080)

    @Test fun anOpenAttachmentOnItsOwnPlayerOwnsAutoAndItsRungs() {
        assertTrue(ContinuousQualityOwnership.owns(true, false, true, PlaybackQuality.Auto) { it in ladder })
        assertTrue(ContinuousQualityOwnership.owns(true, false, true, PlaybackQuality.Q1080) { it in ladder })
        assertFalse(ContinuousQualityOwnership.owns(true, false, true, PlaybackQuality.Q480) { it in ladder })
        assertFalse(ContinuousQualityOwnership.owns(true, false, true, PlaybackQuality.Original) { it in ladder })
    }

    @Test fun aClosedAttachmentOwnsNoQualityChange() {
        // After a fallback to Direct play or the progressive remux the
        // attachment was ended but stayed attached to the same player; it kept
        // claiming every change and could only refuse each one.
        assertFalse(ContinuousQualityOwnership.owns(true, true, true, PlaybackQuality.Auto) { it in ladder })
        assertFalse(ContinuousQualityOwnership.owns(true, true, true, PlaybackQuality.Q720) { it in ladder })
    }

    @Test fun noAttachmentOrAnotherPlayerOwnsNothing() {
        assertFalse(ContinuousQualityOwnership.owns(false, false, true, PlaybackQuality.Auto) { it in ladder })
        assertFalse(ContinuousQualityOwnership.owns(true, false, false, PlaybackQuality.Q720) { it in ladder })
    }
}
