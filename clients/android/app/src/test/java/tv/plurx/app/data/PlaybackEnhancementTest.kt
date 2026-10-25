package tv.plurx.app.data

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.player.ControlAction

class PlaybackEnhancementTest {
    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

    private fun start(extra: String): HlsStart = json.decodeFromString(
        HlsStart.serializer(),
        """{"session_id":"s1","playlist_url":"/hls/s1/master.m3u8"$extra}""",
    )

    @Test
    fun theCreateResponseCarriesTheServerEnhancement() {
        val hls = start(
            ""","enhancement":{"active":["FEL","RPU","DYN"],"details":[
                {"badge":"FEL","active":true,"text":"Full enhancement layer residual is in the picture."},
                {"badge":"RPU","active":true,"text":"RPU reshaping applied by the server."},
                {"badge":"DYN","active":true,"text":"Dynamic metadata reaches the display."}],
               "requires_effective_processing":true}""",
        )
        val enhancement = checkNotNull(PlaybackEnhancement.fromJson(hls.enhancement))
        assertEquals(listOf(EnhancementBadge.FEL, EnhancementBadge.RPU, EnhancementBadge.DYN), enhancement.active)
        assertEquals(
            listOf(
                "Full enhancement layer residual is in the picture.",
                "RPU reshaping applied by the server.",
                "Dynamic metadata reaches the display.",
            ),
            enhancement.details.map { it.text },
        )
        assertTrue(enhancement.details.all { it.active })
        assertTrue(enhancement.requiresEffectiveProcessing)
    }

    @Test
    fun anAbsentOrMalformedEnhancementIsNoEnhancement() {
        assertNull(start("").enhancement)
        assertNull(PlaybackEnhancement.fromJson(start("").enhancement))
        assertNull(PlaybackEnhancement.fromJson(start(""","enhancement":null""").enhancement))
        assertNull(PlaybackEnhancement.fromJson(start(""","enhancement":"FEL"""").enhancement))
        assertNull(PlaybackEnhancement.fromJson(start(""","enhancement":{"active":[],"details":[]}""").enhancement))
    }

    @Test
    fun anOmittedRequirementMeansTheBadgesNeedNoReport() {
        val enhancement = checkNotNull(
            PlaybackEnhancement.fromJson(
                start(""","enhancement":{"active":["DYN"],"details":[{"badge":"DYN","active":true,"text":"Dynamic metadata."}]}""")
                    .enhancement,
            ),
        )
        assertFalse(enhancement.requiresEffectiveProcessing)
        assertEquals(enhancement, enhancement.shown(null, null, "dolby_vision"))
    }

    @Test
    fun anUnknownBadgeIsIgnoredWithoutLosingTheRest() {
        val enhancement = checkNotNull(
            PlaybackEnhancement.fromJson(
                start(
                    ""","enhancement":{"active":["XEL","DYN",7,"DYN"],"details":[
                        {"badge":"XEL","active":true,"text":"A newer enhancement."},
                        {"badge":"RPU","active":false,"text":"RPU reshaping is left to the display."},
                        {"badge":"FEL","active":false,"text":"  "}]}""",
                ).enhancement,
            ),
        )
        assertEquals(listOf(EnhancementBadge.DYN), enhancement.active)
        // The sentence is still the server's explanation; only the badge is unknown.
        assertEquals(listOf(null, EnhancementBadge.RPU), enhancement.details.map { it.badge })
        assertEquals(
            listOf("A newer enhancement.", "RPU reshaping is left to the display."),
            enhancement.details.map { it.text },
        )
        assertEquals(listOf(true, false), enhancement.details.map { it.active })
    }

    @Test
    fun displayTargetOperationObjectRetainsTheCurrentReport() {
        val generation = "11111111-1111-4111-8111-111111111111"
        val wire = """{"generation":"$generation","hdr10_enhanced":true,"fel_contributed":false,"applied_operations":["RpuColorConversion","TargetMapping",{"DisplayTargetMapping":{"nits":1600}}]}"""
        val report = EffectiveProcessingReport.fromJson(json.parseToJsonElement(wire))
        assertEquals(true, report?.matches(generation, "hdr10"))
        assertEquals(true, report?.appliedOperations?.contains("DisplayTargetMapping:1600"))
    }

    @Test
    fun sdrReceiptRequiresItsGradeAndExportOperations() {
        val generation = "11111111-1111-4111-8111-111111111111"
        val wire = """{"generation":"$generation","hdr10_enhanced":false,"sdr_processed":true,"fel_contributed":false,"applied_operations":["RpuColorConversion","TargetMapping","Bt709Gamma24Conversion",{"DisplayTargetMapping":{"nits":100}}]}"""
        val report = EffectiveProcessingReport.fromJson(json.parseToJsonElement(wire))
        assertEquals(true, report?.matches(generation, "sdr"))
        assertEquals(false, report?.matches(generation, "hdr10"))
        assertEquals("SDR-E", report?.mark)
        assertEquals(true, report?.detail?.contains("L1 only"))
        val trimmed = EffectiveProcessingReport.fromJson(json.parseToJsonElement(
            wire.replace("\"sdr_processed\":true,", "\"sdr_processed\":true,\"creative_trims_applied\":true,")
                .replace("\"TargetMapping\",", "\"TargetMapping\",\"CreativeTrimApplication\",")))
        assertEquals(true, trimmed?.matches(generation, "sdr"))
        assertEquals("SDR-E", trimmed?.mark)
        assertEquals(true, trimmed?.detail?.contains("BT.709 gamma 2.4, creative trims applied"))
        assertEquals(false, trimmed?.detail?.contains("L1 only"))
        val partial = EffectiveProcessingReport.fromJson(json.parseToJsonElement(
            wire.replace("\"sdr_processed\":true,", "\"sdr_processed\":true,\"creative_trims_partial\":true,")))
        assertEquals(true, partial?.matches(generation, "sdr"))
        assertEquals(true, partial?.detail?.contains("BT.709 gamma 2.4, creative trims on some shots"))
    }

    @Test
    fun aProcessedDeliveryIsShownOnlyWithACurrentMatchingReport() {
        val generation = "11111111-1111-4111-8111-111111111111"
        val enhancement = PlaybackEnhancement(
            listOf(EnhancementBadge.FEL, EnhancementBadge.RPU),
            listOf(EnhancementDetail(EnhancementBadge.FEL, true, "FEL in the picture.")),
            requiresEffectiveProcessing = true,
        )
        val report = EffectiveProcessingReport(generation, true, true, listOf("PolynomialReshape"))
        assertNull(enhancement.shown(null, generation, "hdr10"))
        assertNull(enhancement.shown(report, "22222222-2222-4222-8222-222222222222", "hdr10"))
        assertNull(enhancement.shown(report, generation, "dolby_vision"))
        assertEquals(enhancement, enhancement.shown(report, generation, "hdr10"))
    }

    private fun prepare(extra: String): ControlAction = json.decodeFromString(
        ControlAction.serializer(),
        """{"type":"prepare","action_id":"$SUCCESSOR_ACTION","session_id":"$SUCCESSOR_SESSION",
            "playlist_url":"/api/v1/hls/$SUCCESSOR_SESSION/index.m3u8","media_origin_ms":0,
            "effective_selection":{"quality_auto":true,"height":1080,"audio_track":null,
              "subtitle_burn":null,"audio_offset_ms":0,"codec":"server_selected","dynamic_range":"hdr10"}$extra}""",
    )

    @Test
    fun aPreparedSuccessorCarriesItsOwnEnhancementParsedLikeTheCreateResponse() {
        val action = prepare(
            ""","enhancement":{"active":["FEL","RPU"],"requires_effective_processing":true,"details":[
                {"badge":"FEL","active":true,"text":"FEL reconstructed."},
                {"badge":"RPU","active":true,"text":"RPU applied by the server."}]}""",
        )
        assertTrue(action.preparedPayloadIsValid)
        val enhancement = checkNotNull(PlaybackEnhancement.fromJson(action.enhancement))
        assertEquals(listOf(EnhancementBadge.FEL, EnhancementBadge.RPU), enhancement.active)
        assertTrue(enhancement.requiresEffectiveProcessing)
        // The report gate is unchanged: no current report, nothing shown.
        assertNull(enhancement.shown(null, null, "hdr10"))
    }

    @Test
    fun aPreparedSuccessorWithoutAnEnhancementFallsBackToNone() {
        assertNull(PlaybackEnhancement.fromJson(prepare("").enhancement))
        val malformed = prepare(""","enhancement":"FEL"""")
        assertTrue(malformed.preparedPayloadIsValid)
        assertNull(PlaybackEnhancement.fromJson(malformed.enhancement))
    }

    private companion object {
        const val SUCCESSOR_ACTION = "33333333-3333-4333-8333-333333333333"
        const val SUCCESSOR_SESSION = "44444444-4444-4444-8444-444444444444"
    }
}
