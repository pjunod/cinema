package tv.plurx.app.livetv

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.data.Net

/**
 * One schedule row, one glyph. These are the cases a viewer reads off the grid
 * without opening anything, so getting one wrong means the guide states
 * something untrue about what the DVR is going to do.
 */
class DvrMarksTest {
    private fun row(
        state: String,
        start: Long = 1_789_000_800,
        channel: String = "7.1",
        ruleId: String? = null,
        captureStart: Long = start - 60,
        captureEnd: Long = start + 1_920,
    ) = DvrRecording(
        id = "row-$state-$channel-$start",
        rule_id = ruleId,
        channel_id = channel,
        airing_start = start,
        airing_end = start + 1_800,
        capture_start = captureStart,
        capture_end = captureEnd,
        title = "Kitchen Table",
        state = state,
    )

    private fun reminder(
        state: String = "armed",
        start: Long = 1_789_000_800,
        channel: String = "7.1",
    ) = DvrReminder(
        id = "reminder-$channel-$start",
        channel_id = channel,
        airing_start = start,
        airing_end = start + 1_800,
        title = "Kitchen Table",
        lead_s = 300,
        state = state,
    )

    @Test
    fun everyStateSelectsTheGlyphTheContractNames() {
        fun shapeOf(state: String, ruleId: String? = null): DvrMarkShape? =
            DvrGuideMarks(listOf(row(state, ruleId = ruleId)))
                .mark("7.1", 1_789_000_800, 1_789_000_000)
                .shape

        assertEquals(DvrMarkShape.Scheduled, shapeOf("scheduled"))
        assertEquals(
            "a rule row is two dots, so a series is legible without opening it",
            DvrMarkShape.Series,
            shapeOf("scheduled", ruleId = "rule-1"),
        )
        assertEquals(DvrMarkShape.Conflict, shapeOf("conflict"))
        assertEquals(DvrMarkShape.Withdrawn, shapeOf("withdrawn"))
        assertEquals(DvrMarkShape.Withdrawn, shapeOf("stale"))
        assertEquals(DvrMarkShape.Recording, shapeOf("recording"))
        // A skipped episode draws nothing: `cancelled` is the viewer's own
        // decision, and a dot would say the opposite of what they chose. The
        // row is still carried, because Scheduled has to offer Restore.
        assertNull(shapeOf("cancelled"))
        assertNull(shapeOf("done"))
        assertNull(shapeOf("deleted"))
        // A state this build has never heard of decodes and simply draws no
        // glyph, rather than discarding the row or guessing at it.
        assertNull(shapeOf("quarantined"))
    }

    @Test
    fun aRecordingCarriesItsCaptureProgressAndNothingElseDoes() {
        val marks = DvrGuideMarks(
            listOf(
                row("recording", captureStart = 1_000, captureEnd = 2_000),
                row("scheduled", channel = "5.1", captureStart = 1_000, captureEnd = 2_000),
            ),
        )
        val recording = marks.mark("7.1", 1_789_000_800, 1_500)
        assertEquals(0.5f, recording.progress!!, 1e-6f)
        assertNull(marks.mark("5.1", 1_789_000_800, 1_500).progress)
        // The underline never runs past the cell, however late the owner's
        // clock and this client's disagree.
        assertEquals(1f, marks.mark("7.1", 1_789_000_800, 9_999).progress!!, 1e-6f)
        assertEquals(0f, marks.mark("7.1", 1_789_000_800, 0).progress!!, 1e-6f)
    }

    @Test
    fun aBellIsIndependentOfWhateverTheScheduleSays() {
        val marks = DvrGuideMarks(listOf(row("recording")), listOf(reminder()))
        val both = marks.mark("7.1", 1_789_000_800, 1_789_000_900)
        assertEquals(DvrMarkShape.Recording, both.shape)
        assertTrue("a recorded programme can still be one the viewer wants told about", both.bell)

        val bellOnly = DvrGuideMarks(reminders = listOf(reminder()))
            .mark("7.1", 1_789_000_800, 0)
        assertNull(bellOnly.shape)
        assertTrue(bellOnly.drawn)

        // Acked and expired reminders are ones the viewer is done with.
        listOf("acked", "expired", "moved").forEach { state ->
            assertFalse(
                state,
                DvrGuideMarks(reminders = listOf(reminder(state = state)))
                    .mark("7.1", 1_789_000_800, 0).bell,
            )
        }
        assertTrue(
            "a fired reminder is still waiting to be dismissed",
            DvrGuideMarks(reminders = listOf(reminder(state = "fired")))
                .mark("7.1", 1_789_000_800, 0).bell,
        )
    }

    @Test
    fun aMarkBelongsToOneAiringAndNeverToItsNeighbourOrItsChannelTwin() {
        val marks = DvrGuideMarks(listOf(row("scheduled")), listOf(reminder()))
        assertTrue(marks.mark("7.1", 1_789_000_800, 0).drawn)
        assertFalse("the next programme on the same channel", marks.mark("7.1", 1_789_002_600, 0).drawn)
        assertFalse("the same instant on another channel", marks.mark("5.1", 1_789_000_800, 0).drawn)
        assertEquals(DvrCellMark.NONE, DvrGuideMarks.EMPTY.mark("7.1", 1_789_000_800, 0))
        assertFalse(DvrCellMark.NONE.drawn)
    }

    @Test
    fun theSpokenDescriptionSaysTheMeaningRatherThanTheShape() {
        assertEquals(
            "Scheduled by a series rule, Reminder set",
            liveTvCellMarkDescription(DvrCellMark(DvrMarkShape.Series, bell = true)),
        )
        assertEquals(
            "Recording now",
            liveTvCellMarkDescription(DvrCellMark(DvrMarkShape.Recording, progress = 0.4f)),
        )
        assertEquals("Reminder set", liveTvCellMarkDescription(DvrCellMark(bell = true)))
        assertEquals("", liveTvCellMarkDescription(DvrCellMark.NONE))
    }

    /**
     * The wire shapes this client dispatches on. `Net.json` ignores unknown
     * keys, which is what lets the server grow a column without breaking an
     * older build — and what must not quietly turn a missing `rule_id` into a
     * series mark.
     */
    @Test
    fun theRecordingWireShapeDecodesTheFieldsTheMarksAreMadeOf() {
        val decoded: DvrRecording = Net.json.decodeFromString(
            """
            {"id":"abc","origin":"rule","rule_id":"rule-1","requested_by_user_id":null,
             "channel_id":"7.1","guide_number":"7.1","channel_name":"WABC",
             "airing_start":1789000800,"airing_end":1789002600,
             "capture_start":1789000740,"capture_end":1789002720,
             "title":"Kitchen Table","episode":"S3E14","state":"scheduled",
             "attempt":0,"gap_s":0,"late_start_s":0,"bytes":0,
             "tuner_owner_node_id":"nynuc","path":"/20t/dvr/x.ts",
             "created_at_ms":1,"updated_at_ms":2}
            """.trimIndent(),
        )
        assertEquals("rule-1", decoded.rule_id)
        assertTrue(decoded.pending)
        assertFalse(decoded.hasMedia)
        assertEquals(
            DvrMarkShape.Series,
            DvrGuideMarks(listOf(decoded)).mark("7.1", 1_789_000_800, 0).shape,
        )

        val due: DvrReminder = Net.json.decodeFromString(
            """
            {"id":"r1","user_id":3,"channel_id":"7.1","guide_number":"7.1",
             "airing_start":1789000800,"airing_end":1789002600,"title":"Kitchen Table",
             "lead_s":300,"state":"fired","covered_by_recording":true,
             "created_at_ms":1,"updated_at_ms":2}
            """.trimIndent(),
        )
        assertTrue(due.covered_by_recording)
        assertEquals(1_789_000_500L, due.fireAt)
    }
}
