package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private const val ACTION_ID = "33333333-3333-4333-8333-333333333333"
private const val SUCCESSOR = "44444444-4444-4444-8444-444444444444"

private val json = Json { ignoreUnknownKeys = true; explicitNulls = false }

private fun selection(
    height: Long = 1_080,
    codec: String = "server_selected",
    dynamicRange: String? = "sdr",
    audioOffsetMs: Long = 0,
) = EffectiveSelection(
    qualityAuto = true,
    height = height,
    audioTrack = null,
    subtitleBurn = null,
    audioOffsetMs = audioOffsetMs,
    codec = codec,
    dynamicRange = dynamicRange,
)

private fun prepare(
    actionId: String = ACTION_ID,
    sessionId: String = SUCCESSOR,
    playlistUrl: String = "/api/v1/hls/$SUCCESSOR/index.m3u8",
    mediaOriginMs: Long = 0,
    effectiveSelection: EffectiveSelection? = selection(),
) = ControlAction(
    type = PlaybackControl.PREPARE_ACTION_TYPE,
    actionId = actionId,
    sessionId = sessionId,
    playlistUrl = playlistUrl,
    mediaOriginMs = mediaOriginMs,
    effectiveSelection = effectiveSelection,
)

/**
 * The inbound action, exactly as the server serialises it.
 *
 * The server asserts the `type` tag against a serialized body and nothing in
 * the tree asserts the four fields a client actually consumes — grep for
 * `["action"]["playlist_url"]` and there is nothing. So this fixture is the
 * assertion that the names on the wire are the names this client reads, and it
 * is written as literal JSON rather than built from the Kotlin type, because a
 * fixture built from the type cannot disagree with the type.
 */
class PreparedActionDecodingTest {
    @Test
    fun `the server's prepare action decodes key by key`() {
        val decoded = json.decodeFromString(
            ControlAction.serializer(),
            """
            {"type":"prepare",
             "action_id":"$ACTION_ID",
             "session_id":"$SUCCESSOR",
             "playlist_url":"/api/v1/hls/$SUCCESSOR/index.m3u8",
             "media_origin_ms":0,
             "effective_selection":{"quality_auto":true,"height":1080,
               "audio_track":null,"subtitle_burn":null,"audio_offset_ms":0,
               "codec":"server_selected","dynamic_range":"sdr"}}
            """.trimIndent(),
        )
        assertEquals("prepare", decoded.type)
        assertEquals(ACTION_ID, decoded.actionId)
        assertEquals(SUCCESSOR, decoded.sessionId)
        assertEquals("/api/v1/hls/$SUCCESSOR/index.m3u8", decoded.playlistUrl)
        assertEquals(0L, decoded.mediaOriginMs)
        val effective = assertNotNull(decoded.effectiveSelection)
        assertTrue(effective.qualityAuto)
        assertEquals(1_080L, effective.height)
        assertNull(effective.audioTrack)
        assertNull(effective.subtitleBurn)
        assertEquals(0L, effective.audioOffsetMs)
        assertEquals("server_selected", effective.codec)
        assertEquals("sdr", effective.dynamicRange)
        assertTrue(decoded.preparedPayloadIsValid)
    }

    @Test
    fun `the declared name and the wire tag are deliberately different`() {
        // Declaring `"prepare"` is never matched by the server's literal
        // `accepts()` comparison and is never offered anything; switching on
        // `"prepare_replacement"` as an action type never fires. Both failures
        // are completely silent, which is why they are pinned here.
        assertTrue(PlaybackControl.SUPPORTED_ACTIONS.contains("prepare_replacement"))
        assertFalse(PlaybackControl.SUPPORTED_ACTIONS.contains("prepare"))
        assertEquals("prepare", PlaybackControl.PREPARE_ACTION_TYPE)
        assertEquals("prepare_replacement", PlaybackControl.PREPARE_REPLACEMENT_ACTION)
    }

    @Test
    fun `a later server's extra keys are tolerated`() {
        // `ControlAction` carries no `deny_unknown_fields` on the server side,
        // so a newer server may add keys and this parser must survive them.
        val decoded = json.decodeFromString(
            ControlAction.serializer(),
            """
            {"type":"prepare","action_id":"$ACTION_ID","session_id":"$SUCCESSOR",
             "playlist_url":"/api/v1/hls/$SUCCESSOR/index.m3u8",
             "media_origin_ms":5,"successor_rung":"new-in-v2",
             "effective_selection":{"quality_auto":false,"height":720,
               "audio_offset_ms":0,"codec":"source"}}
            """.trimIndent(),
        )
        assertTrue(decoded.preparedPayloadIsValid)
        assertNull(assertNotNull(decoded.effectiveSelection).dynamicRange)
    }
}

class PreparedPayloadValidationTest {
    @Test
    fun `a node-relative playlist naming this successor is accepted`() {
        assertTrue(prepare().preparedPayloadIsValid)
        assertTrue(
            prepare(playlistUrl = "/api/v1/hls/$SUCCESSOR/master.m3u8").preparedPayloadIsValid,
        )
        // Query and fragment are allowed and ignored, as the server allows and
        // ignores them.
        assertTrue(
            prepare(playlistUrl = "/api/v1/hls/$SUCCESSOR/index.m3u8?x=1#y")
                .preparedPayloadIsValid,
        )
    }

    @Test
    fun `an absolute playlist url is refused`() {
        // Mirrors the server's own
        // `a_relayed_preparation_cannot_point_a_client_anywhere`. A client that
        // followed one would prime a pipeline against an origin nobody
        // authorised, and the refusal must happen before any request.
        assertFalse(
            prepare(playlistUrl = "https://elsewhere.example/api/v1/hls/$SUCCESSOR/index.m3u8")
                .preparedPayloadIsValid,
        )
        assertFalse(
            prepare(playlistUrl = "//elsewhere.example/api/v1/hls/$SUCCESSOR/index.m3u8")
                .preparedPayloadIsValid,
        )
    }

    @Test
    fun `a playlist naming another session is refused`() {
        // The server checks the URL against the successor's own id. A URL
        // naming a different session is not this successor's stream.
        assertFalse(
            prepare(playlistUrl = "/api/v1/hls/$ACTION_ID/index.m3u8").preparedPayloadIsValid,
        )
    }

    @Test
    fun `a traversal or a different route is refused`() {
        assertFalse(prepare(playlistUrl = "/api/v1/hls/$SUCCESSOR/../../etc").preparedPayloadIsValid)
        assertFalse(prepare(playlistUrl = "/api/v1/hls/$SUCCESSOR/control").preparedPayloadIsValid)
    }

    @Test
    fun `an over-long playlist url is refused`() {
        val padded = "/api/v1/hls/$SUCCESSOR/index.m3u8?" + "a".repeat(600)
        assertTrue(padded.length > PlaybackControl.MAX_PLAYLIST_URL_LEN)
        assertFalse(prepare(playlistUrl = padded).preparedPayloadIsValid)
    }

    @Test
    fun `identifiers that are not uuids are refused`() {
        assertFalse(prepare(actionId = "not-a-uuid").preparedPayloadIsValid)
        assertFalse(prepare(sessionId = "not-a-uuid").preparedPayloadIsValid)
    }

    @Test
    fun `a missing field is refused rather than defaulted`() {
        assertFalse(ControlAction(type = "prepare").preparedPayloadIsValid)
        assertFalse(prepare(effectiveSelection = null).preparedPayloadIsValid)
    }

    @Test
    fun `a negative or absurd media origin is refused`() {
        assertFalse(prepare(mediaOriginMs = -1).preparedPayloadIsValid)
        assertFalse(
            prepare(mediaOriginMs = PlaybackControl.MAX_MEDIA_MILLIS + 1).preparedPayloadIsValid,
        )
        assertTrue(prepare(mediaOriginMs = PlaybackControl.MAX_MEDIA_MILLIS).preparedPayloadIsValid)
    }

    @Test
    fun `an effective selection out of range is refused`() {
        // `codec` is a delivery method, not a codec name: `source` or
        // `server_selected`, and nothing else.
        assertFalse(prepare(effectiveSelection = selection(codec = "hevc")).preparedPayloadIsValid)
        assertTrue(prepare(effectiveSelection = selection(codec = "source")).preparedPayloadIsValid)
        assertFalse(
            prepare(effectiveSelection = selection(dynamicRange = "hdr11")).preparedPayloadIsValid,
        )
        assertFalse(prepare(effectiveSelection = selection(height = 4_320)).preparedPayloadIsValid)
        assertFalse(
            prepare(effectiveSelection = selection(audioOffsetMs = 20_000)).preparedPayloadIsValid,
        )
    }
}

class PreparedReplacementLedgerTest {
    @Test
    fun `a repeated action id is one preparation`() {
        // The server replays a staging byte-identically on every exchange until
        // it settles, so this is what most exchanges carrying a `prepare` look
        // like. Building again would put a third pipeline on the device.
        val ledger = PreparedReplacementLedger()
        assertTrue(ledger.offer(prepare()) is PreparationOffer.Start)
        repeat(3) { assertTrue(ledger.offer(prepare()) is PreparationOffer.Same) }
        assertEquals(ACTION_ID, ledger.actionId)
    }

    @Test
    fun `a new action id aborts the one it supersedes`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        val second = "55555555-5555-4555-8555-555555555555"
        val started = ledger.offer(prepare(actionId = second)) as? PreparationOffer.Start
            ?: error("a different action_id is a different preparation")
        val owed = assertNotNull(started.supersedes)
        assertEquals(ACTION_ID, owed.actionId)
        assertEquals(AcknowledgementState.ABORTED, owed.state)
        assertEquals(second, ledger.actionId)
    }

    @Test
    fun `an invalid payload starts nothing`() {
        val ledger = PreparedReplacementLedger()
        assertTrue(ledger.offer(prepare(playlistUrl = "https://elsewhere.example/x")) is PreparationOffer.Refuse)
        assertNull(ledger.actionId)
        assertFalse(ledger.isLive)
    }

    @Test
    fun `the ladder climbs once and is monotonic`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        val metadata = assertNotNull(ledger.metadataReady())
        assertEquals(AcknowledgementState.METADATA_READY, metadata.state)
        // A second `metadata_ready` is the same rung. The server records
        // progress monotonically and ignores a step backwards, so sending one
        // costs an exchange and buys nothing.
        assertNull(ledger.metadataReady())
        val buffer = assertNotNull(ledger.bufferReady(90_000))
        assertEquals(AcknowledgementState.BUFFER_READY, buffer.state)
        assertEquals(90_000L, buffer.bufferedThroughMs)
        assertNull(ledger.bufferReady(95_000))
    }

    @Test
    fun `buffer ready carries its runway and committed carries its frame`() {
        // The server refuses both pairings, and it refuses the whole exchange
        // they ride on — so they are refused here instead.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        assertNotNull(ledger.bufferReady(12_345)).let {
            assertEquals(12_345L, it.bufferedThroughMs)
            assertTrue(it.isValid)
        }
        val committed = assertNotNull(ledger.committed(1_788_000_000_000))
        assertEquals(1_788_000_000_000L, committed.firstFrameUnixMs)
        assertTrue(committed.isValid)
        assertFalse(
            ActionAcknowledgement(ACTION_ID, AcknowledgementState.BUFFER_READY).isValid,
        )
        assertFalse(
            ActionAcknowledgement(ACTION_ID, AcknowledgementState.COMMITTED).isValid,
        )
    }

    @Test
    fun `a terminal state absorbs, so nothing is settled twice`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        assertEquals(AcknowledgementState.FAILED, assertNotNull(ledger.failed()).state)
        assertNull(ledger.failed())
        assertNull(ledger.aborted())
        assertNull(ledger.metadataReady())
        assertNull(ledger.committed(1))
        assertFalse(ledger.isLive)
    }

    @Test
    fun `an abandoned preparation always owes a terminal state`() {
        // Leaving it to the server's 330 s deadline holds this session's one
        // preparation slot for the rest of the session, so the session gets
        // exactly one preparation, ever, good or not.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.metadataReady()
        val aborted = assertNotNull(ledger.aborted())
        assertEquals(AcknowledgementState.ABORTED, aborted.state)
        assertEquals(ACTION_ID, aborted.actionId)
    }

    @Test
    fun `a commit that never rendered is refused rather than faked`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        assertNull(ledger.committed(0))
        assertNull(ledger.committed(-1))
        assertTrue(ledger.isLive)
    }
}

class PreparedSwitchPointTest {
    @Test
    fun `a successor is switchable only once it holds real runway ahead`() {
        // Both numbers are film time, which is the whole reason
        // `media_origin_ms` is on the action: the comparison is a subtraction
        // rather than a timeline conversion.
        assertFalse(successorIsBuffered(60_000, 60_000))
        assertFalse(successorIsBuffered(62_999, 60_000))
        assertTrue(successorIsBuffered(63_000, 60_000))
        // A successor buffered behind the playhead is not nearly ready.
        assertFalse(successorIsBuffered(10_000, 60_000))
    }
}

class PreparedReplacementRequirementsTest {
    @Test
    fun `a phone meets the device conditions and a television does not`() {
        val phone = preparedReplacementRequirements(
            isTelevision = false,
            tunnelingEnabled = false,
            sessionIsLive = true,
            observedDownloadBps = 40_000_000,
        )
        assertTrue(phone.all { it.met == true })

        val television = preparedReplacementRequirements(
            isTelevision = true,
            tunnelingEnabled = true,
            sessionIsLive = true,
            observedDownloadBps = 40_000_000,
        )
        assertEquals(
            listOf(false, false, true, true),
            television.map { it.met },
        )
    }

    @Test
    fun `a condition this screen cannot judge says so rather than failing it`() {
        // Settings is not inside a session. Reporting the two session-scoped
        // conditions as unmet would read as "your device fails", which is a
        // different claim from "not checked here".
        val fromSettings = preparedReplacementRequirements(
            isTelevision = false,
            tunnelingEnabled = false,
            sessionIsLive = null,
            observedDownloadBps = null,
            throughputKnown = false,
        )
        assertEquals(listOf(true, true, null, null), fromSettings.map { it.met })
        assertEquals(
            listOf("Met", "Met", "Checked during playback", "Checked during playback"),
            fromSettings.map { it.status },
        )
        assertTrue(fromSettings.all { it.detail.isNotBlank() })
    }

    @Test
    fun `a vod session is named as the reason nothing will fire`() {
        val vod = preparedReplacementRequirements(
            isTelevision = false,
            tunnelingEnabled = false,
            sessionIsLive = false,
            observedDownloadBps = 40_000_000,
        )
        assertEquals(false, vod[2].met)
        assertTrue(vod[2].detail.contains("VOD"))
    }
}
