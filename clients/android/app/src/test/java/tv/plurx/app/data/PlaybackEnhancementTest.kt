package tv.plurx.app.data

import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PlaybackEnhancementTest {
    private val json = Json { ignoreUnknownKeys = true }

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
}
