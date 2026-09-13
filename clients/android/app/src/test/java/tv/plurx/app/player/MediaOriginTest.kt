package tv.plurx.app.player

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.HlsStart

class MediaOriginTest {
    @Test
    fun hlsPrefersTheTrueOriginAndRetainsOldServerFallbacks() {
        val modern = HlsStart(
            session_id = "modern",
            playlist_url = "/hls/modern/index.m3u8",
            start_seconds = 10.5,
            media_origin_ms = 10_000,
        )
        assertEquals(10_000L, sessionMediaOriginMs(modern))

        val oldServer = HlsStart(
            session_id = "old",
            playlist_url = "/hls/old/index.m3u8",
            start_seconds = 10.5,
        )
        assertEquals(10_500L, sessionMediaOriginMs(oldServer))

        val cached = modern.copy(vod = true)
        assertEquals(0L, sessionMediaOriginMs(cached))
    }

    @Test
    fun hlsSessionOriginDrivesTheControllerTimeline() {
        val copied = HlsStart(
            session_id = "copy",
            playlist_url = "/hls/copy/index.m3u8",
            start_seconds = 10.5,
            media_origin_ms = 10_000,
        )
        val timeline = sessionPlaybackTimeline(copied, requestedStartMs = 10_500)
        assertEquals(SessionPlaybackTimeline(baseMs = 10_000, attachPositionMs = 0), timeline)
        assertEquals(
            12_500L,
            realMediaPositionMs(
                playerPositionMs = 2_500,
                directTransport = false,
                sessionIsVod = false,
                progressiveTransport = false,
                progressiveOriginMs = 0,
                sessionBaseMs = timeline.baseMs,
            ),
        )

        val cached = copied.copy(vod = true, media_origin_ms = 123_000)
        val cachedTimeline = sessionPlaybackTimeline(cached, requestedStartMs = 10_500)
        assertEquals(SessionPlaybackTimeline(baseMs = 0, attachPositionMs = 10_500), cachedTimeline)
        assertEquals(
            10_500L,
            realMediaPositionMs(
                playerPositionMs = cachedTimeline.attachPositionMs,
                directTransport = false,
                sessionIsVod = true,
                progressiveTransport = false,
                progressiveOriginMs = 0,
                sessionBaseMs = cachedTimeline.baseMs,
            ),
        )

        assertEquals(
            22_500L,
            realMediaPositionMs(
                playerPositionMs = 2_500,
                directTransport = false,
                sessionIsVod = false,
                progressiveTransport = true,
                progressiveOriginMs = 20_000,
                sessionBaseMs = 0,
            ),
        )
    }

    @Test
    fun progressiveHeaderParsingIsCaseInsensitiveAndRejectsBadValues() {
        assertEquals(
            10_000L,
            mediaOriginMsFromHeaders(mapOf("x-plurx-media-origin-ms" to listOf("10000"))),
        )
        assertNull(mediaOriginMsFromHeaders(mapOf(MEDIA_ORIGIN_HEADER to listOf("-1"))))
        assertNull(mediaOriginMsFromHeaders(mapOf(MEDIA_ORIGIN_HEADER to listOf("not-a-time"))))
    }

    @Test
    fun aLateProgressiveResponseCannotReplaceTheNewSeekEpoch() {
        val origin = ProgressiveMediaOrigin()
        val first = "http://server/stream.mp4?start=10.5"
        val second = "http://server/stream.mp4?start=20.5"
        val headers = mapOf(MEDIA_ORIGIN_HEADER to listOf("10000"))

        origin.begin(first, 10_500)
        assertTrue(origin.acceptResponse(first, headers))
        assertEquals(10_000L, origin.currentOriginMs())

        origin.begin(second, 20_500)
        assertFalse(origin.acceptResponse(first, headers))
        assertEquals(20_500L, origin.currentOriginMs())

        assertTrue(
            origin.acceptResponse(
                second,
                mapOf(MEDIA_ORIGIN_HEADER to listOf("20000")),
            ),
        )
        assertEquals(20_000L, origin.currentOriginMs())
    }
}

/**
 * A prepared successor has no [HlsStart] to read — it has the one number the
 * server sent — so the timeline mapping takes a raw origin and the two callers
 * share it rather than computing it twice and eventually disagreeing.
 */
class SuccessorTimelineTest {
    @Test
    fun theRawOriginPathReproducesTheHlsStartPathExactly() {
        // Byte for byte the same answers as before the generalisation, on the
        // three shapes that actually occur: a copy session with a real origin,
        // an old server that only sends start_seconds, and a cached VOD one.
        val copied = HlsStart(
            session_id = "copy",
            playlist_url = "/hls/copy/index.m3u8",
            start_seconds = 10.5,
            media_origin_ms = 10_000,
        )
        assertEquals(
            sessionPlaybackTimeline(copied, requestedStartMs = 10_500),
            sessionPlaybackTimeline(
                mediaOriginMs = 10_000,
                isVod = false,
                requestedStartMs = 10_500,
            ),
        )
        val oldServer = copied.copy(media_origin_ms = null)
        assertEquals(
            sessionPlaybackTimeline(oldServer, requestedStartMs = 10_500),
            sessionPlaybackTimeline(
                mediaOriginMs = 10_500,
                isVod = false,
                requestedStartMs = 10_500,
            ),
        )
        val cached = copied.copy(vod = true)
        assertEquals(
            sessionPlaybackTimeline(cached, requestedStartMs = 10_500),
            sessionPlaybackTimeline(mediaOriginMs = 0, isVod = true, requestedStartMs = 10_500),
        )
    }

    @Test
    fun aLiveRecoveryOriginBecomesTheBaseAndAttachesAtZero() {
        val timeline = sessionPlaybackTimeline(
            mediaOriginMs = 1_800_000,
            isVod = false,
            requestedStartMs = 1_805_000,
        )
        assertEquals(SessionPlaybackTimeline(baseMs = 1_800_000, attachPositionMs = 0), timeline)
    }

    @Test
    fun aVodOriginIsIgnoredAndTheRequestedPositionIsTheAttach() {
        val timeline = sessionPlaybackTimeline(
            mediaOriginMs = 123_000,
            isVod = true,
            requestedStartMs = 10_500,
        )
        assertEquals(SessionPlaybackTimeline(baseMs = 123_000, attachPositionMs = 10_500), timeline)
    }

    @Test
    fun aNegativeOriginIsClampedRatherThanTrusted() {
        assertEquals(
            0L,
            sessionPlaybackTimeline(
                mediaOriginMs = -5,
                isVod = false,
                requestedStartMs = 0,
            ).baseMs,
        )
    }

    @Test
    fun theSuccessorAttachesWhereTheIncumbentIsWatching() {
        // The successor's session-relative zero maps to media_origin_ms, so the
        // local position to prime is the difference — which is exactly why the
        // server sends that number at all.
        assertEquals(5_000L, successorAttachPositionMs(mediaOriginMs = 1_800_000, filmPositionMs = 1_805_000))
        assertEquals(1_805_000L, successorAttachPositionMs(mediaOriginMs = 0, filmPositionMs = 1_805_000))
        // A successor whose origin is already past the playhead starts at zero
        // rather than negative.
        assertEquals(0L, successorAttachPositionMs(mediaOriginMs = 1_900_000, filmPositionMs = 1_805_000))
    }

    @Test
    fun theSuccessorsRunwayComesBackInFilmTime() {
        // The inverse. Every number the protocol carries is film time, and the
        // player only knows its own local clock.
        assertEquals(1_808_000L, successorFilmPositionMs(mediaOriginMs = 1_800_000, playerPositionMs = 8_000))
        assertEquals(
            1_805_000L,
            successorFilmPositionMs(
                mediaOriginMs = 1_800_000,
                playerPositionMs = successorAttachPositionMs(1_800_000, 1_805_000),
            ),
        )
        assertEquals(0L, successorFilmPositionMs(mediaOriginMs = -1, playerPositionMs = -1))
    }
}

/**
 * `observed_download_bps` is the client's half of the server's preparation
 * floor, and the floor asks for **headroom**: `observed >= 2 * delivered`. A
 * window that divides bytes by wall clock cannot answer that question, because
 * an HLS player with a full buffer fetches a segment in a burst and then idles
 * for seconds — so the denominator is mostly idle.
 *
 * The old algorithm's pathology was that the reading *oscillated*: roughly the
 * link speed at the end of a burst, and a small fraction of it on the first
 * byte after a gap. **So a test that samples only at the end of a run passes
 * against it**, which is what the first draft of this class did — eight of its
 * nine cases would have passed unchanged against the code they were written to
 * replace. Every case below now samples where the old code was wrong, and the
 * bands are tight: the arithmetic here is exact to about one part in a million,
 * so a ±10 % band is five orders of magnitude looser than the computation and
 * pins nothing.
 *
 * Nothing here needs Media3, which is the point: this arithmetic was
 * untestable while it lived inside the transfer listener, and it was wrong the
 * whole time it was untestable.
 *
 * Checked rather than assumed: the old algorithm was reinstated behind this
 * same API and the suite run against it. **Eight of these ten reject it.** The
 * two that do not are `aWindowIsNotCountedTwice`, which guards a regression
 * only the *new* shape can have — carrying a closed window's clock into the
 * next one — and `nothingIsReportedBeforeASecondOfTransferHasHappened`, which
 * pins that a window exists at all rather than what its denominator is. Both
 * are kept knowingly.
 */
class ThroughputWindowTest {
    private val second = 1_000_000_000L
    private val megabit = 1_000_000L

    /** Bytes that carry [bitsPerSecond] for [nanos]. */
    private fun bytesFor(bitsPerSecond: Long, nanos: Long): Int =
        (bitsPerSecond / 8.0 * (nanos / 1e9)).toInt()

    @Test
    fun idleTimeBetweenSegmentsIsNotInTheDenominator() {
        // A 100 Mbit/s link carrying a stream that keeps it busy for 0.95 s per
        // segment and idle for six. The sample that discriminates is the first
        // byte after a gap — the one the old window divided by the gap.
        //
        // The burst is sized so that byte is also the one that completes a
        // second of transfer, which is the only way this class can report on
        // it: it needs a second of *transfer*, and a fraction of a burst is not
        // one. The old algorithm reported about 8 Mbit/s there against a link
        // doing a hundred.
        val window = ThroughputWindow()
        var now = 0L
        repeat(4) { cycle ->
            window.transferStarted(now)
            repeat(19) {
                now += second / 20
                window.bytesTransferred(bytesFor(100 * megabit, second / 20), now)
            }
            window.transferEnded(now)
            now += 6 * second

            window.transferStarted(now)
            now += second / 20
            window.bytesTransferred(bytesFor(100 * megabit, second / 20), now)
            val afterGap = requireNotNull(
                window.bitsPerSecond(),
                { "cycle $cycle: a second of transfer has passed and nothing was reported" },
            )
            assertTrue(
                "cycle $cycle: the first byte after a six-second gap read $afterGap, " +
                    "expected about 100 Mbit/s",
                afterGap > 95 * megabit && afterGap < 105 * megabit,
            )
            window.transferEnded(now)
        }
    }

    @Test
    fun aWindowThatSpansAnIdleGapDoesNotReportNearZero() {
        // Two half-second bursts at 80 Mbit/s with seven idle seconds between
        // them. The old window closed on the first byte after the gap and
        // reported about 5 Mbit/s.
        val window = ThroughputWindow()
        var now = 0L
        window.transferStarted(now)
        now += second / 2
        window.bytesTransferred(bytesFor(80 * megabit, second / 2), now)
        window.transferEnded(now)

        now += 7 * second
        window.transferStarted(now)
        now += second / 2
        window.bytesTransferred(bytesFor(80 * megabit, second / 2), now)
        window.transferEnded(now)

        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured, expected about 80 Mbit/s",
            measured > 76 * megabit && measured < 84 * megabit,
        )
    }

    @Test
    fun nothingIsReportedBeforeASecondOfTransferHasHappened() {
        // Null is the honest answer, and the server reads it as "may not be
        // offered a preparation" — correct for a client that has measured
        // nothing yet. Three quarters of a second is past the old window's
        // half-second threshold and short of this one's.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(100 * megabit, second * 3 / 4), second * 3 / 4)
        window.transferEnded(second * 3 / 4)
        assertNull(window.bitsPerSecond())
    }

    @Test
    fun concurrentTransfersCountTheLinkBusyOnce() {
        // Media3 fetches a playlist and a segment at the same time. The clock
        // must not start twice, and — the part worth testing — it must not stop
        // when the first of the two finishes while the other is still pulling.
        //
        // So the window is crossed *after* the first `transferEnded`, with one
        // transfer still open. An implementation that stopped the clock at 0.5 s
        // would divide by half a second and report about 100 Mbit/s.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.transferStarted(second / 10)
        window.bytesTransferred(bytesFor(50 * megabit, second), second / 2)
        window.transferEnded(second / 2)
        window.bytesTransferred(1, second)
        window.transferEnded(second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured over one busy second, expected about 50 Mbit/s",
            measured > 48 * megabit && measured < 52 * megabit,
        )
    }

    @Test
    fun aTransferThatNeverEndsCannotPoisonEveryLaterWindow() {
        // The failure mode that would be worse than the bug this class fixed:
        // `TransferListener` is attached to a factory, so a wrapping data source
        // anywhere in the chain can lose the `onTransferEnd`. A leaked in-flight
        // count leaves the clock running, and every later window then accrues
        // wall time — the wall-clock average again, silently and permanently.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(100 * megabit, second), second)
        // ...and no end. Ten minutes later a new stream starts.
        val muchLater = 600 * second
        window.transferStarted(muchLater)
        window.bytesTransferred(bytesFor(40 * megabit, second), muchLater + second)
        window.transferEnded(muchLater + second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured after a leaked transfer, expected about 40 Mbit/s",
            measured > 38 * megabit && measured < 42 * megabit,
        )
    }

    @Test
    fun aNewStreamInheritsNothingFromTheOneItReplaces() {
        // The first exchange of a replacement is the only one the server's
        // replacement seam reads, so a predecessor's rate arriving there is
        // worse than no rate.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(100 * megabit, 2 * second), 2 * second)
        window.transferEnded(2 * second)
        requireNotNull(window.bitsPerSecond())
        window.begin()
        assertNull(window.bitsPerSecond())
        // And the clock it left running is gone with it: the new stream's two
        // busy seconds must not be divided by the time since the old one
        // started, and its bytes must not be added to the old one's.
        window.transferStarted(4 * second)
        window.bytesTransferred(bytesFor(25 * megabit, 2 * second), 6 * second)
        window.transferEnded(6 * second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured after begin(), expected about 25 Mbit/s",
            measured > 24 * megabit && measured < 26 * megabit,
        )
    }

    @Test
    fun aWindowIsNotCountedTwice() {
        // Four seconds of continuous transfer at 40 Mbit/s. An implementation
        // that carried a closed window's clock into the next one would read
        // 20, then 13.3, then 10.
        val window = ThroughputWindow()
        window.transferStarted(0)
        var now = 0L
        repeat(4) {
            now += second
            window.bytesTransferred(bytesFor(40 * megabit, second), now)
        }
        window.transferEnded(now)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured, expected about 40 Mbit/s",
            measured > 39 * megabit && measured < 41 * megabit,
        )
    }

    @Test
    fun bytesWithNoStartAdoptTheTransferRatherThanDivideByNothing() {
        // Exactly 20 Mbit/s over the two seconds from the adopting byte, not a
        // band wide enough to accept anything.
        val window = ThroughputWindow()
        window.bytesTransferred(1_000, 0)
        window.bytesTransferred(bytesFor(20 * megabit, 2 * second), 2 * second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured, expected about 20 Mbit/s",
            measured > 19 * megabit && measured < 21 * megabit,
        )
    }

    @Test
    fun anAbsurdReadingIsClampedRatherThanInvalidatingEveryExchange() {
        // `PlaybackControlSnapshot.isValid` refuses a rate above the protocol
        // ceiling, and an invalid snapshot is dropped whole — so an
        // out-of-range reading would silently stop the client reporting
        // anything at all, which is a much larger failure than a wrong number.
        //
        // The bytes have to accumulate inside ONE window for the clamp to
        // engage: a call that closes the window resets the count, so a hundred
        // closing calls each report their own small number and the ceiling is
        // never approached.
        val window = ThroughputWindow()
        window.transferStarted(0)
        repeat(1_000) { window.bytesTransferred(Int.MAX_VALUE, second / 2) }
        window.bytesTransferred(Int.MAX_VALUE, second)
        assertEquals(
            PlaybackControl.MAX_OBSERVED_DOWNLOAD_BPS,
            requireNotNull(window.bitsPerSecond()),
        )
    }

    @Test
    fun theListenerPublishesTheWindowAndForgetsItOnANewStream() {
        // What this can prove from the JVM lane, and no more: the wrapper reads
        // the window it owns, and `begin` resets it. The `TransferListener`
        // callbacks themselves take Media3's `DataSource` and `DataSpec`, whose
        // `android.net.Uri` throws here — so whether `onTransferEnd` is wired at
        // all is checked by the compiler and by the instrumented lane, not by
        // this test. Said plainly rather than dressed up: a test that asserted
        // `window.bitsPerSecond() == origin.currentObservedBitsPerSecond()`
        // through the same object would be proving one delegating line.
        val origin = ProgressiveMediaOrigin()
        val window = origin.throughputWindowForTest()
        assertNull(origin.currentObservedBitsPerSecond())
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(60 * megabit, 2 * second), 2 * second)
        window.transferEnded(2 * second)
        val measured = requireNotNull(origin.currentObservedBitsPerSecond())
        assertTrue(
            "the wrapper reported $measured, expected about 60 Mbit/s",
            measured > 58 * megabit && measured < 62 * megabit,
        )
        origin.begin("http://server/stream.mp4", 0)
        assertNull(origin.currentObservedBitsPerSecond())
    }

    /**
     * M5's `BEHIND_LIVE_WINDOW` recovery reads the last real position in FILM
     * time and hands `ExoPlayer.seekTo` a number in the PLAYER's, so the two
     * mappings have to be each other's inverse on every transport.
     *
     * UNRUN: no Android toolchain on the machine this was written on.
     */
    @Test
    fun playerLocalPositionIsTheInverseOfTheFilmPosition() {
        data class Transport(
            val name: String,
            val direct: Boolean,
            val vod: Boolean,
            val progressive: Boolean,
            val originMs: Long,
            val baseMs: Long,
        )
        for (transport in listOf(
            Transport("direct", direct = true, vod = false, progressive = false, originMs = 0, baseMs = 0),
            Transport("vod session", direct = false, vod = true, progressive = false, originMs = 0, baseMs = 0),
            Transport("progressive remux", direct = false, vod = false, progressive = true, originMs = 86_000, baseMs = 0),
            Transport("live-shaped session", direct = false, vod = false, progressive = false, originMs = 0, baseMs = 120_000),
        )) {
            for (local in listOf(0L, 1L, 4_321L, 600_000L)) {
                val film = realMediaPositionMs(
                    playerPositionMs = local,
                    directTransport = transport.direct,
                    sessionIsVod = transport.vod,
                    progressiveTransport = transport.progressive,
                    progressiveOriginMs = transport.originMs,
                    sessionBaseMs = transport.baseMs,
                )
                assertEquals(
                    "${transport.name} at ${local}ms round-trips",
                    local,
                    playerLocalPositionMs(
                        filmPositionMs = film,
                        directTransport = transport.direct,
                        sessionIsVod = transport.vod,
                        progressiveTransport = transport.progressive,
                        progressiveOriginMs = transport.originMs,
                        sessionBaseMs = transport.baseMs,
                    ),
                )
            }
        }
        // A film position before this session's own origin cannot be seeked to
        // inside it, and a negative seek is not an answer.
        assertEquals(
            0L,
            playerLocalPositionMs(
                filmPositionMs = 1_000,
                directTransport = false,
                sessionIsVod = false,
                progressiveTransport = false,
                progressiveOriginMs = 0,
                sessionBaseMs = 120_000,
            ),
        )
    }
}
