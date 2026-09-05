package tv.plurx.app.player

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNotNull
import kotlin.test.assertNull
import kotlin.test.assertTrue

private fun mapperSelection() = ClientSelection(
    quality = QualitySelection.Auto,
    audioTrack = 0,
    subtitle = SubtitleSelection(SubtitleMode.OFF),
    audioOffsetMs = 0,
    codec = CodecPolicy.AUTO,
    dynamicRange = DynamicRangePolicy.AUTO,
)

private fun mapperCapabilities() = DynamicCapabilities(
    platform = "android",
    maxHeight = 2_160,
    codecs = listOf(CodecPolicy.HEVC, CodecPolicy.H264),
    dynamicRanges = listOf(DynamicRangePolicy.HDR10, DynamicRangePolicy.SDR),
    dualPlayerPreparation = false,
)

/**
 * A player mid-title, playing, with ten seconds of runway. Every test states
 * only what it changes.
 */
private fun playing(
    position: Long = 60_000,
    duration: Long = 3_600_000,
) = PlayerControlObservation(
    positionMs = position,
    durationMs = duration,
    bufferedFromMs = position - 5_000,
    bufferedThroughMs = position + 10_000,
    rate = 1.0,
    isPaused = false,
    isEnded = false,
    isSeeking = false,
    hasStarted = true,
    waitingForMs = null,
    isLikelyToKeepUp = true,
    selection = mapperSelection(),
    capabilities = mapperCapabilities(),
)

private fun map(observation: PlayerControlObservation) =
    PlaybackControlMapping.snapshot(observation)

class PlaybackControlMappingOrdinaryStateTest {
    @Test
    fun `a playing player is active and rendering`() {
        val snapshot = map(playing())
        assertEquals(PlaybackDemand.ACTIVE, snapshot.demand)
        assertEquals(RenderState.RENDERING, snapshot.renderState)
        assertEquals(60_000, snapshot.positionMs)
        assertEquals(70_000, snapshot.bufferedThroughMs)
        assertEquals(55_000, snapshot.bufferedFromMs)
        assertNull(snapshot.seekTargetMs)
        assertTrue(snapshot.isValid)
    }

    @Test
    fun `a paused player holds rather than ending`() {
        val snapshot = map(playing().copy(isPaused = true, rate = 0.0))
        assertEquals(PlaybackDemand.HOLD, snapshot.demand)
        assertEquals(0.0, snapshot.playbackRate)
        assertTrue(snapshot.isValid)
    }

    @Test
    fun `a before-first-frame state is starting however busy it looks`() {
        val snapshot = map(playing().copy(hasStarted = false, isLikelyToKeepUp = false))
        assertEquals(RenderState.STARTING, snapshot.renderState)
    }

    @Test
    fun `a seek outranks every other busy state`() {
        val snapshot = map(
            playing().copy(
                isSeeking = true,
                seekTargetMs = 125_000,
                isLikelyToKeepUp = false,
                waitingForMs = 20_000,
            ),
        )
        assertEquals(RenderState.SEEKING, snapshot.renderState)
        assertEquals(60_000, snapshot.positionMs, "the current playhead remains current")
        assertEquals(125_000, snapshot.seekTargetMs, "the explicit destination is retained")
    }

    @Test
    fun `only a seek reports a target`() {
        listOf(
            playing(),
            playing().copy(isPaused = true),
            playing().copy(hasStarted = false),
        ).forEach { assertNull(map(it).seekTargetMs) }
    }
}

class PlaybackControlMappingWaitTest {
    @Test
    fun `a short wait is waiting and a long one is stalled`() {
        assertEquals(
            RenderState.WAITING,
            map(playing().copy(waitingForMs = PlaybackControlMapping.PERSISTENT_STALL_MS - 1))
                .renderState,
        )
        assertEquals(
            RenderState.STALLED,
            map(playing().copy(waitingForMs = PlaybackControlMapping.PERSISTENT_STALL_MS))
                .renderState,
            "the boundary is inclusive",
        )
    }

    @Test
    fun `a player that cannot keep up is waiting even without a reported wait`() {
        assertEquals(RenderState.WAITING, map(playing().copy(isLikelyToKeepUp = false)).renderState)
    }

    @Test
    fun `a paused player that cannot keep up is not waiting`() {
        assertEquals(
            RenderState.RENDERING,
            map(playing().copy(isPaused = true, isLikelyToKeepUp = false)).renderState,
            "a paused player is not waiting for anything",
        )
    }

    @Test
    fun `a stalled player reports a starved decoder`() {
        assertEquals(
            DecoderState.STARVED,
            map(playing().copy(waitingForMs = 30_000)).observation?.decoderState,
        )
    }
}

class PlaybackControlMappingEndTest {
    @Test
    fun `a stream ending at the title end is terminal`() {
        val snapshot = map(playing(position = 3_600_000).copy(isEnded = true))
        assertEquals(PlaybackDemand.END, snapshot.demand)
        assertEquals(RenderState.ENDED, snapshot.renderState)
    }

    @Test
    fun `a stream ending within the slack is still terminal`() {
        val snapshot = map(
            playing(position = 3_600_000 - PlaybackControlMapping.ENDED_SLACK_MS)
                .copy(isEnded = true),
        )
        assertEquals(PlaybackDemand.END, snapshot.demand)
    }

    @Test
    fun `a truncated stream keeps demanding and reports failed`() {
        val snapshot = map(playing(position = 1_200_000).copy(isEnded = true))
        assertEquals(
            PlaybackDemand.ACTIVE,
            snapshot.demand,
            "the bytes ran out early; the client reopens rather than leaving",
        )
        assertEquals(RenderState.FAILED, snapshot.renderState)
    }

    @Test
    fun `a growing stream with no known duration ends terminally`() {
        val snapshot = map(playing(position = 90_000, duration = 0).copy(isEnded = true))
        assertEquals(PlaybackDemand.END, snapshot.demand)
        assertEquals(RenderState.ENDED, snapshot.renderState)
    }

    @Test
    fun `an error outranks an end`() {
        val snapshot = map(
            playing(position = 3_600_000).copy(isEnded = true, errorCode = ClientErrorCode.DECODER),
        )
        assertEquals(RenderState.FAILED, snapshot.renderState)
        assertEquals(DecoderState.FAILED, snapshot.observation?.decoderState)
    }
}

class PlaybackControlMappingPositionTest {
    @Test
    fun `position is clamped to a known duration`() {
        assertEquals(3_600_000, map(playing(position = 3_700_000)).positionMs)
    }

    @Test
    fun `position is not clamped without a known duration`() {
        assertEquals(3_700_000, map(playing(position = 3_700_000, duration = 0)).positionMs)
    }

    @Test
    fun `a negative position is floored`() {
        assertEquals(0, map(playing(position = -5)).positionMs)
    }

    @Test
    fun `no contiguous range means no runway rather than a guess`() {
        val snapshot = map(playing().copy(bufferedFromMs = null, bufferedThroughMs = null))
        assertNull(snapshot.bufferedFromMs)
        assertEquals(
            snapshot.positionMs,
            snapshot.bufferedThroughMs,
            "zero runway, which is exactly what an unbuffered playhead has",
        )
        assertTrue(snapshot.isValid)
    }

    @Test
    fun `a range ending behind the playhead still reports zero runway`() {
        val snapshot = map(playing().copy(bufferedThroughMs = 58_000))
        assertEquals(snapshot.positionMs, snapshot.bufferedThroughMs)
        assertTrue(snapshot.isValid, "the server rejects a runway behind the playhead")
    }

    @Test
    fun `a range starting ahead of the playhead is pulled back to it`() {
        val snapshot = map(playing().copy(bufferedFromMs = 61_000))
        assertEquals(snapshot.positionMs, snapshot.bufferedFromMs)
        assertTrue(snapshot.isValid)
    }
}

class PlaybackControlMappingRateTest {
    @Test
    fun `an active player never reports a zero rate`() {
        assertEquals(
            1.0,
            map(playing().copy(rate = 0.0)).playbackRate,
            "zero from an active player reads as a hold nobody asked for",
        )
    }

    @Test
    fun `an active player's rate is floored at a quarter`() {
        assertEquals(
            PlaybackControlMapping.MIN_ACTIVE_RATE,
            map(playing().copy(rate = 0.1)).playbackRate,
        )
    }

    @Test
    fun `an absurd rate is capped in both demands`() {
        assertEquals(PlaybackControlMapping.MAX_RATE, map(playing().copy(rate = 64.0)).playbackRate)
        assertEquals(
            PlaybackControlMapping.MAX_RATE,
            map(playing().copy(rate = 64.0, isPaused = true)).playbackRate,
        )
    }

    @Test
    fun `a non-finite rate becomes zero rather than travelling to the server`() {
        val snapshot = map(playing().copy(isPaused = true, rate = Double.NaN))
        assertEquals(0.0, snapshot.playbackRate)
        assertTrue(snapshot.isValid)
    }
}

class PlaybackControlMappingObservationTest {
    @Test
    fun `a before-start player reports an unknown decoder`() {
        assertEquals(
            DecoderState.UNKNOWN,
            map(playing().copy(hasStarted = false)).observation?.decoderState,
        )
    }

    @Test
    fun `dropped frames travel and a negative count does not`() {
        assertEquals(42, map(playing().copy(droppedFrames = 42)).observation?.droppedFrames)
        assertNull(map(playing().copy(droppedFrames = -1)).observation?.droppedFrames)
    }

    @Test
    fun `detail only travels with a code`() {
        assertNull(
            map(playing().copy(errorDetail = "something went wrong")).observation?.errorDetail,
            "the server rejects a detail with no code",
        )
        assertEquals(
            "something went wrong",
            map(
                playing().copy(
                    errorDetail = "something went wrong",
                    errorCode = ClientErrorCode.NETWORK,
                ),
            ).observation?.errorDetail,
        )
    }

    @Test
    fun `a recovery path's evidence outranks what the player can see`() {
        val value = assertNotNull(
            map(
                playing().copy(
                    errorCode = ClientErrorCode.MEDIA,
                    errorDetail = "media_code_0",
                    observationOverride = ClientObservation(
                        decoderState = DecoderState.STARVED,
                        errorCode = ClientErrorCode.MANIFEST,
                        errorDetail = "manifest_load_error",
                    ),
                ),
            ).observation,
        )
        assertEquals(DecoderState.STARVED, value.decoderState)
        assertEquals(ClientErrorCode.MANIFEST, value.errorCode)
        assertEquals("manifest_load_error", value.errorDetail)
    }

    @Test
    fun `an override that says nothing changes nothing`() {
        val value = assertNotNull(
            map(
                playing().copy(droppedFrames = 7, observationOverride = ClientObservation()),
            ).observation,
        )
        assertEquals(DecoderState.READY, value.decoderState)
        assertEquals(7, value.droppedFrames)
        assertNull(value.errorCode)
    }

    @Test
    fun `a render override is honoured but never over an error or an end`() {
        assertEquals(
            RenderState.STALLED,
            map(playing().copy(renderOverride = RenderState.STALLED)).renderState,
        )
        assertEquals(
            RenderState.FAILED,
            map(
                playing().copy(
                    renderOverride = RenderState.STALLED,
                    errorCode = ClientErrorCode.DECODER,
                ),
            ).renderState,
            "an error outranks an override",
        )
        assertEquals(
            RenderState.ENDED,
            map(
                playing(position = 3_600_000)
                    .copy(renderOverride = RenderState.STALLED, isEnded = true),
            ).renderState,
            "an end outranks an override",
        )
    }
}

class PlaybackControlMappingThroughputTest {
    @Test
    fun `a throughput estimate travels and a zero does not`() {
        assertEquals(
            8_000_000,
            map(playing().copy(observedDownloadBps = 8_000_000)).observedDownloadBps,
        )
        assertNull(
            map(playing().copy(observedDownloadBps = 0)).observedDownloadBps,
            "zero is not an estimate",
        )
    }
}

class PlaybackControlMappingTotalityTest {
    @Test
    fun `every mapped state is accepted by snapshot validation`() {
        var checked = 0
        for (paused in listOf(true, false)) {
            for (ended in listOf(true, false)) {
                for (seeking in listOf(true, false)) {
                    for (started in listOf(true, false)) {
                        for (waiting in listOf(null, 100L, 30_000L)) {
                            for (position in listOf(0L, 60_000L, 3_700_000L)) {
                                val snapshot = map(
                                    playing(position = position).copy(
                                        isPaused = paused,
                                        isEnded = ended,
                                        isSeeking = seeking,
                                        hasStarted = started,
                                        waitingForMs = waiting,
                                    ),
                                )
                                assertTrue(
                                    snapshot.isValid,
                                    "the server would reject ${snapshot.renderState} " +
                                        "at ${snapshot.positionMs}",
                                )
                                checked += 1
                            }
                        }
                    }
                }
            }
        }
        assertEquals(144, checked)
    }
}
