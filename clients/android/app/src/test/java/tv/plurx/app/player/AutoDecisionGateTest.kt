package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

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
}
