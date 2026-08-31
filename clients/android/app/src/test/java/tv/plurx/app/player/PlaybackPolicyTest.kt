package tv.plurx.app.player

import androidx.media3.common.PlaybackException
import org.junit.Assert.assertTrue
import org.junit.Assert.assertFalse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import tv.plurx.app.data.HdrType
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.Rung

/**
 * The decisions this client still makes on its own. The subtitle contract —
 * routing, rendition ordinals, the server's automatic pick, and the session
 * body each arm posts — is `SubtitlePolicyTest`'s.
 */
class PlaybackPolicyTest {

    // ---- §5.3 force says what the menu means --------------------------------

    @Test
    fun forceSaysWhatTheMenuMeans() {
        assertEquals("auto", decisionForce(PlaybackQuality.Auto))
        assertEquals("original", decisionForce(PlaybackQuality.Original))
        // Every rung is a request to transcode; how tall is the create's job.
        assertEquals("transcode", decisionForce(PlaybackQuality.Q2160))
        assertEquals("transcode", decisionForce(PlaybackQuality.Q1080))
        assertEquals("transcode", decisionForce(PlaybackQuality.Q720))
        assertEquals("transcode", decisionForce(PlaybackQuality.Q480))
        assertEquals("transcode", decisionForce(PlaybackQuality.Q360))
    }

    // ---- §4.2 the fidelity-preserving rescue ladder ------------------------

    @Test
    fun dolbyVisionDirectPlayGetsAContainerRescueBeforeTheSdrLastResort() {
        assertEquals(
            PlaybackErrorAction.RetryAsDolbyVisionRemux,
            playbackErrorAction("direct", true, false, false),
        )
        assertEquals(
            PlaybackErrorAction.RetryAsCompatibilityTranscode,
            playbackErrorAction("remux", true, true, false),
        )
        assertEquals(
            PlaybackErrorAction.RetryAsCompatibilityTranscode,
            playbackErrorAction("direct", false, false, false),
        )
        // Once the compatibility transcode has been tried there is no lower
        // fidelity mode left, and retrying it would loop.
        assertEquals(PlaybackErrorAction.Fail, playbackErrorAction("remux", true, true, true))
        // A transcode that fails has nothing cheaper to fall back to; retrying
        // it would loop.
        assertEquals(PlaybackErrorAction.Fail, playbackErrorAction("transcode", true, false, false))
        assertEquals(PlaybackErrorAction.Fail, playbackErrorAction("transcode", false, true, true))
    }

    @Test
    fun establishedHdrRetriesTheSameDeliveryAndNeverFallsThroughToSdr() {
        assertEquals(
            PlaybackErrorAction.RetrySameHDRDelivery,
            playbackErrorAction(
                deliveryMode = "remux",
                preservesDolbyVision = false,
                remuxRescueAlreadyUsed = false,
                transcodeRescueAlreadyUsed = false,
                deliveredRange = "hdr10",
                establishedPlayback = true,
                sameHdrRetryAlreadyUsed = false,
            ),
        )
        assertEquals(
            PlaybackErrorAction.Fail,
            playbackErrorAction(
                deliveryMode = "remux",
                preservesDolbyVision = false,
                remuxRescueAlreadyUsed = false,
                transcodeRescueAlreadyUsed = false,
                deliveredRange = "hdr10",
                establishedPlayback = true,
                sameHdrRetryAlreadyUsed = true,
            ),
        )
    }

    @Test
    fun transportAndBufferFailuresNeverSpendTheCompatibilityLadder() {
        for (errorCode in listOf(1003, 2000, 2001, 2002, 2004, 3002, 3004, 4006)) {
            assertEquals(false, isCompatibilityPlaybackError(errorCode))
            assertEquals(
                PlaybackErrorAction.Fail,
                playbackErrorAction(
                    deliveryMode = "remux",
                    preservesDolbyVision = true,
                    remuxRescueAlreadyUsed = false,
                    transcodeRescueAlreadyUsed = false,
                    mediaCompatibilityFailure = isCompatibilityPlaybackError(errorCode),
                ),
            )
        }
    }

    @Test
    fun onlyContainerAndDecoderRejectionMayChangeTheEncode() {
        for (errorCode in listOf(3001, 3003, 4001, 4002, 4003, 4004, 4005)) {
            assertEquals(true, isCompatibilityPlaybackError(errorCode))
        }
    }

    // ---- §5.6 the menu is the server's ladder ------------------------------

    @Test
    fun theQualityMenuIsTheServersLadder() {
        // A 1080p source: the server drops every rung above it.
        val ladder = listOf(
            Rung(1080, total_kbps = 8_192),
            Rung(720, total_kbps = 4_096),
            Rung(480, total_kbps = 2_048),
            Rung(360, total_kbps = 896),
        )
        val options = qualityOptions(ladder)

        assertEquals(
            listOf(
                PlaybackQuality.Auto,
                PlaybackQuality.Original,
                PlaybackQuality.Q1080,
                PlaybackQuality.Q720,
                PlaybackQuality.Q480,
                PlaybackQuality.Q360,
            ),
            options.map { it.quality },
        )
        assertEquals("1080p · 8.2 Mbps", options[2].label)
        assertEquals("360p · 896 kbps", options[5].label)
    }

    @Test
    fun anOldServerWithoutALadderKeepsTheStoredEnumAsItsMenu() {
        assertEquals(PlaybackQuality.entries.map { it }, qualityOptions(emptyList()).map { it.quality })
    }

    // ---- Badges M3: what is actually on screen ------------------------------

    @Test
    fun aServerThatSaysNothingLeavesTheBadgeSourceOnly() {
        assertNull(renderedRange(null, null, null, setOf(HdrType.DOLBY_VISION)))
    }

    @Test
    fun anSdrPanelRendersSdrWhateverTheServerDelivers() {
        // The one case that needs no decoder at all: there is nowhere for the
        // extra range to go.
        assertEquals(SDR, renderedRange(DOLBY_VISION, null, null, emptySet()))
        assertEquals(SDR, renderedRange(HDR10, null, null, emptySet()))
        assertEquals(SDR, renderedRange(HLG, null, null, emptySet()))
    }

    @Test
    fun theDisplayGatesEachGradeSeparately() {
        // An HDR10-only panel is not a Dolby Vision panel; a DV panel is a PQ
        // panel and an HLG panel both.
        assertEquals(SDR, renderedRange(DOLBY_VISION, null, null, setOf(HdrType.HDR10)))
        assertEquals(DOLBY_VISION, renderedRange(DOLBY_VISION, null, null, setOf(HdrType.DOLBY_VISION)))
        assertEquals(HDR10, renderedRange(HDR10, null, null, setOf(HdrType.HDR10_PLUS)))
        assertEquals(SDR, renderedRange(HLG, null, null, setOf(HdrType.HDR10)))
        assertEquals(HLG, renderedRange(HLG, null, null, setOf(HdrType.HLG)))
    }

    @Test
    fun theDecoderWinsWhenItContradictsThePlan() {
        val dvPanel = setOf(HdrType.DOLBY_VISION, HdrType.HDR10)
        // Preserved DV that the device is in fact playing as its PQ base.
        assertEquals(HDR10, renderedRange(DOLBY_VISION, "video/hevc", COLOR_TRANSFER_ST2084, dvPanel))
        // The DV decoder engaged: a PQ transfer on a DV sample MIME is still DV.
        assertEquals(
            DOLBY_VISION,
            renderedRange(DOLBY_VISION, "video/dolby-vision", COLOR_TRANSFER_ST2084, dvPanel),
        )
        // An explicit SDR transfer contradicts an HDR plan outright.
        assertEquals(SDR, renderedRange(HDR10, "video/avc", COLOR_TRANSFER_SDR, dvPanel))
    }

    @Test
    fun silenceFromTheDecoderIsNotAContradiction() {
        val panel = setOf(HdrType.HDR10)
        // An HLS variant can publish no ColorInfo at all. Absence of evidence
        // must not become evidence of SDR — the server's answer stands.
        assertEquals(HDR10, renderedRange(HDR10, "video/hevc", null, panel))
        assertEquals(HDR10, renderedRange(HDR10, "video/hevc", COLOR_TRANSFER_UNSET, panel))
        assertEquals(HDR10, renderedRange(HDR10, null, null, panel))
    }

    private companion object {
        const val DOLBY_VISION = "dolby_vision"
        const val HDR10 = "hdr10"
        const val HLG = "hlg"
        const val SDR = "sdr"

        // Media3 `C.COLOR_TRANSFER_*` / `Format.NO_VALUE`, restated so the
        // table above reads as a table.
        const val COLOR_TRANSFER_SDR = 3
        const val COLOR_TRANSFER_ST2084 = 6
        const val COLOR_TRANSFER_UNSET = -1
    }
}

/**
 * Which class of failure Media3 reported, in the protocol's vocabulary.
 *
 * The classes are not decoration. A decoder error says this device cannot play
 * this recipe and a different rung might; a network error says nothing about
 * the recipe at all; a manifest error is about what the server produced.
 * Collapsing them to `unknown` hands the arbiter one word where it has to
 * choose between three different answers — which is exactly the ambiguity M5
 * exists to remove.
 */
class ControlErrorClassTest {

    @Test
    fun eachMedia3ErrorFamilyKeepsItsOwnAnswer() {
        assertEquals(
            ClientErrorCode.NETWORK,
            controlErrorCode(PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED),
        )
        assertEquals(ClientErrorCode.NETWORK, controlErrorCode(PlaybackException.ERROR_CODE_TIMEOUT))
        assertEquals(
            ClientErrorCode.MANIFEST,
            controlErrorCode(PlaybackException.ERROR_CODE_PARSING_MANIFEST_MALFORMED),
        )
        assertEquals(
            ClientErrorCode.DECODER,
            controlErrorCode(PlaybackException.ERROR_CODE_DECODING_FAILED),
        )
        assertEquals(
            ClientErrorCode.DECODER,
            controlErrorCode(PlaybackException.ERROR_CODE_AUDIO_TRACK_INIT_FAILED),
        )
        assertEquals(
            ClientErrorCode.DRM,
            controlErrorCode(PlaybackException.ERROR_CODE_DRM_UNSPECIFIED),
        )
    }

    /**
     * The parsing family splits, and getting it wrong tells the arbiter the
     * opposite of what this client believes. A malformed or unsupported
     * *container* is what drives the DV-remux and compatibility-transcode
     * ladder — the client is about to re-encode the file — so reporting it as
     * a manifest error would say the server produced a bad playlist.
     */
    @Test
    fun aBadContainerIsNotABadPlaylist() {
        for (code in listOf(
            PlaybackException.ERROR_CODE_PARSING_CONTAINER_MALFORMED,
            PlaybackException.ERROR_CODE_PARSING_CONTAINER_UNSUPPORTED,
        )) {
            assertEquals(ClientErrorCode.MEDIA, controlErrorCode(code))
            // The same codes this client's own ladder calls a compatibility
            // failure. If these two ever disagree, one of them is lying to
            // somebody.
            assertEquals(true, isCompatibilityPlaybackError(code))
        }
        assertEquals(
            ClientErrorCode.MANIFEST,
            controlErrorCode(PlaybackException.ERROR_CODE_PARSING_MANIFEST_UNSUPPORTED),
        )
    }

    /**
     * An unrecognised code is `unknown` rather than a nearby guess. A wrong
     * class is worse than no class: the arbiter would act on it.
     */
    @Test
    fun anUnrecognisedCodeClaimsNothing() {
        assertEquals(ClientErrorCode.UNKNOWN, controlErrorCode(PlaybackException.ERROR_CODE_UNSPECIFIED))
        assertEquals(ClientErrorCode.UNKNOWN, controlErrorCode(999_999))
    }

    /**
     * Every class must survive the wire's own bounding, or the evidence is
     * dropped silently at the last step.
     */
    @Test
    fun everyClassSurvivesTheWiresBounding() {
        for (code in listOf(
            PlaybackException.ERROR_CODE_IO_NETWORK_CONNECTION_FAILED,
            PlaybackException.ERROR_CODE_PARSING_MANIFEST_MALFORMED,
            PlaybackException.ERROR_CODE_DECODING_FAILED,
            PlaybackException.ERROR_CODE_UNSPECIFIED,
        )) {
            val observation = ClientObservation(
                decoderState = DecoderState.FAILED,
                errorCode = controlErrorCode(code),
                errorDetail = "detail",
            )
            val bounded = observation.bounded()
            assertEquals(controlErrorCode(code), bounded?.errorCode)
            assertEquals(DecoderState.FAILED, bounded?.decoderState)
        }
    }
}

/**
 * The server's seven hold reasons, in the viewer's words.
 *
 * A viewer reading `working_set` learns less than one reading a sentence, and
 * a reason this client has never heard of is a newer server rather than a
 * broken one — so an unknown reason gets the generic line instead of its wire
 * name or nothing at all.
 */
class HoldNoticeTest {

    @Test
    fun everyHoldReasonHasSomethingAViewerCanRead() {
        for (reason in listOf("demand", "time", "bytes", "global", "ahead", "working_set", "no_room")) {
            val notice = holdNotice(reason)
            // A sentence, not a token. "ahead" legitimately appears inside its
            // own sentence, so the test is that the viewer gets prose rather
            // than that a particular word is absent.
            assertTrue(notice.endsWith("."))
            assertTrue(notice.length > reason.length + 8)
            assertFalse(notice == reason)
        }
    }

    @Test
    fun anUnknownReasonFallsBackRatherThanShowingItsWireName() {
        assertEquals("Waiting for the server.", holdNotice("a reason from a newer server"))
        assertEquals("Waiting for the server.", holdNotice(null))
    }

    /**
     * Ruling D3. The fallback is the branch the whole fleet takes, so the ask
     * bound is added to every real stall on every device — and the three
     * platforms carry the same pair of numbers, so a drift is visible here
     * rather than silent.
     */
    @Test
    fun theAskBoundIsShortAndItsCapIsLonger() {
        assertEquals(1_500L, CONTROL_ASK_MS)
        assertEquals(3_000L, CONTROL_ASK_CAP_MS)
        assertTrue(CONTROL_ASK_CAP_MS > CONTROL_ASK_MS)
    }
}
