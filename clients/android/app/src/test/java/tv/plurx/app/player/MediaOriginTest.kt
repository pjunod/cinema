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
