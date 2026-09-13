package tv.plurx.app.livetv

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The sentences the DVR surfaces put in front of a viewer. Each one is a claim
 * about tuners, files or time, and each is computed from rows this client
 * already has rather than from a route that could disagree with them.
 */
class DvrUiTest {
    private val start = 1_789_000_800L
    private val programme = LiveTvProgramme(start, start + 1_800, "Kitchen Table")

    private fun recording(
        state: String,
        captureStart: Long,
        captureEnd: Long,
        id: String = "row-$captureStart",
    ) = DvrRecording(
        id = id,
        channel_id = "5.1",
        airing_start = captureStart,
        airing_end = captureEnd,
        capture_start = captureStart,
        capture_end = captureEnd,
        title = "Something Else",
        state = state,
    )

    private fun status(max: Int = 2, reserve: Int = 1, enabled: Boolean = true) = DvrStatus(
        enabled = enabled,
        slots = DvrSlots(max = max, reserve = reserve, recording = 0),
    )

    @Test
    fun theTunerLineCountsOnlyWhatOverlapsThisProgramme() {
        val dvr = DvrScreenState(
            status = status(),
            schedule = listOf(
                recording("scheduled", start - 600, start + 600),
                recording("recording", start + 600, start + 3_600),
                // Ends exactly as the programme begins: a half-open window, so
                // it is not competing for a tuner during it.
                recording("scheduled", start - 3_600, start, id = "earlier"),
                // A cancelled row has no claim on a tuner at all.
                recording("cancelled", start, start + 1_800, id = "skipped"),
            ),
        )
        assertEquals(
            "2 tuners · 2 recording then · none free · 1 kept for viewers",
            dvrTunerLine(dvr, programme),
        )
        assertEquals(
            "4 tuners · 2 recording then · 2 free · 1 kept for viewers",
            dvrTunerLine(dvr.copy(status = status(max = 4)), programme),
        )
    }

    @Test
    fun theTunerLineSaysSoWhenThereIsNothingToSay() {
        assertEquals("Recording status unavailable", dvrTunerLine(DvrScreenState(), programme))
        assertEquals(
            "Recording is switched off",
            dvrTunerLine(DvrScreenState(status = status(enabled = false)), programme),
        )
    }

    @Test
    fun aPartialCaptureNeverPresentsAsACleanOne() {
        val partial = DvrRecording(
            id = "p", channel_id = "7.1", guide_number = "7.1",
            airing_start = start, airing_end = start + 1_800,
            capture_start = start - 60, capture_end = start + 1_920,
            title = "Kitchen Table", state = "partial", gap_s = 120, late_start_s = 9,
        )
        val detail = dvrRecordingDetail(partial, start)
        assertTrue(detail, detail.contains("Partial"))
        assertTrue("the gap is the whole difference from `done`", detail.contains("2 min gap"))
        assertTrue(detail.contains("started 9 s late"))

        val failed = partial.copy(state = "failed", state_reason = "the tuner stopped answering")
        assertTrue(dvrRecordingDetail(failed, start).contains("the tuner stopped answering"))

        val running = partial.copy(state = "recording")
        assertTrue(dvrRecordingDetail(running, start).contains("Recording"))
        assertTrue(dvrRecordingDetail(running, start).contains("32 min left"))
    }

    @Test
    fun theHolderLineNamesWhoToStopAndSaysWhenStoppingIsPointless() {
        val one = DvrHolder(
            channel_id = "5.1", guide_number = "5.1", channel_name = "WNYW",
            sinks = listOf(DvrHolderSink("r1", "Kitchen Table", start + 1_800)),
        )
        assertTrue(dvrHolderLine(one).startsWith("5.1 WNYW · Kitchen Table until "))
        assertEquals("r1", one.stoppable?.recording_id)

        val shared = one.copy(
            sinks = one.sinks + DvrHolderSink("r2", "The Nine", start + 3_600),
        )
        assertEquals(
            "stopping one of two recordings on one transport frees no tuner",
            null,
            shared.stoppable,
        )
    }

    @Test
    fun theReminderCountdownReadsTheWayAViewerWouldSayIt() {
        val reminder = DvrReminder(
            id = "r", channel_id = "7.1", guide_number = "7.1",
            airing_start = start, airing_end = start + 1_800,
            title = "Kitchen Table", lead_s = 300,
        )
        assertTrue(reminderCountdown(reminder, start - 240).startsWith("Starts in 4 min · "))
        assertTrue(reminderCountdown(reminder, start - 30).startsWith("Starts in under a minute"))
        assertEquals("Starting now on 7.1", reminderCountdown(reminder, start))
        assertEquals("Starting now on 7.1", reminderCountdown(reminder, start + 60))

        assertEquals(0f, reminderProgress(reminder, start - 300), 1e-6f)
        assertEquals(0.5f, reminderProgress(reminder, start - 150), 1e-6f)
        assertEquals(1f, reminderProgress(reminder, start + 999), 1e-6f)
        // A reminder with no lead at all is already at its start, and must not
        // divide by the zero seconds between the two.
        assertEquals(1f, reminderProgress(reminder.copy(lead_s = 0), start - 10), 1e-6f)
    }

    @Test
    fun aRuleSaysWhichExactnessItActuallyHas() {
        val byId = DvrRule(id = "1", name = "Kitchen Table", match_mode = "series_id", match_value = "EP123")
        assertEquals("Series id · any channel", byId.matchSummary)
        val byTitle = DvrRule(
            id = "2", name = "Kitchen Table", match_mode = "title",
            match_value = "kitchen table", channel_id = "7.1",
        )
        assertEquals("Title match · 7.1 only", byTitle.matchSummary)
        assertEquals("Title match · any channel", byTitle.copy(channel_id = null).matchSummary)
    }

    /** Each edit names exactly the fields it means to change, and no others. */
    @Test
    fun aRuleEditSendsOneFieldSet() {
        assertEquals(setOf("enabled"), DvrRuleChange.Enabled(false).body().keys)
        assertEquals(setOf("new_only"), DvrRuleChange.NewOnly(true).body().keys)
        assertEquals(setOf("keep_mode", "keep_value"), DvrRuleChange.Keep("last_n", 5).body().keys)
        assertEquals(setOf("pad_start_s", "pad_end_s"), DvrRuleChange.Padding(60, 120).body().keys)
        assertEquals(setOf("channel_id"), DvrRuleChange.Channel("7.1").body().keys)
        assertEquals(
            "an explicit any-channel is a different request from an absent channel",
            setOf("any_channel"),
            DvrRuleChange.Channel(null).body().keys,
        )
    }

    @Test
    fun theReorderBodyCarriesTheIdsAsJsonStringsInOrder() {
        val body = dvrReorderBody(listOf("r-2", "r-1", "r-3"))
        assertEquals(setOf("ids"), body.keys)
        val ids = body["ids"] as JsonArray
        assertEquals(
            "the order is the request - a set would lose the whole point",
            listOf("r-2", "r-1", "r-3"),
            ids.map { it.jsonPrimitive.content },
        )
        assertTrue("every id is a JSON string", ids.all { it is JsonPrimitive && it.isString })
        assertEquals("an empty reorder is still a well-formed body", 0, (dvrReorderBody(emptyList())["ids"] as JsonArray).size)
    }

    @Test
    fun everyTypedRefusalHasItsOwnSentence() {
        val codes = listOf(
            "dvr_disabled", "airing_unknown", "airing_past", "rule_limit",
            "reminder_limit", "delete_file_required", "tuner_capacity",
        )
        val messages = codes.map(::dvrMessage)
        assertEquals("no two refusals may read the same", messages.size, messages.toSet().size)
        assertTrue(messages.none { it == dvrMessage("something_new_from_a_newer_server") })
    }
}
