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
 * for seconds — so the denominator is mostly idle, and the reading a floor sees
 * is roughly the stream's own bitrate or, when a window happens to span a gap,
 * near zero.
 *
 * These drive the window directly with an injected clock. Nothing here needs
 * Media3, which is the point: this arithmetic was untestable while it lived
 * inside the transfer listener, and it was wrong the whole time.
 */
class ThroughputWindowTest {
    private val second = 1_000_000_000L
    private val megabit = 1_000_000L

    /** 12.5 MB/s = 100 Mbit/s. */
    private fun bytesFor(bitsPerSecond: Long, nanos: Long): Int =
        (bitsPerSecond / 8.0 * (nanos / 1e9)).toInt()

    @Test
    fun idleTimeBetweenSegmentsIsNotInTheDenominator() {
        // A 100 Mbit/s link carrying a 10 Mbit/s stream: each six-second
        // segment arrives in 0.6 s and the link then sits idle for 5.4 s. The
        // honest answer is 100, and the floor needs it to be at least twice the
        // 10 the server is delivering.
        val window = ThroughputWindow()
        var now = 0L
        repeat(4) {
            window.transferStarted(now)
            // The burst, delivered in chunks as Media3 reports them.
            repeat(6) {
                now += second / 10
                window.bytesTransferred(bytesFor(100 * megabit, second / 10), now)
            }
            window.transferEnded(now)
            now += 5_400_000_000L // idle
        }
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured, expected about 100 Mbit/s",
            measured > 90 * megabit && measured < 110 * megabit,
        )
    }

    @Test
    fun aWindowThatSpansAnIdleGapDoesNotReportNearZero() {
        // The regression that motivated this: the old window closed on the
        // first byte after a gap, so its span was the gap. Here the gap is
        // seven seconds and the two bursts either side are a second of actual
        // transfer at 80 Mbit/s.
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
        assertTrue("measured $measured, expected about 80 Mbit/s", measured > 70 * megabit)
    }

    @Test
    fun nothingIsReportedBeforeASecondOfTransferHasHappened() {
        // Null is the honest answer, and the server reads it as "may not be
        // offered a preparation" — which is correct for a client that has not
        // measured anything yet.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(100 * megabit, second / 4), second / 4)
        window.transferEnded(second / 4)
        assertNull(window.bitsPerSecond())
    }

    @Test
    fun concurrentTransfersCountTheLinkBusyOnce() {
        // Media3 fetches a playlist and a segment at the same time. The clock
        // must not start twice, and it must not stop when the first of the two
        // finishes while the other is still pulling.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.transferStarted(second / 10)
        window.bytesTransferred(bytesFor(50 * megabit, second), second)
        window.transferEnded(second / 2) // the playlist finished; the segment has not
        window.transferEnded(second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue(
            "measured $measured, expected about 50 Mbit/s over one busy second",
            measured > 45 * megabit && measured < 55 * megabit,
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
    }

    @Test
    fun aWindowIsNotCountedTwice() {
        // Two seconds of transfer at one rate must not read as a rate the link
        // never achieved, which is what carrying the closed window's clock
        // forward into the next one would do.
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
            measured > 35 * megabit && measured < 45 * megabit,
        )
    }

    @Test
    fun bytesWithNoStartAdoptTheTransferRatherThanDivideByNothing() {
        val window = ThroughputWindow()
        window.bytesTransferred(1_000, 0)
        window.bytesTransferred(bytesFor(20 * megabit, 2 * second), 2 * second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue("measured $measured", measured in 1..(30 * megabit))
    }

    @Test
    fun anAbsurdReadingIsClampedRatherThanInvalidatingEveryExchange() {
        // `PlaybackControlSnapshot.isValid` refuses a rate above the protocol
        // ceiling, and an invalid snapshot is dropped whole — so an
        // out-of-range reading would silently stop the client reporting
        // anything at all, which is a much larger failure than a wrong number.
        val window = ThroughputWindow()
        window.transferStarted(0)
        window.bytesTransferred(Int.MAX_VALUE, second)
        repeat(64) { window.bytesTransferred(Int.MAX_VALUE, second) }
        window.transferEnded(second)
        val measured = requireNotNull(window.bitsPerSecond())
        assertTrue("measured $measured", measured <= PlaybackControl.MAX_OBSERVED_DOWNLOAD_BPS)
    }

    @Test
    fun theTransferListenerReportsWhatTheWindowMeasured() {
        // The seam is real: `ProgressiveMediaOrigin` delegates rather than
        // keeping a second copy of this arithmetic.
        val origin = ProgressiveMediaOrigin()
        val window = origin.throughputWindowForTest()
        window.transferStarted(0)
        window.bytesTransferred(bytesFor(60 * megabit, 2 * second), 2 * second)
        window.transferEnded(2 * second)
        assertEquals(window.bitsPerSecond(), origin.currentObservedBitsPerSecond())
        origin.begin("http://server/stream.mp4", 0)
        assertNull(origin.currentObservedBitsPerSecond())
    }
}
