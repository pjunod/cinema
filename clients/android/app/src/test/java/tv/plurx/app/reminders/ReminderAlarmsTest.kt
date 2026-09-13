package tv.plurx.app.reminders

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import tv.plurx.app.livetv.DvrReminder

/**
 * The mirror is a reconciliation, not an append. Every case here is one an
 * append-only mirror gets wrong, and each of them is a phone announcing a
 * programme nobody asked about — or staying silent about one they did.
 */
class ReminderAlarmsTest {
    private val now = 1_789_000_000L

    private fun alarm(id: String, fireAt: Long) = ReminderAlarm(
        id = id,
        channelId = "7.1",
        title = "Kitchen Table",
        airingStart = fireAt + 300,
        fireAt = fireAt,
    )

    @Test
    fun anUnchangedReminderIsLeftExactlyAsItIs() {
        val plan = reconcileReminderAlarms(
            armed = listOf(alarm("a", now + 600)),
            scheduled = mapOf("a" to now + 600),
            now = now,
        )
        assertTrue("re-setting an unchanged alarm is work with no observer", plan.empty)
    }

    @Test
    fun aReminderDeletedElsewhereLosesItsAlarm() {
        val plan = reconcileReminderAlarms(
            armed = emptyList(),
            scheduled = mapOf("a" to now + 600),
            now = now,
        )
        assertEquals(listOf("a"), plan.cancel)
        assertTrue(plan.schedule.isEmpty())
    }

    @Test
    fun aReminderCreatedElsewhereGainsOne() {
        val plan = reconcileReminderAlarms(
            armed = listOf(alarm("a", now + 600)),
            scheduled = emptyMap(),
            now = now,
        )
        assertTrue(plan.cancel.isEmpty())
        assertEquals(listOf("a"), plan.schedule.map { it.id })
    }

    @Test
    fun aProgrammeThatMovedIsCancelledAndSetAgainRatherThanLeftAtTheOldTime() {
        val plan = reconcileReminderAlarms(
            armed = listOf(alarm("a", now + 900)),
            scheduled = mapOf("a" to now + 600),
            now = now,
        )
        assertEquals(listOf("a"), plan.cancel)
        assertEquals(listOf(now + 900), plan.schedule.map { it.fireAt })
    }

    @Test
    fun aMomentThatHasAlreadyPassedIsNeverScheduledAndIsTidiedAway() {
        val plan = reconcileReminderAlarms(
            armed = listOf(alarm("past", now - 60), alarm("soon", now + 60)),
            scheduled = mapOf("past" to now - 60),
            now = now,
        )
        assertEquals(
            "an alarm set in the past fires at once on some Android versions",
            listOf("past"),
            plan.cancel,
        )
        assertEquals(listOf("soon"), plan.schedule.map { it.id })
    }

    @Test
    fun thePlanIsOrderedSoTwoRunsOverTheSameFactsAreTheSamePlan() {
        val armed = listOf(alarm("z", now + 300), alarm("a", now + 300), alarm("m", now + 100))
        val plan = reconcileReminderAlarms(
            armed = armed,
            scheduled = mapOf("q" to now + 10, "b" to now + 20),
            now = now,
        )
        assertEquals(listOf("b", "q"), plan.cancel)
        assertEquals(listOf("m", "a", "z"), plan.schedule.map { it.id })
        assertEquals(plan, reconcileReminderAlarms(armed.reversed(), mapOf("b" to now + 20, "q" to now + 10), now))
    }

    /**
     * Only `armed` reminders become alarms, and an alarm is due one lead
     * before the airing rather than at it.
     */
    @Test
    fun onlyArmedServerRowsBecomeAlarms() {
        val rows = listOf("armed", "fired", "acked", "expired", "moved").map { state ->
            DvrReminder(
                id = state,
                channel_id = "7.1",
                airing_start = now + 900,
                airing_end = now + 2_700,
                title = "Kitchen Table",
                lead_s = 300,
                state = state,
            )
        }
        val armed = ReminderAlarms.armedAlarms(rows)
        assertEquals(listOf("armed"), armed.map { it.id })
        assertEquals(now + 600, armed.single().fireAt)
        assertEquals("Kitchen Table", armed.single().title)
        assertEquals("7.1", armed.single().channelId)
    }
}
