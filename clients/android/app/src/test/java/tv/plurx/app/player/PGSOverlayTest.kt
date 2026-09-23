package tv.plurx.app.player

import kotlinx.serialization.decodeFromString
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test
import tv.plurx.app.data.Net

class PGSOverlayTest {
    private val generation = "a".repeat(64)
    private val image = "overlay/$generation/objects/${"b".repeat(64)}.png"

    @Test
    fun manifestDecodesAndValidatesExactIdentityAndPaths() {
        val manifest = Net.json.decodeFromString<PGSOverlayManifest>(
            """{
              "schema":1,
              "generation":"$generation",
              "file_id":42,
              "track_index":3,
              "kind":"pgs",
              "timebase":"source_ms",
              "duration_ms":120000,
              "cues":[{
                "id":"c1","start_ms":1000,"end_ms":2500,
                "canvas_width":1920,"canvas_height":1080,
                "objects":[{
                  "image":"$image","x":240,"y":850,"width":1440,"height":180
                }]
              }]
            }""".trimIndent(),
        )

        assertEquals(manifest, manifest.validated(42, 3))
        assertEquals("b".repeat(64), manifest.objectHash(image))
        assertNull(manifest.objectHash("overlay/$generation/objects/../secret.png"))
        assertThrows(IllegalArgumentException::class.java) { manifest.validated(41, 3) }
    }

    @Test
    fun manifestRejectsOverflowGeometryAndReusedPathDimensionDrift() {
        val overflow = manifest(
            listOf(
                cue(
                    id = "overflow",
                    start = 1,
                    end = 2,
                    objects = listOf(
                        PGSOverlayObject(image, Int.MAX_VALUE, 0, Int.MAX_VALUE, 1),
                    ),
                ),
            ),
        )
        assertThrows(IllegalArgumentException::class.java) { overflow.validated(42, 3) }

        val drift = manifest(
            listOf(
                cue("one", 1, 2, listOf(PGSOverlayObject(image, 0, 0, 100, 50))),
                cue("two", 3, 4, listOf(PGSOverlayObject(image, 0, 0, 101, 50))),
            ),
        )
        assertThrows(IllegalArgumentException::class.java) { drift.validated(42, 3) }
    }

    @Test
    fun manifestRejectsOverlappingAndDuplicateCueStarts() {
        val first = cue("first", 1_000, 2_000)

        listOf(1_999L, 1_000L).forEach { secondStart ->
            val overlapping = manifest(
                listOf(
                    first,
                    cue("second-$secondStart", secondStart, 3_000),
                ),
            )

            assertThrows(IllegalArgumentException::class.java) {
                overlapping.validated(42, 3)
            }
        }
    }

    @Test
    fun manifestRejectsZeroLengthCue() {
        val zeroLength = manifest(listOf(cue("zero-length", 1_000, 1_000)))

        assertThrows(IllegalArgumentException::class.java) {
            zeroLength.validated(42, 3)
        }
    }

    @Test
    fun manifestAcceptsExactlyAdjacentCues() {
        val adjacent = manifest(
            listOf(
                cue("first", 1_000, 2_000),
                cue("second", 2_000, 3_000),
            ),
        )

        assertEquals(adjacent, adjacent.validated(42, 3))
    }

    @Test
    fun timelineUsesSourcePositionAndSchedulesExactBoundaries() {
        assertEquals(
            listOf(
                PGSOverlayManifestDisposition.Preparing,
                PGSOverlayManifestDisposition.Preparing,
                PGSOverlayManifestDisposition.Ready,
            ),
            listOf(503, 202, 200).map(PGSOverlayPolicy::manifestDisposition),
        )
        assertEquals(2_000, PGSOverlayPolicy.retryAfterMs("2"))
        assertEquals(1_000, PGSOverlayPolicy.retryAfterMs(null))
        val cues = listOf(
            cue("one", 90_000, 93_000),
            cue("two", 95_000, 96_500),
        )
        assertEquals(0, PGSOverlayPolicy.activeCueIndex(cues, 92_000))
        assertNull(PGSOverlayPolicy.activeCueIndex(cues, 94_000))
        assertEquals(93_000L, PGSOverlayPolicy.nextBoundaryMs(cues, 92_000))
        assertEquals(95_000L, PGSOverlayPolicy.nextBoundaryMs(cues, 94_000))
        assertEquals(
            PGSOverlayTimeWindow(90_000, 120_000),
            PGSOverlayPolicy.windowAt(95_000, 120_000),
        )
        assertFalse(PGSOverlayPolicy.shouldRefresh(95_000, PGSOverlayTimeWindow(90_000, 120_000)))
        assertTrue(PGSOverlayPolicy.shouldRefresh(101_000, PGSOverlayTimeWindow(90_000, 120_000)))
    }

    @Test
    fun decodedWindowBudgetCountsEachImmutableObjectOnce() {
        val fullCanvas = PGSOverlayObject(image, 0, 0, 4096, 2160)
        val one = cue("one", 0, 1, listOf(fullCanvas, fullCanvas))
        assertEquals(35_389_440L, PGSOverlayPolicy.decodedWindowBytes(listOf(one)))

        val twoMore = cue(
            "two",
            2,
            3,
            listOf(
                fullCanvas.copy(image = "overlay/$generation/objects/${"c".repeat(64)}.png"),
                fullCanvas.copy(image = "overlay/$generation/objects/${"d".repeat(64)}.png"),
            ),
        )
        assertNull(PGSOverlayPolicy.decodedWindowBytes(listOf(one, twoMore)))
    }

    @Test
    fun denseWindowsExposeTheOverflowCueInsteadOfSilentlyTruncating() {
        val cues = List(PGSOverlayPolicy.maximumWindowCues + 2) { index ->
            val start = index.toLong() * 10
            cue("cue-$index", start, start + 1)
        }
        val selected = PGSOverlayPolicy.cuesInWindow(
            cues,
            PGSOverlayTimeWindow(0, 90_000),
        )

        assertEquals(PGSOverlayPolicy.maximumWindowCues + 1, selected.size)
        assertTrue(selected.size > PGSOverlayPolicy.maximumWindowCues)
    }

    @Test
    fun layoutMapsAuthoredCanvasIntoActualVideoRect() {
        val object_ = PGSOverlayObject(image, 100, 800, 500, 200)
        assertEquals(
            PGSOverlayRect(200f, 1600f, 1000f, 400f),
            PGSOverlayPolicy.objectRect(
                object_,
                1920,
                1080,
                PGSOverlayPolicy.videoRect(3840f, 2160f, 16f / 9f),
            ),
        )

        val fourThreeVideo = PGSOverlayPolicy.videoRect(1024f, 768f, 16f / 9f)
        assertFloat(0f, fourThreeVideo.x)
        assertFloat(96f, fourThreeVideo.y)
        assertFloat(1024f, fourThreeVideo.width)
        assertFloat(576f, fourThreeVideo.height)
        val mapped = PGSOverlayPolicy.objectRect(object_, 1920, 1080, fourThreeVideo)
        assertFloat(53.333f, mapped.x)
        assertFloat(522.667f, mapped.y)

        val anamorphic = PGSOverlayPolicy.objectRect(
            PGSOverlayObject(image, 0, 400, 720, 80),
            720,
            480,
            PGSOverlayRect(0f, 0f, 1920f, 1080f),
        )
        assertEquals(PGSOverlayRect(150f, 900f, 1620f, 180f), anamorphic)
    }

    /**
     * `tests/playback/pgs-overlay-cases.json` is the fixture the Apple suite
     * reads too. The row that failed before this test existed is the refresh
     * margin one: a forward seek into the loaded window's last 20 s kept the
     * pre-seek cue on screen, because the refresh cleared only when the new
     * position was outside the loaded window.
     */
    @Test
    fun seekCasesFromSharedFixture() {
        val fixture = sharedFixture()
        val manifest = fixture.getValue("manifest").jsonObject
        val durationMs = manifest.getValue("duration_ms").jsonPrimitive.long
        val cues = manifest.getValue("cues").jsonArray.map {
            val cue = it.jsonObject
            cue(
                cue.getValue("id").jsonPrimitive.content,
                cue.getValue("start_ms").jsonPrimitive.long,
                cue.getValue("end_ms").jsonPrimitive.long,
            )
        }
        val cases = fixture.getValue("seek_cases").jsonArray
        assertTrue(cases.size >= 6)
        cases.forEach { element ->
            val case = element.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            val expect = case.getValue("expect").jsonObject
            val plan = PGSOverlayPolicy.seekPlan(
                positionMs = case.getValue("to_ms").jsonPrimitive.long,
                loadedWindow = window(case.getValue("loaded_window")),
                shownCueId = case.getValue("shown_cue").stringOrNull(),
                cues = cues,
                durationMs = durationMs,
            )
            assertEquals(name, expect.getValue("refresh").jsonPrimitive.boolean, plan.refresh)
            assertEquals(name, expect.getValue("clear_now").jsonPrimitive.boolean, plan.clearNow)
            assertEquals(name, expect.getValue("active_cue").stringOrNull(), plan.activeCueId)
            assertEquals(name, window(expect.getValue("window")), plan.window)
        }
    }

    /**
     * The manifest answers both clients must read the same way. The rows that
     * matter are the typed ones: a remembered preparation failure has to stop
     * the ten-minute poll and say so, and capacity has to keep it going.
     */
    @Test
    fun manifestResponsesFromSharedFixture() {
        val cases = sharedFixture().getValue("manifest_responses").jsonArray
        assertTrue(cases.size >= 6)
        cases.forEach { element ->
            val case = element.jsonObject
            val name = case.getValue("name").jsonPrimitive.content
            val status = case.getValue("status").jsonPrimitive.int
            val retryAfter = case.getValue("retry_after").stringOrNull()
            val body = case.getValue("body").toString()
            val expect = case.getValue("expect").jsonObject
            val disposition = expect.getValue("disposition").jsonPrimitive.content
            val expectedRetry = expect["retry_after_ms"]?.jsonPrimitive?.int
            val expectedNotice = expect["notice"]?.jsonPrimitive?.content

            if (status in 200..299) {
                val actual = PGSOverlayPolicy.manifestDisposition(status)
                assertEquals(name, disposition, actual.name.lowercase())
                when (actual) {
                    PGSOverlayManifestDisposition.Ready -> Net.json
                        .decodeFromString<PGSOverlayManifest>(body)
                        .validated(42, 3)
                    PGSOverlayManifestDisposition.Preparing -> assertEquals(
                        name,
                        expectedRetry,
                        Net.json.decodeFromString<PGSOverlayPreparing>(body).retryAfterMs,
                    )
                    PGSOverlayManifestDisposition.Terminal -> Unit
                }
                return@forEach
            }

            when (val refusal = PGSOverlayPolicy.manifestRefusal(status, retryAfter, body)) {
                is PGSOverlayRefusal.Wait -> {
                    assertEquals(name, "preparing", disposition)
                    assertEquals(name, expectedRetry, refusal.retryAfterMs)
                }
                is PGSOverlayRefusal.Terminal -> {
                    assertEquals(name, "terminal", disposition)
                    val notice = PGSOverlayPolicy.failureNotice(refusal.message)
                    if (expectedNotice != null) {
                        assertEquals(name, expectedNotice, notice)
                    } else {
                        assertFalse(name, notice.contains("empty"))
                    }
                }
            }
        }
    }

    private fun sharedFixture(): JsonObject {
        val resource = checkNotNull(javaClass.classLoader?.getResource("pgs-overlay-cases.json")) {
            "tests/playback/pgs-overlay-cases.json is not on the JVM test classpath"
        }
        return Net.json.parseToJsonElement(resource.readText()).jsonObject
    }

    private fun window(element: JsonElement): PGSOverlayTimeWindow? {
        if (element is JsonNull) return null
        val bounds = element as JsonArray
        return PGSOverlayTimeWindow(bounds[0].jsonPrimitive.long, bounds[1].jsonPrimitive.long)
    }

    private fun JsonElement.stringOrNull(): String? =
        if (this is JsonNull) null else jsonPrimitive.content

    private fun manifest(cues: List<PGSOverlayCue>) = PGSOverlayManifest(
        schema = 1,
        generation = generation,
        fileId = 42,
        trackIndex = 3,
        kind = "pgs",
        timebase = "source_ms",
        durationMs = 120_000,
        cues = cues,
    )

    private fun cue(
        id: String,
        start: Long,
        end: Long,
        objects: List<PGSOverlayObject> = emptyList(),
    ) = PGSOverlayCue(id, start, end, 1920, 1080, objects)

    private fun assertFloat(expected: Float, actual: Float) {
        assertEquals(expected.toDouble(), actual.toDouble(), 0.01)
    }
}
