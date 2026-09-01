package tv.plurx.app.ui.components

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test
import tv.plurx.app.data.AudioTrack
import tv.plurx.app.data.DolbyVisionFactsDto
import tv.plurx.app.data.DynamicRange
import tv.plurx.app.data.HdrType
import tv.plurx.app.data.MediaFileDto
import tv.plurx.app.player.renderedRange

class MediaFactsTest {
    @Test
    fun portraitDimensionsUseTheShortEdgeAndCompactPlaybackFacts() {
        val file = MediaFileDto(
            id = 1,
            filename = "episode.mkv",
            width = 1_080,
            height = 1_920,
            video_codec = "hevc",
            hdr = "dolby_vision",
            hdr_format = "Dolby Vision Profile 8",
        )
        val audio = AudioTrack(
            index = 0,
            codec = "eac3",
            channels = 8,
            title = "Dolby Atmos",
            default = true,
        )

        val facts = playerMediaFacts(file, audio)

        assertEquals(listOf("1080P", "DV", "ATMOS 7.1"), facts.map { it.label })
        assertEquals(
            listOf(MediaFactKind.Resolution, MediaFactKind.DynamicRange, MediaFactKind.Audio),
            facts.map { it.kind },
        )
    }

    @Test
    fun detailFactsAddTheVideoCodecWithoutInventingVerticalResolution() {
        val file = MediaFileDto(
            id = 2,
            filename = "movie.mkv",
            width = 3_840,
            height = 1_608,
            video_codec = "hevc",
        )

        assertEquals(
            listOf("2160P", "HEVC"),
            detailMediaFacts(file).map { it.label },
        )
    }

    @Test
    fun lightweightItemResolutionUsesTheSameTechnicalBadgeVocabulary() {
        assertEquals("2160P", itemResolutionFact(2_160)?.label)
        assertEquals(MediaFactKind.Resolution, itemResolutionFact(2_160)?.kind)
    }

    // ---- MEDIA-BADGES-PLAN §2.3: the three states, case by case -------------
    //
    // The same table the web player's badge is driven by, adapted to Android's
    // chip labels ("DV"/"HDR" rather than "DV P7"/"HDR10").

    @Test
    fun aStrippedDolbyVisionRemuxSaysWhatItIsPlayingInstead() {
        // 6041: DV Profile 7, HDR10-compatible base, HDR panel. The server
        // stripped DV for this client, so the base layer is what arrives.
        val fact = rangeFact(
            file = dolbyVision("Dolby Vision · Profile 7 (HDR10-compatible)"),
            delivered = DynamicRange.HDR10,
            display = setOf(HdrType.HDR10),
        )

        assertEquals(FactState.Downgraded, fact.state)
        assertEquals("DV → HDR10", fact.chipText)
        assertEquals("Dolby Vision, playing as HDR10", fact.accessibilityLabel)
    }

    @Test
    fun aPreservedDolbyVisionCopyOnADolbyVisionPanelIsLit() {
        // 6045: DV Profile 8 preserved through the copy, panel says it can
        // show DV, nothing in the decoder contradicts it.
        val fact = rangeFact(
            file = dolbyVision("Dolby Vision · Profile 8 (HDR10-compatible)"),
            delivered = DynamicRange.DOLBY_VISION,
            display = setOf(HdrType.DOLBY_VISION, HdrType.HDR10),
        )

        assertEquals(FactState.Active, fact.state)
        assertEquals("DV", fact.chipText)
    }

    @Test
    fun theDecoderOverrulesTheServerWhenItSaysSomethingElseIsOnScreen() {
        // The panel claims Dolby Vision and the server preserved it, but the
        // decoder reports a PQ stream through a non-DV codec: whatever the plan
        // said, HDR10 is what the viewer is looking at.
        val fact = rangeFact(
            file = dolbyVision("Dolby Vision · Profile 8 (HDR10-compatible)"),
            delivered = DynamicRange.DOLBY_VISION,
            display = setOf(HdrType.DOLBY_VISION, HdrType.HDR10),
            decoderMime = "video/hevc",
            decoderColorTransfer = COLOR_TRANSFER_ST2084,
        )

        assertEquals(FactState.Downgraded, fact.state)
        assertEquals("DV → HDR10", fact.chipText)
    }

    @Test
    fun anHdrStreamOnAnSdrPanelIsSdr() {
        val fact = rangeFact(
            file = hdr10(),
            delivered = DynamicRange.HDR10,
            display = emptySet(),
        )

        assertEquals(FactState.Downgraded, fact.state)
        assertEquals("HDR → SDR", fact.chipText)
    }

    @Test
    fun aToneMappedTranscodeOfAnHdrSourceIsSdrOnAnyPanel() {
        val fact = rangeFact(
            file = hdr10(),
            delivered = DynamicRange.SDR,
            display = setOf(HdrType.HDR10),
        )

        assertEquals(FactState.Downgraded, fact.state)
        assertEquals("HDR → SDR", fact.chipText)
    }

    @Test
    fun anSdrSourceHasNoDynamicRangeChipAtAll() {
        val file = MediaFileDto(id = 7, filename = "sdr.mkv", width = 1_920, height = 1_080)

        assertNull(
            playerMediaFacts(file, null, delivered = DynamicRange.SDR, rendered = DynamicRange.SDR)
                .firstOrNull { it.kind == MediaFactKind.DynamicRange },
        )
    }

    @Test
    fun aServerThatDoesNotSendTheFieldKeepsTheOldSourceOnlyChip() {
        // No `delivered_dynamic_range` on the wire — the chip must not start
        // guessing. Same rendering as before this feature existed.
        val fact = playerMediaFacts(dolbyVision("Dolby Vision · Profile 7"), null)
            .single { it.kind == MediaFactKind.DynamicRange }

        assertEquals(FactState.Source, fact.state)
        assertEquals("DV", fact.chipText)
        assertNull(fact.activeLabel)
    }

    @Test
    fun theDetailScreenStaysSourceOnlyBecauseThereIsNoSessionToReportOn() {
        val facts = detailMediaFacts(dolbyVision("Dolby Vision · Profile 7 (HDR10-compatible)"))

        assertEquals(
            listOf(FactState.Source),
            facts.filter { it.kind == MediaFactKind.DynamicRange }.map { it.state },
        )
    }

    @Test
    fun theSourceGradeIsReadInTheServersOwnVocabulary() {
        // A DV format string names its *base* layer's compatibility, so "Dolby
        // Vision … (HLG-compatible)" is a Dolby Vision source, not an HLG one.
        assertEquals(
            DynamicRange.DOLBY_VISION,
            sourceDynamicRange(dolbyVision("Dolby Vision · Profile 7 (HLG-compatible)")),
        )
        assertEquals(DynamicRange.HDR10, sourceDynamicRange(hdr10()))
        assertEquals(
            DynamicRange.HDR10,
            sourceDynamicRange(MediaFileDto(id = 8, filename = "plus.mkv", hdr = "hdr10", hdr_format = "HDR10+")),
        )
        assertEquals(
            DynamicRange.HLG,
            sourceDynamicRange(MediaFileDto(id = 9, filename = "hlg.mkv", hdr = "hlg")),
        )
        assertNull(sourceDynamicRange(MediaFileDto(id = 10, filename = "sdr.mkv")))
    }

    private fun rangeFact(
        file: MediaFileDto,
        delivered: String,
        display: Set<Int>,
        decoderMime: String? = null,
        decoderColorTransfer: Int? = null,
    ) = playerMediaFacts(
        file = file,
        audio = null,
        delivered = delivered,
        rendered = renderedRange(delivered, decoderMime, decoderColorTransfer, display),
    ).single { it.kind == MediaFactKind.DynamicRange }

    /** The converted state: same grade, different profile, neither half dim. */
    @Test
    fun aConvertedProfileSevenTitleShowsBothHalvesLit() {
        val fact = playerMediaFacts(
            dolbyVision("Dolby Vision · Profile 7 (HDR10-compatible)", profile = 7),
            null,
            delivered = DynamicRange.DOLBY_VISION,
            rendered = DynamicRange.DOLBY_VISION,
            deliveredDolbyVisionProfile = 8,
        ).single { it.kind == MediaFactKind.DynamicRange }

        // Active, not Downgraded. The dimmed source half means "this
        // capability is unavailable"; here the base layer is copied byte for
        // byte and what reaches the device IS Dolby Vision, so dimming it
        // would say the opposite of what happened.
        assertEquals(FactState.Active, fact.state)
        assertEquals("DV", fact.chipText)
        assertEquals("Dolby Vision Profile 8", fact.activeLabel)
        assertEquals("Dolby Vision, playing as Dolby Vision Profile 8", fact.accessibilityLabel)
    }

    @Test
    fun aPreservedTitleAtItsOwnProfileGetsNoArrow() {
        // A device that genuinely decodes this profile gets the stream
        // untouched. Nothing changed, so there is nothing to announce.
        val fact = playerMediaFacts(
            dolbyVision("Dolby Vision · Profile 8 (HDR10-compatible)", profile = 8),
            null,
            delivered = DynamicRange.DOLBY_VISION,
            rendered = DynamicRange.DOLBY_VISION,
            deliveredDolbyVisionProfile = 8,
        ).single { it.kind == MediaFactKind.DynamicRange }

        assertEquals(FactState.Active, fact.state)
        assertNull(fact.activeLabel)
    }

    @Test
    fun aServerThatOmitsTheProfileGetsExactlyTodaysChip() {
        // Absent means "no answer", not "not Dolby Vision". An older server
        // sends nothing here and the badge must be what it always was — a
        // client that renders a new state off a missing optional field is a
        // client that lies on every server that predates the field.
        val fact = playerMediaFacts(
            dolbyVision("Dolby Vision · Profile 7 (HDR10-compatible)", profile = 7),
            null,
            delivered = DynamicRange.DOLBY_VISION,
            rendered = DynamicRange.DOLBY_VISION,
        ).single { it.kind == MediaFactKind.DynamicRange }

        assertEquals(FactState.Active, fact.state)
        assertNull(fact.activeLabel)
    }

    @Test
    fun aRowWithNoProfileColumnAndNoNumberInItsLabelGetsNoArrow() {
        // Scanned before the profile columns existed: the label is the bare
        // string with no number in it. Reading a number out of prose that does
        // not have one is how a badge invents a conversion that never
        // happened.
        val fact = playerMediaFacts(
            dolbyVision("Dolby Vision"),
            null,
            delivered = DynamicRange.DOLBY_VISION,
            rendered = DynamicRange.DOLBY_VISION,
            deliveredDolbyVisionProfile = 8,
        ).single { it.kind == MediaFactKind.DynamicRange }

        assertEquals(FactState.Active, fact.state)
        assertNull(fact.activeLabel)
    }

    @Test
    fun theSourceProfileIsReadFromTheColumnBeforeTheProse() {
        // The column wins, and the prose is the fallback — the same order the
        // web chip uses, so the same file cannot show an arrow on one client
        // and not the other.
        assertEquals(
            7,
            sourceDolbyVisionProfile(dolbyVision("Dolby Vision · Profile 5", profile = 7)),
        )
        assertEquals(5, sourceDolbyVisionProfile(dolbyVision("Dolby Vision · Profile 5")))
        assertNull(sourceDolbyVisionProfile(dolbyVision("Dolby Vision")))
        assertNull(sourceDolbyVisionProfile(hdr10()))
    }

    @Test
    fun aForcedTranscodeStillDimsTheSourceHalf() {
        // The converted state must not swallow the real downgrade beside it:
        // a forced 1080p rung re-encodes to SDR and reports no profile, and
        // that half MUST dim.
        val fact = playerMediaFacts(
            dolbyVision("Dolby Vision · Profile 7 (HDR10-compatible)", profile = 7),
            null,
            delivered = DynamicRange.SDR,
            rendered = DynamicRange.SDR,
        ).single { it.kind == MediaFactKind.DynamicRange }

        assertEquals(FactState.Downgraded, fact.state)
        assertEquals("SDR", fact.activeLabel)
    }

    private fun dolbyVision(format: String, profile: Int? = null) = MediaFileDto(
        id = 6_041,
        filename = "dv.mkv",
        width = 3_840,
        height = 2_160,
        video_codec = "hevc",
        hdr = "dolby_vision",
        hdr_format = format,
        dolby_vision = profile?.let { DolbyVisionFactsDto(profile = it) },
    )

    private fun hdr10() = MediaFileDto(
        id = 6_100,
        filename = "hdr10.mkv",
        width = 3_840,
        height = 2_160,
        video_codec = "hevc",
        hdr = "hdr10",
        hdr_format = "HDR10",
    )

    private companion object {
        /** `C.COLOR_TRANSFER_ST2084`, restated so the table reads as a table. */
        const val COLOR_TRANSFER_ST2084 = 6
    }
}
