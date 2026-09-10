package tv.plurx.app.livetv

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.boolean
import kotlinx.serialization.json.double
import kotlinx.serialization.json.float
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.long
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Net

/**
 * `tests/playback/live-tv-guide-cases.json` is the same fixture the web and
 * Apple suites read. Three reducers, one truth: if this file and
 * `tests/web/live-tv.test.js` disagree, one of the clients is lying about what
 * is on.
 */
class LiveTvGuideTest {
    private val fixture = checkNotNull(
        javaClass.classLoader?.getResource("live-tv-guide-cases.json"),
    ) { "tests/playback/live-tv-guide-cases.json is not on the JVM test classpath" }
        .readText()

    private val cases: JsonObject = Json.parseToJsonElement(fixture).jsonObject
    private val guide: LiveTvGuide = Net.json.decodeFromString(cases.getValue("guide").toString())
    private val lineup: List<LiveTvChannel> =
        Net.json.decodeFromString(cases.getValue("lineup").toString())

    @Test
    fun airingAnswersEverySharedNowNextProgressCase() {
        val rows = cases.getValue("programme_at").jsonArray
        assertTrue("the fixture lost its airing cases", rows.isNotEmpty())
        rows.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("name").jsonPrimitive.content
            val expect = row.getValue("expect").jsonObject
            val answer = LiveTvGuideReducer.airing(
                guide,
                row.getValue("channel").jsonPrimitive.content,
                row.getValue("now").jsonPrimitive.long,
            )
            assertEquals(name, expect["now_title"]?.jsonPrimitive?.contentOrNullSafe(), answer.now?.title)
            assertEquals(name, expect["next_title"]?.jsonPrimitive?.contentOrNullSafe(), answer.next?.title)
            val wanted = expect["progress"]?.jsonPrimitive?.doubleOrNullSafe()
            if (wanted == null) {
                assertNull(name, answer.progress)
            } else {
                assertEquals(name, wanted, (answer.progress ?: Float.NaN).toDouble(), 1e-6)
            }
        }
    }

    @Test
    fun gridLayoutPlacesEveryCellWhereTheSharedCasesSay() {
        val grid = cases.getValue("grid").jsonObject
        val window = LiveTvGuideWindow(
            grid.getValue("window").jsonObject.getValue("start").jsonPrimitive.long,
            grid.getValue("window").jsonObject.getValue("end").jsonPrimitive.long,
        )
        val now = grid.getValue("now").jsonPrimitive.long
        val expect = grid.getValue("expect").jsonObject
        val layout = LiveTvGuideReducer.gridLayout(
            guide = guide,
            channels = lineup,
            window = window,
            now = now,
            slotSeconds = grid.getValue("slot_seconds").jsonPrimitive.long,
            pxPerSlot = grid.getValue("px_per_slot").jsonPrimitive.float,
        )
        assertEquals(expect.getValue("now_line_x").jsonPrimitive.float, layout.nowX!!, 1e-4f)
        assertEquals(expect.getValue("total_width").jsonPrimitive.float, layout.totalWidth, 1e-4f)
        assertEquals("a channel never loses its row", lineup.size, layout.rows.size)
        expect.getValue("rows").jsonArray.forEach { element ->
            val wanted = element.jsonObject
            val channelId = wanted.getValue("channel").jsonPrimitive.content
            val row = layout.rows.first { it.channel.id == channelId }
            val cells = wanted.getValue("cells").jsonArray
            assertEquals(channelId, cells.size, row.cells.size)
            cells.forEachIndexed { index, cellElement ->
                val cell = cellElement.jsonObject
                val actual = row.cells[index]
                val where = "$channelId/${cell.getValue("title").jsonPrimitive.content}"
                assertEquals(where, cell.getValue("title").jsonPrimitive.content, actual.programme.title)
                assertEquals(where, cell.getValue("left").jsonPrimitive.float, actual.left, 1e-4f)
                assertEquals(where, cell.getValue("width").jsonPrimitive.float, actual.width, 1e-4f)
                assertEquals(where, cell.getValue("airing").jsonPrimitive.boolean, actual.airing)
                assertEquals(where, cell.getValue("clipped").jsonPrimitive.boolean, actual.clipped)
            }
            // Geometry the fixture cannot state row by row: cells advance and
            // never overlap, so a grid can never draw two programmes over one
            // another.
            var edge = -Float.MAX_VALUE
            row.cells.forEach { cell ->
                assertTrue("$channelId: cells overlap", cell.left >= edge - 1e-4f)
                assertTrue("$channelId: a zero-width cell", cell.width > 0f)
                edge = cell.left + cell.width
            }
        }
        // A lineup channel the guide does not carry keeps its row, empty.
        assertTrue(layout.rows.first { it.channel.id == "68.1" }.cells.isEmpty())
        assertEquals(4, LiveTvGuideReducer.gridSlots(window, LiveTvGuideReducer.SLOT_SECONDS).size)
        assertNotNull(LiveTvGuideReducer.guideEnds(guide, "11.1", window))
        assertNull(LiveTvGuideReducer.guideEnds(guide, "7.1", window))
    }

    @Test
    fun filteringAndChannelAdjacencyAnswerTheSharedCases() {
        val now = cases.getValue("grid").jsonObject.getValue("now").jsonPrimitive.long
        cases.getValue("filters").jsonArray.forEach { element ->
            val row = element.jsonObject
            val name = row.getValue("name").jsonPrimitive.content
            val opts = row.getValue("opts").jsonObject
            val visible = LiveTvGuideReducer.filter(
                channels = lineup,
                guide = guide,
                options = LiveTvChannelFilter(
                    query = opts.getValue("query").jsonPrimitive.content,
                    favoritesOnly = opts.getValue("filter").jsonPrimitive.content == "favorites",
                    hideProtected = opts.getValue("hide_protected").jsonPrimitive.boolean,
                ),
                now = now,
            )
            assertEquals(
                name,
                row.getValue("expect").jsonArray.map { it.jsonPrimitive.content },
                visible.map { it.id },
            )
        }
        cases.getValue("adjacent").jsonArray.forEach { element ->
            val row = element.jsonObject
            assertEquals(
                row.getValue("name").jsonPrimitive.content,
                row.getValue("expect").jsonPrimitive.contentOrNullSafe(),
                LiveTvGuideReducer.adjacent(
                    row.getValue("visible").jsonArray.map { it.jsonPrimitive.content },
                    row.getValue("current").jsonPrimitive.content,
                    row.getValue("delta").jsonPrimitive.int,
                ),
            )
        }
    }

    @Test
    fun aGuideThatIsOffOrEmptyIsARenderedStateRatherThanAFailure() {
        val empty = LiveTvGuide()
        assertEquals(false, empty.hasProgrammes)
        assertEquals(LiveTvAiring(), LiveTvGuideReducer.airing(empty, "7.1", 1789000800))
        assertEquals(LiveTvAiring(), LiveTvGuideReducer.airing(null, "7.1", 0))
        // Every channel still renders, with a row and no cells, and filtering
        // still works with no guide behind it.
        val window = LiveTvGuideReducer.window(1789000800)
        val layout = LiveTvGuideReducer.gridLayout(empty, lineup, window, 1789000800, pxPerSlot = 240f)
        assertEquals(lineup.size, layout.rows.size)
        assertTrue(layout.rows.all { it.cells.isEmpty() })
        assertEquals(
            listOf("7.1"),
            LiveTvGuideReducer
                .filter(lineup, empty, LiveTvChannelFilter(query = "wabc"), 0)
                .map { it.id },
        )
    }

    @Test
    fun verticalGuideFocusKeepsItsUtcAnchorAcrossDifferentProgrammeLengths() {
        fun cell(start: Long, end: Long, title: String) = LiveTvGridCell(
            LiveTvProgramme(start, end, title), 0f, 100f, airing = false, clipped = false,
        )
        val layout = LiveTvGridLayout(
            rows = listOf(
                LiveTvGridRow(lineup[0], listOf(cell(0, 3_600, "Hour"))),
                LiveTvGridRow(lineup[1], listOf(cell(0, 1_800, "Early"), cell(1_800, 2_700, "Short"), cell(2_700, 5_400, "Long"))),
                LiveTvGridRow(lineup[2], listOf(cell(0, 1_200, "A"), cell(1_200, 2_400, "B"), cell(2_400, 3_600, "C"))),
            ),
            totalWidth = 480f,
            nowX = null,
        )
        val first = LiveTvGuideReducer.moveGuideFocus(
            layout,
            LiveTvGuideFocusTarget(lineup[0].id, 0),
            anchorTime = 3_000,
            direction = LiveTvGuideFocusDirection.Down,
        )
        assertEquals(2_700, first.target?.programmeStart)
        val second = LiveTvGuideReducer.moveGuideFocus(
            layout,
            checkNotNull(first.target),
            anchorTime = first.anchorTime,
            direction = LiveTvGuideFocusDirection.Down,
        )
        assertEquals(2_400, second.target?.programmeStart)
        assertEquals(3_000, second.anchorTime)
    }

    @Test
    fun guideFocusReachesHeadersEmptyRowsAndTheToolbarBoundary() {
        val layout = LiveTvGridLayout(
            rows = listOf(
                LiveTvGridRow(
                    lineup[0],
                    listOf(LiveTvGridCell(LiveTvProgramme(0, 1_800, "Now"), 0f, 100f, false, false)),
                ),
                LiveTvGridRow(lineup[1], emptyList()),
            ),
            totalWidth = 320f,
            nowX = null,
        )
        val cell = LiveTvGuideFocusTarget(lineup[0].id, 0)
        val header = LiveTvGuideReducer.moveGuideFocus(
            layout, cell, 900, LiveTvGuideFocusDirection.Left,
        )
        assertEquals(true, header.target?.channelHeader)
        val empty = LiveTvGuideReducer.moveGuideFocus(
            layout, cell, 900, LiveTvGuideFocusDirection.Down,
        )
        assertEquals(lineup[1].id, empty.target?.channelId)
        assertEquals(null, empty.target?.programmeStart)
        assertEquals(false, empty.target?.channelHeader)
        assertTrue(
            LiveTvGuideReducer.moveGuideFocus(
                layout, cell, 900, LiveTvGuideFocusDirection.Up,
            ).toolbarBoundary,
        )
    }

    @Test
    fun theGuideWireShapeIgnoresFieldsThisClientShouldNotSee() {
        // `Net.json` ignores unknown keys, which is what lets a server grow a
        // field without breaking an older build — and what keeps a credential
        // the server must never send from becoming a field here if it did.
        val decoded: LiveTvGuide = Net.json.decodeFromString(
            """
            {"source":"hdhomerun","freshness":"fresh","age_seconds":412,"fetched_at":1789000000,
             "window":{"start":1788996400,"end":1789086400},"refresh_error":null,
             "matched_channels":12,"lineup_channels":12,"device_auth":"MUST-NOT-BE-READ",
             "channels":[{"id":"7.1","guide_number":"7.1","affiliate":"ABC",
               "programmes":[{"start":1789000800,"end":1789002600,"title":"City Beat",
                 "episode_title":"Pier 40","episode":"S3E14","filters":["News"]}]}]}
            """.trimIndent(),
        )
        assertEquals(12, decoded.matched_channels)
        assertEquals("ABC", decoded.channels.first().affiliate)
        assertEquals("Pier 40", decoded.channels.first().programmes.first().episode_title)
        assertTrue(decoded.hasProgrammes)
    }
}

private fun kotlinx.serialization.json.JsonPrimitive.contentOrNullSafe(): String? =
    if (this is kotlinx.serialization.json.JsonNull) null else content

private fun kotlinx.serialization.json.JsonPrimitive.doubleOrNullSafe(): Double? =
    if (this is kotlinx.serialization.json.JsonNull) null else double
