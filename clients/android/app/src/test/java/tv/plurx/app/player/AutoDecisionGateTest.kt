package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.PlaybackQuality

class AutoDecisionGateTest {
    private val open = AutoTickState(protocol = true, automatic = true, playing = true, seeking = false,
        changePending = false, controlClosed = false, producerState = "running")

    @Test fun aHeldProducerNeverBlocksAnAutoDecision() {
        // Android Auto returned whenever the server producer was held, which is
        // most of the time a producer runs ahead of a 12-second client budget,
        // so continuous playback never downgraded or upgraded.
        assertNull(autoDecisionGate(open.copy(producerState = "held")))
        assertNull(autoDecisionGate(open.copy(producerState = null)))
    }

    @Test fun realBlockersStillGateInPriorityOrder() {
        assertEquals("protocol", autoDecisionGate(open.copy(protocol = false, automatic = false)))
        assertEquals("manual", autoDecisionGate(open.copy(automatic = false)))
        assertEquals("not_playing", autoDecisionGate(open.copy(playing = false)))
        assertEquals("seeking", autoDecisionGate(open.copy(seeking = true)))
        assertEquals("change_pending", autoDecisionGate(open.copy(changePending = true)))
        assertEquals("control_closed", autoDecisionGate(open.copy(controlClosed = true)))
    }

    @Test fun onlyAnUnsettledDirectedChangeHoldsAuto() {
        // The controller keeps its last directed change after it settles and
        // never clears the reference. Testing non-nullness held Auto at
        // change_pending for the rest of the title after the viewer's first
        // quality choice, including the choice of Auto itself.
        assertFalse(directedChangeOutstanding(null))
        val tapped = DirectedChange(epoch = 1, quality = PlaybackQuality.Auto)
        assertTrue(directedChangeOutstanding(tapped))
        tapped.committed()
        assertFalse(directedChangeOutstanding(tapped))
        assertNull(autoDecisionGate(open.copy(changePending = directedChangeOutstanding(tapped))))

        val replaced = DirectedChange(epoch = 1, quality = PlaybackQuality.Auto)
        replaced.superseded()
        assertFalse(directedChangeOutstanding(replaced))

        val retained = DirectedChange(epoch = 1, quality = PlaybackQuality.Auto)
        assertTrue(retained.settleFailureOnce(1, incumbentHealthy = true, retain = {}, reopen = {}))
        assertFalse(directedChangeOutstanding(retained))
    }

    @Test fun autoOwnsQualityOnlyWhileAutoExecutes() {
        // A retained failed Auto choice leaves the wish on Auto while the
        // incumbent Manual rung keeps playing: Auto must not drive it.
        val fromManual = PlaybackIntent(initialQuality = PlaybackQuality.Q720)
        assertFalse(autoExecuting(fromManual))
        val toAuto = fromManual.beginQualityChange(PlaybackQuality.Auto, tappedAtMs = 1)
        assertTrue(fromManual.retainFailedQuality(toAuto, QualitySelection.Manual(720)))
        assertEquals(PlaybackQuality.Auto, fromManual.desiredQuality)
        assertFalse(autoExecuting(fromManual))
        assertEquals("manual", autoDecisionGate(open.copy(automatic = autoExecuting(fromManual))))

        // A retained failed Manual choice under Auto keeps Auto executing.
        val fromAuto = PlaybackIntent(initialQuality = PlaybackQuality.Auto)
        val toManual = fromAuto.beginQualityChange(PlaybackQuality.Q480, tappedAtMs = 1)
        assertTrue(fromAuto.retainFailedQuality(toManual, QualitySelection.Auto))
        assertTrue(autoExecuting(fromAuto))
    }
}
