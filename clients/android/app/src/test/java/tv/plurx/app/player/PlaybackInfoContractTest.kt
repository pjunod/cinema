package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.data.DisplayTargetTexts
import tv.plurx.app.data.DvMetadataTexts
import tv.plurx.app.data.EffectiveProcessingReport
import tv.plurx.app.data.EnhancementBadge
import tv.plurx.app.data.PlaybackEnhancement
import tv.plurx.app.data.PlaybackSessionStatus
import tv.plurx.app.ui.components.DisplayTargetSubRowLabels
import tv.plurx.app.ui.components.DvInfoRow
import tv.plurx.app.ui.components.DvInfoRows
import tv.plurx.app.ui.components.DvMetadataSubRowLabels
import tv.plurx.app.ui.components.dvInfoRows

class PlaybackInfoContractTest {
    private val fixture = checkNotNull(
        javaClass.classLoader?.getResource("playback-info-fields.json"),
    ) { "tests/playback/playback-info-fields.json is not on the JVM test classpath" }
        .readText()
        .let { Json.parseToJsonElement(it).jsonObject }

    private val details = PlaybackInfoDetails(
        title = "Fixture",
        fileId = 42,
        delivery = "Transcode · nvenc · 1080p",
        position = "1:00 / 2:00",
        clientLoadedSeconds = 12.3,
        presentationAgeMs = 320,
        statusAgeMs = 85,
        frames = "2 / 4,380 frames",
        sourceFile = "fixture.mkv",
        sourceVideo = "HEVC · Main 10 · 10-bit · HDR10",
        sourceResolution = "3840×2160",
        sourceBitrate = "48 Mb/s",
        container = "MKV",
        sourceAudio = "TRUEHD · 7.1 · English",
        playingVideo = "HEVC",
        streamFrame = "1920×1080",
        playingAudio = "AAC · Stereo · English",
        dynamicRange = "HDR10",
        enhancementSummary = "FEL · DYN",
        enhancement = listOf("The full enhancement layer is in the picture.", "Dynamic metadata reaches the display."),
        dvRows = DvInfoRows(
            metadata = DvInfoRow("L1 and L2", "In the file: L1, L2\nApplied by Cinema: L1, L2\nLeft for the display: none"),
            target = DvInfoRow("1000 nits", "Reported peak: 1000 nits\nMapped for: 1000 nits\nTitle peak: 4000 nits"),
        ),
        subtitles = "English · native",
        decoder = "c2.android.hevc.decoder",
        stalls = "2 (1 supply · 1 decode)",
        observedRate = "18 Mb/s",
        streamRate = "12 Mb/s",
        startedIn = "1.2 s",
        encoder = "nvenc",
        audioSync = "250 ms",
        build = "0.3.0 (65)",
        transport = "Segmented HLS · Android Media3",
        sessionId = "session-42",
        sessionStatus = PlaybackSessionStatus(
            id = "session-42",
            encoder = "nvenc",
            tone_map_peak_nits = 1_000,
            tone_map_peak_source = "default",
            speed = 1.25,
            recent_speed = 1.20,
            out_time_ms = 90_000,
            published_end_ms = 90_000,
            fetched_end_ms = 88_000,
            playlist_shape = "VOD",
            server_ready_state = "ready",
            server_ready_anchor_ms = 60_000,
            server_ready_end_ms = 74_500,
            server_ready_seconds = 14.5,
            server_next_ready_start_ms = 78_000,
            server_next_ready_end_ms = 82_000,
            production_policy = "adaptive target",
            production_ahead_seconds = 18,
            production_target_seconds = 24,
            producer_state = "Producing",
            ahead_seconds = 12,
            ahead_bytes = 8_600_000,
            hold_reason = "runway",
            delivered_bytes = 86_000_000,
            delivered_bps = 12_300_000,
            delivered_idle_ms = 250,
            http_wait_count = 1,
            http_wait_oldest_ms = 180,
            http_wait_segment = 13,
            readrate = 1.05,
            suspended = true,
            suspend_count = 2,
            last_request = "segment 12",
            idle_seconds = 1,
        ),
        playerState = "Playing",
        control = "node-a · active",
        surfaceKind = "Blocking",
        surfaceClass = "stopped",
        surfaceSource = "owner_stopped",
        surfaceIds = "3 / —",
        surfaceHistory = "stopped · owner_stopped · raised 1000 ms → open",
    )

    @Test
    fun rowsMatchTheSharedAndroidFieldListInEveryMode() {
        PlaybackStatsMode.entries.forEach { mode ->
            // Label AND placement: the fixture's `notes` column decides
            // whether a row joins the grid or the notes strip, and comparing
            // labels alone left that column checked by nothing.
            val expected = fixture.getValue("fields").jsonArray.map { it.jsonObject }
                .filter { field ->
                    mode.storageValue in field.getValue("modes").jsonArray.map {
                        it.jsonPrimitive.content
                    } && field["available_on"]?.jsonArray?.map {
                        it.jsonPrimitive.content
                    }?.let { "android" in it } != false
                }
                .map {
                    it.getValue("label").jsonPrimitive.content to
                        (it["placement"]?.jsonPrimitive?.content ?: "grid")
                }
            val actual = playbackInfoRows(
                details,
                listOf("fixture reason"),
            ).filter { mode in it.modes && it.value != null }
                .map { it.label to it.placement }

            assertEquals(mode.label, expected, actual)
        }
    }

    @Test
    fun theMiniStripShowsTheFixtureRowsForMini() {
        // The Mini strip used to render a composite (codec · W×H · HDR ·
        // bitrate) that no mode of the contract has, and that is blank for an
        // offline file. Mini ⊂ Standard ⊂ Debug is the whole point of §7.
        val mini = playbackInfoRows(details, emptyList())
            .filter { PlaybackStatsMode.Mini in it.modes }
            .map(InfoRow::id)
        assertEquals(
            listOf("decode_resolution", "stream_frame", "player_state", "client_loaded"),
            mini,
        )
    }

    @Test
    fun theEnhancementRowLeadsWithTheSummaryThenOneMutedLinePerDetail() {
        val row = playbackInfoRows(details, emptyList()).single { it.id == "enhancement" }
        assertEquals("Enhancement", row.label)
        assertEquals("FEL · DYN", row.value)
        assertEquals(
            listOf("The full enhancement layer is in the picture.", "Dynamic metadata reaches the display."),
            row.note?.lines(),
        )
        assertEquals(PlaybackStatTone.Muted, row.tone)
        val none = playbackInfoRows(details.copy(enhancementSummary = "None active", enhancement = null), emptyList())
            .single { it.id == "enhancement" }
        assertEquals("None active", none.value)
        assertEquals(null, none.note)
        assertEquals(
            null,
            playbackInfoRows(details.copy(enhancementSummary = null, enhancement = null), emptyList())
                .single { it.id == "enhancement" }.value,
        )
    }

    @Test
    fun missingPlayerSizeDoesNotFallBackToTheSourceSize() {
        val rows = playbackInfoRows(details.copy(decodeResolution = null, sourceResolution = "3840×2160"), emptyList())
        assertEquals("Unavailable", rows.single { it.id == "decode_resolution" }.value)
        assertEquals("1920×1080", rows.single { it.id == "stream_frame" }.value)
        assertEquals("3840×2160", rows.single { it.id == "source_resolution" }.value)
    }

    @Test
    fun waitingCopyDoesNotCallServerWaitsClientBuffering() {
        assertEquals(
            PlaybackWaitPresentation(
                title = "Buffering…",
                detail = "3.3 s client loaded · 2 server HTTP waits",
            ),
            playbackWaitPresentation(3.25, 2, buffering = true),
        )
        assertEquals(
            "0.0 s client loaded · server wait state unavailable",
            playbackWaitPresentation(0.0, null, buffering = true).detail,
        )
        assertEquals(
            "Loading…",
            playbackWaitPresentation(0.0, null, buffering = false).title,
        )
    }

    @Test
    fun nativeUnitsMatchTheSharedVocabulary() {
        assertEquals("800 kb/s", formatBitrate(800_000))
        assertEquals("9.5 Mb/s", formatBitrate(9_500_000))
        assertEquals("12 Mb/s", formatBitrate(12_300_000))
    }

    @Test
    fun toneMapPeakProvenanceDoesNotCallThePolicyDefaultSourceTruth() {
        val dynamicRange = playbackInfoRows(details, emptyList()).single { it.id == "dynamic_range" }
        assertEquals("Tone-map peak 1,000 nits · policy default", dynamicRange.note)
        assertEquals(
            "Tone-map peak 4,000 nits · source MaxCLL",
            toneMapPeakSummary(
                PlaybackSessionStatus(
                    id = "source-cll",
                    tone_map_peak_nits = 4_000,
                    tone_map_peak_source = "cll",
                ),
            ),
        )
    }

    private fun contractSubRows(id: String) = fixture.getValue("fields").jsonArray.map { it.jsonObject }
        .single { it.getValue("id").jsonPrimitive.content == id }
        .getValue("sub_rows").jsonArray.map { it.jsonPrimitive.content }

    @Test
    fun theDolbyVisionRowsTakeTheirLabelsAndSubRowOrderFromTheContract() {
        assertEquals(contractSubRows("dv_metadata"), DvMetadataSubRowLabels)
        assertEquals(contractSubRows("display_target"), DisplayTargetSubRowLabels)
        val rows = playbackInfoRows(details, emptyList())
        assertEquals("Dolby Vision metadata", rows.single { it.id == "dv_metadata" }.label)
        assertEquals("Display target", rows.single { it.id == "display_target" }.label)
        val generation = "11111111-1111-4111-8111-111111111111"
        val processed = PlaybackEnhancement(listOf(EnhancementBadge.DYN), emptyList(), requiresEffectiveProcessing = true)
        val report = EffectiveProcessingReport(generation, true, false, listOf("PolynomialReshape"),
            dvMetadata = DvMetadataTexts("a", "b", "c"), displayTarget = DisplayTargetTexts("d", "e", "f"))
        val built = dvInfoRows(processed, report, generation, "hdr10")
        assertEquals(DvInfoRow("b", contractSubRows("dv_metadata").zip(listOf("a", "b", "c")).joinToString("\n") { "${it.first}: ${it.second}" }), built.metadata)
        assertEquals(DvInfoRow("e", contractSubRows("display_target").zip(listOf("d", "e", "f")).joinToString("\n") { "${it.first}: ${it.second}" }), built.target)
    }

    @Test
    fun aProcessedDeliveryShowsTheRowsOnlyFromItsCurrentReport() {
        val generation = "11111111-1111-4111-8111-111111111111"
        val processed = PlaybackEnhancement(listOf(EnhancementBadge.DYN), emptyList(), requiresEffectiveProcessing = true,
            dvMetadata = DvMetadataTexts("x", "y", "z"))
        val report = EffectiveProcessingReport(generation, true, false, listOf("PolynomialReshape"),
            dvMetadata = DvMetadataTexts("a", "b", "c"), displayTarget = DisplayTargetTexts("d", "e", "f"))
        assertEquals(DvInfoRows(), dvInfoRows(processed, null, generation, "hdr10"))
        assertEquals(DvInfoRows(), dvInfoRows(processed, report, "22222222-2222-4222-8222-222222222222", "hdr10"))
        assertEquals(DvInfoRows(), dvInfoRows(processed, report, generation, "sdr"))
        assertEquals("b", dvInfoRows(processed, report, generation, "hdr10").metadata?.value)
    }

    @Test
    fun anUnprocessedDeliveryTakesMetadataFromTheEnhancementAndHasNoDisplayTarget() {
        val plain = PlaybackEnhancement(listOf(EnhancementBadge.DYN), emptyList(),
            dvMetadata = DvMetadataTexts("a", "b", "c"))
        val report = EffectiveProcessingReport("11111111-1111-4111-8111-111111111111", true, false, listOf("PolynomialReshape"),
            displayTarget = DisplayTargetTexts("d", "e", "f"))
        val rows = dvInfoRows(plain, report, report.generation, "hdr10")
        assertEquals("b", rows.metadata?.value)
        assertEquals(null, rows.target)
        assertEquals(DvInfoRows(), dvInfoRows(null, null, null, null))
    }

    @Test
    fun anOutOfShapeTextDropsItsRow() {
        fun metadata(vararg texts: String?) = dvInfoRows(
            PlaybackEnhancement(listOf(EnhancementBadge.DYN), emptyList(), dvMetadata = DvMetadataTexts(texts[0], texts[1], texts[2])),
            null, null, null,
        ).metadata
        assertEquals("b", metadata("a", "b", "c")?.value)
        assertEquals(null, metadata("a", "b", null))
        assertEquals(null, metadata("a", "  ", "c"))
        assertEquals(null, metadata("a", "b\u0085", "c"))
        assertEquals(null, metadata("a\u007f", "b", "c"))
        assertEquals(null, metadata("a", "b", "x".repeat(513)))
        assertEquals("b", metadata("a", "b", "x".repeat(512))?.value)
    }

    @Test
    fun theWireFieldsParseLeniently() {
        val json = Json.parseToJsonElement(
            """{"active":["DYN"],"details":[],"dv_metadata":{"in_file":{"text":"a"},"applied":7,"left_for_display":{"text":"c"}}}""",
        )
        val enhancement = checkNotNull(PlaybackEnhancement.fromJson(json))
        assertEquals(DvMetadataTexts("a", null, "c"), enhancement.dvMetadata)
        assertEquals(null, PlaybackEnhancement.fromJson(Json.parseToJsonElement("""{"active":["DYN"],"dv_metadata":"x"}"""))?.dvMetadata)
    }
}
