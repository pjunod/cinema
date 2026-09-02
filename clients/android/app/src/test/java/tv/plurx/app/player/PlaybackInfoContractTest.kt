package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Test
import tv.plurx.app.data.PlaybackSessionStatus

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
        buffer = "12.3 s",
        frames = "2 / 4,380 frames",
        sourceFile = "fixture.mkv",
        sourceVideo = "HEVC · Main 10 · 10-bit · HDR10",
        sourceResolution = "3840×2160",
        sourceBitrate = "48 Mb/s",
        container = "MKV",
        sourceAudio = "TRUEHD · 7.1 · English",
        playingVideo = "HEVC · 1920×1080",
        decodeResolution = "1920×1080",
        playingAudio = "AAC · Stereo · English",
        dynamicRange = "HDR10",
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
            speed = 1.25,
            recent_speed = 1.20,
            out_time_ms = 90_000,
            published_end_ms = 90_000,
            fetched_end_ms = 88_000,
            playlist_shape = "VOD",
            ahead_seconds = 12,
            ahead_bytes = 8_600_000,
            hold_reason = "runway",
            delivered_bytes = 86_000_000,
            delivered_bps = 12_300_000,
            delivered_idle_ms = 250,
            readrate = 1.05,
            suspended = true,
            suspend_count = 2,
            last_request = "segment 12",
            idle_seconds = 1,
        ),
        playerState = "Playing",
        control = "node-a · active",
    )

    @Test
    fun rowsMatchTheSharedAndroidFieldListInEveryMode() {
        PlaybackStatsMode.entries.forEach { mode ->
            val expected = fixture.getValue("fields").jsonArray.map { it.jsonObject }
                .filter { field ->
                    mode.storageValue in field.getValue("modes").jsonArray.map {
                        it.jsonPrimitive.content
                    } && field["available_on"]?.jsonArray?.map {
                        it.jsonPrimitive.content
                    }?.let { "android" in it } != false
                }
                .map { it.getValue("label").jsonPrimitive.content }
            val actual = playbackInfoRows(
                details,
                listOf("fixture reason"),
            ).filter { mode in it.modes && it.value != null }.map(InfoRow::label)

            assertEquals(mode.label, expected, actual)
        }
    }

    @Test
    fun nativeUnitsMatchTheSharedVocabulary() {
        assertEquals("800 kb/s", formatBitrate(800_000))
        assertEquals("9.5 Mb/s", formatBitrate(9_500_000))
        assertEquals("12 Mb/s", formatBitrate(12_300_000))
    }
}
