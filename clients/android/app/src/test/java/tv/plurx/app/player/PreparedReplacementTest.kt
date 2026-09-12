package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
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
        // The vocabulary spells its names literally so the cross-client
        // conformance check can read them out of the source; this is what stops
        // the literal and the constant drifting apart once it does.
        assertTrue(
            PlaybackControl.SUPPORTED_ACTIONS
                .contains(PlaybackControl.PREPARE_REPLACEMENT_ACTION),
        )
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
        // A commit carries both. The frame is the measurement; the origin is
        // the evidence, and the server refuses a commit that omits either.
        assertFalse(
            ActionAcknowledgement(
                ACTION_ID,
                AcknowledgementState.COMMITTED,
                firstFrameUnixMs = 1_788_000_000_000,
            ).isValid,
        )
        assertFalse(
            ActionAcknowledgement(
                ACTION_ID,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = 0,
            ).isValid,
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

    @Test
    fun `a switched successor with no frame fails without a commit timestamp`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        assertTrue(ledger.switched())
        val failed = assertNotNull(ledger.failedAfterSwitch())
        assertEquals(AcknowledgementState.FAILED, failed.state)
        assertNull(failed.firstFrameUnixMs)
        assertNull(failed.committedMediaOriginMs)
        assertFalse(ledger.isLive)
        assertNull(ledger.failedAfterSwitch(), "the terminal settlement is emitted once")
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
        assertTrue(vod[2].detail.contains("delivered throughput"))
    }
}

/**
 * The window between the swap and the successor's first frame.
 *
 * A prepared successor has no surface, so it renders nothing until it takes
 * one — which means the commit is always owed a moment *after* the switch. In
 * that gap the viewer is already watching the successor, so nothing may abandon
 * it: an `aborted` there tells the server to tear down the incarnation its
 * pointer is about to move to.
 */
class PreparedCommitWindowTest {
    @Test
    fun `nothing may replace a preparation the viewer is already watching`() {
        // The switched window took the "supersedes" branch, where `terminal()`
        // refuses to abort a switched preparation and the branch reads that
        // refusal as "nothing was owed" — silently dropping the commit and
        // stamping the next preparation's id with it.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.switched()
        val second = "55555555-5555-4555-8555-555555555555"
        assertTrue(ledger.offer(prepare(actionId = second)) is PreparationOffer.Refuse)
        assertEquals(ACTION_ID, ledger.actionId, "the switched preparation still owns the ledger")
        assertTrue(ledger.isSwitched)
        val committed = assertNotNull(ledger.committed(1_788_000_000_000))
        assertEquals(ACTION_ID, committed.actionId)
    }

    @Test
    fun `a switched preparation cannot be aborted or failed`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.bufferReady(90_000)
        assertTrue(ledger.switched())
        assertTrue(ledger.isSwitched)
        assertNull(ledger.aborted(), "the viewer is watching this pipeline")
        assertNull(ledger.failed())
        assertTrue(ledger.isLive, "it still owes a commit")
        val committed = assertNotNull(ledger.committed(1_788_000_000_000))
        assertEquals(AcknowledgementState.COMMITTED, committed.state)
        assertFalse(ledger.isLive)
    }

    @Test
    fun `switching twice is refused, and a dead preparation cannot switch`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        assertTrue(ledger.switched())
        assertFalse(ledger.switched())

        val abandoned = PreparedReplacementLedger()
        abandoned.offer(prepare())
        abandoned.aborted()
        assertFalse(abandoned.switched())
    }

    @Test
    fun `a replayed action id after the ladder settled is still the same preparation`() {
        // The regression this exists for: the ordinary commit produces it. The
        // exchange carrying `buffer_ready` is in flight when the client
        // switches and commits, and the server — which has not settled
        // anything yet — replays `Prepare` with the same `action_id` in its
        // answer. Read as a *new* preparation that builds a third pipeline over
        // the one the viewer is watching.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.bufferReady(90_000)
        ledger.switched()
        ledger.committed(1_788_000_000_000)
        assertFalse(ledger.isLive)
        assertTrue(
            ledger.offer(prepare()) is PreparationOffer.Same,
            "a settled id is not a new staging",
        )
        // And the same after an abandon, which is the other way a preparation
        // stops being live while the server keeps replaying it.
        val abandoned = PreparedReplacementLedger()
        abandoned.offer(prepare())
        abandoned.aborted()
        assertTrue(abandoned.offer(prepare()) is PreparationOffer.Same)
    }
}

/**
 * The outbound body of an abandon, asserted rather than inspected.
 *
 * §C12.6 wants the abandon path proved by what goes on the wire. The ledger is
 * the seam it can be proved at: a `Controller` needs a `Context` and a real
 * ExoPlayer, neither of which exists in the JVM unit lane, and a test that
 * could not run would prove less than this one.
 */
class PreparedAbandonBodyTest {
    private fun encode(acknowledgement: ActionAcknowledgement?): String {
        val snapshot = PlaybackControlMapping.snapshot(
            PlayerControlObservation(
                positionMs = 60_000,
                durationMs = 3_600_000,
                bufferedFromMs = 60_000,
                bufferedThroughMs = 70_000,
                rate = 1.0,
                isPaused = false,
                isEnded = false,
                isSeeking = false,
                hasStarted = true,
                isLikelyToKeepUp = true,
                acknowledgement = acknowledgement,
                selection = ClientSelection(
                    quality = QualitySelection.Auto,
                    audioTrack = 0,
                    subtitle = SubtitleSelection(SubtitleMode.OFF),
                    audioOffsetMs = 0,
                    codec = CodecPolicy.AUTO,
                    dynamicRange = DynamicRangePolicy.AUTO,
                ),
                capabilities = DynamicCapabilities(
                    platform = "android",
                    maxHeight = 2_160,
                    codecs = listOf(CodecPolicy.H264),
                    dynamicRanges = listOf(DynamicRangePolicy.SDR),
                    dualPlayerPreparation = false,
                ),
            ),
        )
        return json.encodeToString(
            ControlRequest.serializer(),
            ControlRequest(
                protocol = PlaybackControl.PROTOCOL,
                generation = "11111111-1111-4111-8111-111111111111",
                controlEpoch = 7,
                clientInstanceId = "22222222-2222-4222-8222-222222222222",
                sequence = 9,
                demand = snapshot.demand,
                positionMs = snapshot.positionMs,
                bufferedThroughMs = snapshot.bufferedThroughMs,
                playbackRate = snapshot.playbackRate,
                renderState = snapshot.renderState,
                selection = snapshot.selection,
                acknowledgement = snapshot.sendableAcknowledgement,
                supportedActions = PlaybackControl.SUPPORTED_ACTIONS,
            ),
        )
    }

    @Test
    fun `an abandoned preparation puts aborted on the wire`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.metadataReady()
        val encoded = encode(ledger.aborted())
        assertTrue(encoded.contains("\"state\":\"aborted\""), encoded)
        assertTrue(encoded.contains("\"action_id\":\"$ACTION_ID\""), encoded)
    }

    @Test
    fun `a successor that will not build puts failed on the wire`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        val encoded = encode(ledger.failed())
        assertTrue(encoded.contains("\"state\":\"failed\""), encoded)
    }

    @Test
    fun `a preparation that was never offered puts nothing on the wire`() {
        assertFalse(encode(null).contains("acknowledgement"))
    }
}

class PreparedIdentifierShapeTest {
    @Test
    fun `an identifier the server minted is accepted even at a version this client predates`() {
        // The server runs `Uuid::parse_str`, which accepts v6 and v7. A client
        // stricter than the server here does not fail safe: an unrecognised
        // version makes the action fatal and kills the control plane for the
        // rest of the film.
        val v7 = "01912d4c-7b3a-7c2e-9f01-2b7c9d4e5f60"
        assertFalse(PlaybackControl.isUuid(v7), "the strict check is what guards the bootstrap")
        assertTrue(PlaybackControl.isUuidShape(v7))
        assertTrue(prepare(actionId = v7).preparedPayloadIsValid)
    }

    @Test
    fun `the shape check is still a shape check`() {
        assertFalse(PlaybackControl.isUuidShape(""))
        assertFalse(PlaybackControl.isUuidShape("../../etc/passwd"))
        assertFalse(PlaybackControl.isUuidShape("4444444444444444444444444444444"))
        assertFalse(PlaybackControl.isUuidShape("gggggggg-4444-4444-8444-444444444444"))
        assertTrue(PlaybackControl.isUuidShape(SUCCESSOR))
    }
}

class EffectiveSelectionDecodingTest {
    @Test
    fun `a payload missing a field the server declares plainly is refused`() {
        // The server's struct carries `deny_unknown_fields` and no `Option` on
        // `quality_auto`, `height`, `audio_offset_ms` or `codec`, so a payload
        // omitting one is a serde error there. A Kotlin default would turn that
        // refusal into a silent `height = 0` that passes validation and gets
        // seeded into the stall budget.
        for (missing in listOf("quality_auto", "height", "audio_offset_ms", "codec")) {
            val fields = linkedMapOf(
                "quality_auto" to "true",
                "height" to "1080",
                "audio_offset_ms" to "0",
                "codec" to "\"server_selected\"",
            )
            fields.remove(missing)
            val body = fields.entries.joinToString(",", "{", "}") { "\"${it.key}\":${it.value}" }
            assertFailsWith<kotlinx.serialization.MissingFieldException>(
                "$missing must be required",
            ) {
                json.decodeFromString(EffectiveSelection.serializer(), body)
            }
        }
    }

    @Test
    fun `the three the server declares optional may be absent`() {
        val decoded = json.decodeFromString(
            EffectiveSelection.serializer(),
            """{"quality_auto":false,"height":720,"audio_offset_ms":0,"codec":"source"}""",
        )
        assertNull(decoded.audioTrack)
        assertNull(decoded.subtitleBurn)
        assertNull(decoded.dynamicRange)
        assertTrue(decoded.isValid)
    }
}

/**
 * Two rules that used to live inside `Controller` as a pair of lines each, and
 * were wrong there both times. `Controller` needs a `Context` and a real
 * ExoPlayer, so nothing in the JVM lane can reach it — which is why every
 * defect that survived a review pass lived in that file. The answer is not more
 * tests; it is one fewer decision in there per defect.
 */
class PreparedGradeAdoptionTest {
    @Test
    fun `a grade that moves takes the profile with it`() {
        // The §9.1 two-axis case the server actually admits: a Dolby Vision
        // direct play handing over to a `server_selected` SDR transcode. Two
        // independent assignments left the profile standing and the panel read
        // "SDR - Profile 8", with each half correct on its own.
        val adopted = adoptedGrade(
            DeliveredGrade("dolby_vision", 8),
            selection(codec = "server_selected", dynamicRange = "sdr"),
        )
        assertEquals(DeliveredGrade("sdr", null), adopted)
    }

    @Test
    fun `a successor that names no grade moves neither half`() {
        val current = DeliveredGrade("hdr10", null)
        assertEquals(current, adoptedGrade(current, selection(dynamicRange = null)))
        assertEquals(current, adoptedGrade(current, null))
    }

    @Test
    fun `an unchanged grade is still adopted with no profile`() {
        // An `EffectiveSelection` carries no profile field, so "no answer" is
        // the only honest value once the successor is the one being described —
        // even when the grade itself did not move.
        assertEquals(
            DeliveredGrade("dolby_vision", null),
            adoptedGrade(DeliveredGrade("dolby_vision", 8), selection(dynamicRange = "dolby_vision")),
        )
    }
}

class SettlingSnapshotTest {
    private fun ended(
        acknowledgement: ActionAcknowledgement?,
        rate: Double = 0.0,
    ) = PlaybackControlMapping.snapshot(
        PlayerControlObservation(
            positionMs = 3_600_000,
            durationMs = 3_600_000,
            bufferedFromMs = 3_600_000,
            bufferedThroughMs = 3_600_000,
            rate = rate,
            isPaused = true,
            isEnded = true,
            isSeeking = false,
            hasStarted = true,
            isLikelyToKeepUp = false,
            acknowledgement = acknowledgement,
            selection = ClientSelection(
                quality = QualitySelection.Auto,
                audioTrack = 0,
                subtitle = SubtitleSelection(SubtitleMode.OFF),
                audioOffsetMs = 0,
                codec = CodecPolicy.AUTO,
                dynamicRange = DynamicRangePolicy.AUTO,
            ),
            capabilities = DynamicCapabilities(
                platform = "android",
                maxHeight = 2_160,
                codecs = listOf(CodecPolicy.H264),
                dynamicRanges = listOf(DynamicRangePolicy.SDR),
                dualPlayerPreparation = false,
            ),
        ),
    )

    @Test
    fun `a commit at the end of a title travels as a pair the server accepts`() {
        // Asserted as a pair on purpose. `demand` and `playback_rate` are one
        // answer computed together — the mapper derives the rate *from* the
        // demand — and raising one without the other produced `active` at rate
        // zero, which the server refuses outright, taking the commit with it.
        val committed = settlingSnapshot(
            ended(
                ActionAcknowledgement(
                    ACTION_ID,
                    AcknowledgementState.COMMITTED,
                    committedMediaOriginMs = 1_800_000,
                    firstFrameUnixMs = 1_788_000_000_000,
                ),
            ),
        )
        assertEquals(PlaybackDemand.ACTIVE, committed.demand)
        assertTrue(
            committed.playbackRate >= PlaybackControlMapping.MIN_ACTIVE_RATE,
            "active demand at rate ${committed.playbackRate} is a 400",
        )
        assertNotNull(committed.sendableAcknowledgement)
        assertTrue(committed.isValid)
        // The one field still saying what the player is actually doing is left
        // alone. This exchange is not what ends the session.
        assertEquals(RenderState.ENDED, committed.renderState)
    }

    @Test
    fun `every other settlement rides the exchange as mapped`() {
        for (state in listOf(
            AcknowledgementState.ABORTED,
            AcknowledgementState.FAILED,
            AcknowledgementState.METADATA_READY,
        )) {
            val snapshot = ended(ActionAcknowledgement(ACTION_ID, state))
            assertEquals(snapshot, settlingSnapshot(snapshot), "$state was rewritten")
            assertEquals(PlaybackDemand.END, settlingSnapshot(snapshot).demand)
        }
        val none = ended(null)
        assertEquals(none, settlingSnapshot(none))
    }

    @Test
    fun `a commit mid-title is not rewritten because it does not need to be`() {
        val playing = PlaybackControlMapping.snapshot(
            PlayerControlObservation(
                positionMs = 60_000,
                durationMs = 3_600_000,
                bufferedFromMs = 60_000,
                bufferedThroughMs = 70_000,
                rate = 1.0,
                isPaused = false,
                isEnded = false,
                isSeeking = false,
                hasStarted = true,
                isLikelyToKeepUp = true,
                acknowledgement = ActionAcknowledgement(
                    ACTION_ID,
                    AcknowledgementState.COMMITTED,
                    committedMediaOriginMs = 1_800_000,
                    firstFrameUnixMs = 1_788_000_000_000,
                ),
                selection = ClientSelection(
                    quality = QualitySelection.Auto,
                    audioTrack = 0,
                    subtitle = SubtitleSelection(SubtitleMode.OFF),
                    audioOffsetMs = 0,
                    codec = CodecPolicy.AUTO,
                    dynamicRange = DynamicRangePolicy.AUTO,
                ),
                capabilities = DynamicCapabilities(
                    platform = "android",
                    maxHeight = 2_160,
                    codecs = listOf(CodecPolicy.H264),
                    dynamicRanges = listOf(DynamicRangePolicy.SDR),
                    dualPlayerPreparation = false,
                ),
            ),
        )
        assertEquals(playing, settlingSnapshot(playing))
    }
}

/** A failed prepared transaction settles only that action; a later explicit
 * change may reserve and prime a fresh successor instead of inheriting
 * session-wide suppression from the earlier failure. */
class PreparedFailureRetryTest {
    @Test
    fun `a successor that was never playable does not suppress a later explicit change`() {
        val ledger = PreparedReplacementLedger()
        assertTrue(ledger.offer(prepare()) is PreparationOffer.Start)
        assertEquals(AcknowledgementState.FAILED, assertNotNull(ledger.failed()).state)
        val second = "55555555-5555-4555-8555-555555555555"
        assertTrue(ledger.offer(prepare(actionId = second)) is PreparationOffer.Start)
        assertEquals(second, ledger.actionId)
    }

    @Test
    fun `a successor that was playable and then failed does not end them`() {
        // An ordinary failure from a rung it had reached is a fact about this
        // attempt. The next offer is still worth taking.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.metadataReady()
        ledger.failed()
        val second = "55555555-5555-4555-8555-555555555555"
        assertTrue(ledger.offer(prepare(actionId = second)) is PreparationOffer.Start)
    }

    @Test
    fun `an abandoned preparation is not evidence about the platform`() {
        // The viewer seeked. That says nothing about whether a successor could
        // have become playable.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare())
        ledger.aborted()
        val second = "55555555-5555-4555-8555-555555555555"
        assertTrue(ledger.offer(prepare(actionId = second)) is PreparationOffer.Start)
    }
}

/**
 * `committed_media_origin_ms`: the field that makes a commit evidence rather
 * than an assertion.
 *
 * `action_id` says which offer the client is answering. This says the client
 * built the thing that offer described — and the server compares it against
 * the staged successor's own `media_origin_ms` and refuses a mismatch, which
 * closes the one gap the commit digest does not cover: a successor primed for
 * one point in the film and committed after the viewer seeked elsewhere.
 *
 * So the only correct source is the offer. A number this client derived from
 * its own player would agree with itself no matter what the viewer did, which
 * is exactly the check being defeated.
 */
class CommittedMediaOriginTest {
    @Test
    fun `a commit echoes the offer's origin rather than recomputing one`() {
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare(mediaOriginMs = 1_800_000))
        ledger.bufferReady(1_805_000)
        ledger.switched()
        val committed = assertNotNull(ledger.committed(1_788_000_000_000))
        assertEquals(1_800_000L, committed.committedMediaOriginMs)
        assertEquals(1_788_000_000_000L, committed.firstFrameUnixMs)
        assertTrue(committed.isValid)
    }

    @Test
    fun `a zero origin is echoed as zero rather than dropped`() {
        // The common VOD-shaped offer. `explicitNulls` is off, so a field left
        // null vanishes from the wire — and a commit that omits this one is a
        // `400` that takes the whole exchange with it.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare(mediaOriginMs = 0))
        ledger.switched()
        val committed = assertNotNull(ledger.committed(1_788_000_000_000))
        assertEquals(0L, committed.committedMediaOriginMs)
        val encoded = json.encodeToString(ActionAcknowledgement.serializer(), committed)
        assertTrue(encoded.contains("\"committed_media_origin_ms\":0"), encoded)
    }

    @Test
    fun `only a commit carries it`() {
        // The earlier states report progress toward a successor that is not
        // being published yet, and the server does not ask them for an origin.
        val ledger = PreparedReplacementLedger()
        ledger.offer(prepare(mediaOriginMs = 1_800_000))
        assertNull(assertNotNull(ledger.metadataReady()).committedMediaOriginMs)
        assertNull(assertNotNull(ledger.bufferReady(90_000)).committedMediaOriginMs)
        val abandoned = PreparedReplacementLedger()
        abandoned.offer(prepare(mediaOriginMs = 1_800_000))
        assertNull(assertNotNull(abandoned.aborted()).committedMediaOriginMs)
    }

    @Test
    fun `the field is absent from the wire when it is absent from the state`() {
        val encoded = json.encodeToString(
            ActionAcknowledgement.serializer(),
            ActionAcknowledgement(ACTION_ID, AcknowledgementState.ABORTED),
        )
        assertFalse(encoded.contains("committed_media_origin_ms"), encoded)
    }

    @Test
    fun `an origin outside the protocol's range is refused before it is sent`() {
        assertFalse(
            ActionAcknowledgement(
                ACTION_ID,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = -1,
                firstFrameUnixMs = 1,
            ).isValid,
        )
        assertFalse(
            ActionAcknowledgement(
                ACTION_ID,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = PlaybackControl.MAX_MEDIA_MILLIS + 1,
                firstFrameUnixMs = 1,
            ).isValid,
        )
        assertTrue(
            ActionAcknowledgement(
                ACTION_ID,
                AcknowledgementState.COMMITTED,
                committedMediaOriginMs = PlaybackControl.MAX_MEDIA_MILLIS,
                firstFrameUnixMs = 1,
            ).isValid,
        )
    }
}
