package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue
import kotlinx.serialization.json.Json
import kotlinx.serialization.encodeToString
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.ReopenReason

class AutoQualityAdapterTest {
    @Test fun eligibilityRequiresAutoOwnershipAndFreshForegroundSample() {
        val state = AutoQualityState()
        fun eligible(now: Long, presenting: Boolean = true, replacing: Boolean = false) =
            state.eligible(true, PlaybackQuality.Auto, presenting, true, replacing, now, 5_000)
        assertFalse(eligible(0))
        assertTrue(eligible(5_000))
        assertFalse(eligible(6_000, presenting = false))
        assertFalse(eligible(7_000))
        assertFalse(eligible(11_999))
        assertTrue(eligible(12_000))
        assertFalse(eligible(13_000, replacing = true))
        assertFalse(state.eligible(false, PlaybackQuality.Auto, true, true, false, 20_000, 5_000))
        assertFalse(state.eligible(true, PlaybackQuality.Original, true, true, false, 20_000, 5_000))
        assertFalse(state.eligible(true, PlaybackQuality.Q720, true, true, false, 20_000, 5_000))
    }

    @Test fun staleControllerCannotReleaseNewOwnerAndManualCannotEraseDecodeEvidence() {
        val state = AutoQualityState()
        val old = Any()
        val current = Any()
        assertTrue(state.claim(old))
        assertFalse(state.claim(current))
        state.release(current)
        assertTrue(state.inFlight)
        state.release(old)
        assertTrue(state.claim(current))
        state.release(old)
        assertTrue(state.inFlight)
        state.blockedHeights.add(720.0)
        state.decodeStepConsumed = true
        state.requestedHeight = 480
        state.viewerChangedQuality()
        assertEquals(setOf(720.0), state.blockedHeights)
        assertTrue(state.decodeStepConsumed)
        assertEquals(null, state.requestedHeight)
    }

    @Test fun typedCauseUsesFreshOwnerEvidenceNotRoutineHeldOrStationaryClock() {
        val state = AutoQualityState()
        fun cause(age: Long? = null, producer: String? = null, speed: Double? = null) =
            nativeAutoCause(state, 10_000, 5_000, age, producer, speed).kind
        assertEquals("unknown", cause(0, "held", 1.0))
        assertEquals("unknown", cause(6_000, "failed", 0.5))
        assertEquals("capacity-shortfall", cause(0, "running", 0.5))
        state.decodeAtMs = 9_000
        assertEquals("decode-failed", cause())
        state.holdUntilMs = 11_000
        assertEquals("control-stall-verdict", cause(0, "failed", 0.5))
        state.holdUntilMs = null
        state.refusalAtMs = 9_000
        state.refusalKind = "authority-refused"
        assertEquals("authority-refused", cause(0, "failed", 0.5))
        assertFalse(adaptiveGradeAllowsChange("hdr10"))
        assertFalse(adaptiveGradeAllowsChange(null))
        assertTrue(adaptiveGradeAllowsChange("sdr"))
    }

    @Test fun adaptiveWireKeepsViewerAutoDistinctFromSelectedHeight() {
        val json = Json
        val selected = QualitySelection.Auto(480)
        assertTrue(selected.isValid)
        val encoded = json.encodeToString<QualitySelection>(selected)
        assertEquals(selected, json.decodeFromString<QualitySelection>(encoded))
        assertEquals(QualitySelection.Auto(), json.decodeFromString<QualitySelection>("""{"mode":"auto"}"""))
        assertFalse(QualitySelection.Auto(719).isValid)
        assertEquals(ReopenReason.Decode, adaptiveReopenReason(AutoQualityPolicy.Decision(height = 480.0, reason = "decode")))
    }
}
