package tv.plurx.app.reminders

import android.app.AlarmManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import kotlinx.coroutines.flow.first
import tv.plurx.app.data.SettingsStore
import tv.plurx.app.livetv.DvrApi
import tv.plurx.app.livetv.DvrReminder

/**
 * One reminder as the platform scheduler needs to know it.
 *
 * Deliberately not [DvrReminder]: the reconciliation is a pure function over
 * ids and instants, and giving it the wire model would make it a function over
 * whatever the server happens to send next.
 */
data class ReminderAlarm(
    val id: String,
    val channelId: String,
    val title: String,
    val airingStart: Long,
    val fireAt: Long,
)

/**
 * What the mirror has to do to make the platform agree with the server.
 *
 * A reminder that *moved* appears in both lists. Cancelling and re-setting the
 * same alarm is what AlarmManager does anyway when a matching PendingIntent is
 * replaced, but saying so in the plan is what lets the persisted map be
 * rewritten from one answer rather than inferred from two.
 */
data class ReminderAlarmPlan(
    val cancel: List<String>,
    val schedule: List<ReminderAlarm>,
) {
    val empty: Boolean get() = cancel.isEmpty() && schedule.isEmpty()
}

/**
 * The diff between the server's armed reminders and this phone's alarms.
 *
 * A reconciliation, never an append. Appending is what makes a phone fire a
 * notification for a reminder somebody deleted on the television an hour ago:
 * the alarm is still in the scheduler because nothing ever went looking for
 * alarms that no longer have a reminder behind them.
 *
 * A reminder whose moment has already passed is neither scheduled nor kept: an
 * alarm set for a time in the past fires immediately on some Android versions,
 * which is how a phone opened at midnight announces a programme that aired at
 * eight.
 */
fun reconcileReminderAlarms(
    armed: List<ReminderAlarm>,
    scheduled: Map<String, Long>,
    now: Long,
): ReminderAlarmPlan {
    val wanted = armed.filter { it.fireAt > now }.associateBy { it.id }
    val cancel = scheduled
        .filter { (id, at) -> wanted[id]?.fireAt != at }
        .keys
        .sorted()
    val schedule = wanted.values
        .filter { scheduled[it.id] != it.fireAt }
        .sortedWith(compareBy({ it.fireAt }, { it.id }))
    return ReminderAlarmPlan(cancel = cancel, schedule = schedule)
}

/**
 * This phone's mirror of the server's armed reminders.
 *
 * **What it cannot promise.** The mirror only runs while the app is open: on
 * launch, when it comes to the foreground, and after this phone itself sets or
 * deletes a reminder. A reminder created, deleted or moved on another device
 * while this phone stays closed is not reflected until the app next opens — a
 * local notification will still fire for one deleted elsewhere, and none will
 * fire for one created elsewhere. Closing that gap needs a push channel this
 * build does not have; until then the honest floor is better than an
 * append-only mirror that silently drifts.
 */
object ReminderAlarms {
    private const val STORE = "plurx-reminder-alarms"

    const val ACTION_FIRE = "tv.plurx.app.action.REMINDER_FIRED"
    const val ACTION_RECORD = "tv.plurx.app.action.REMINDER_RECORD"
    const val EXTRA_REMINDER_ID = "reminder_id"
    const val EXTRA_CHANNEL_ID = "channel_id"
    const val EXTRA_TITLE = "title"
    const val EXTRA_AIRING_START = "airing_start"

    /** Fetch the server's reminders and reconcile. Failure leaves the alarms alone. */
    suspend fun refresh(context: Context, origin: String, token: String?) {
        if (origin.isEmpty() || token.isNullOrEmpty()) return
        val reminders = runCatching { DvrApi(origin, token).reminders() }.getOrNull() ?: return
        mirror(context, reminders)
    }

    fun mirror(context: Context, reminders: List<DvrReminder>) {
        reconcile(context, armedAlarms(reminders))
    }

    /**
     * The server rows that deserve an alarm. Only `armed`: a `fired` one is
     * already on screen somewhere and about to be acked, and `acked`,
     * `expired` and `moved` are all decisions already taken.
     */
    internal fun armedAlarms(reminders: List<DvrReminder>): List<ReminderAlarm> =
        reminders.filter { it.state == "armed" }.map {
            ReminderAlarm(
                id = it.id,
                channelId = it.channel_id,
                title = it.title,
                airingStart = it.airing_start,
                fireAt = it.fireAt,
            )
        }

    /** Sign-out. Another account's reminders must not fire on this phone. */
    fun clear(context: Context) = reconcile(context, emptyList())

    // Named `reconcile`, not `apply`: a private `apply` in this scope shadows
    // kotlin.apply for every call site inside the object, and the editor below
    // ends in `.apply()`.
    private fun reconcile(context: Context, armed: List<ReminderAlarm>) {
        val store = context.getSharedPreferences(STORE, Context.MODE_PRIVATE)
        val scheduled = store.all.mapNotNull { (id, at) ->
            (at as? Long)?.let { id to it }
        }.toMap()
        val plan = reconcileReminderAlarms(armed, scheduled, System.currentTimeMillis() / 1000)
        if (plan.empty) return
        val manager = context.getSystemService(AlarmManager::class.java) ?: return
        val edit = store.edit()
        plan.cancel.forEach { id ->
            // `FLAG_NO_CREATE` so cancelling an alarm the system has already
            // delivered does not mint a PendingIntent to cancel. Extras are not
            // part of PendingIntent matching; the action and the data are.
            existing(context, id)?.let { manager.cancel(it) }
            edit.remove(id)
        }
        plan.schedule.forEach { alarm ->
            val pending = PendingIntent.getBroadcast(
                context,
                0,
                Intent(context, ReminderAlarmReceiver::class.java)
                    .setAction(ACTION_FIRE)
                    // The data is the identity: two PendingIntents with the
                    // same request code but different data are distinct, which
                    // is what lets every reminder keep its own alarm without a
                    // request-code counter to persist and keep in step.
                    .setData(reminderUri(alarm.id))
                    .putExtra(EXTRA_REMINDER_ID, alarm.id)
                    .putExtra(EXTRA_CHANNEL_ID, alarm.channelId)
                    .putExtra(EXTRA_TITLE, alarm.title)
                    .putExtra(EXTRA_AIRING_START, alarm.airingStart),
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
            val at = alarm.fireAt * 1_000
            // `USE_EXACT_ALARM` is declared and granted on install from Android
            // 13; the check still runs, because the permission does not exist
            // on 12, where only `SCHEDULE_EXACT_ALARM` would do and this build
            // does not ask for it. Those two releases get an inexact alarm — a
            // reminder that may arrive minutes late — rather than a
            // SecurityException thrown on the way into the guide.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && !manager.canScheduleExactAlarms()) {
                manager.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pending)
            } else {
                manager.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, at, pending)
            }
            edit.putLong(alarm.id, alarm.fireAt)
        }
        edit.apply()
    }

    internal fun reminderUri(id: String): Uri = Uri.parse("plurx://reminder/$id")

    private fun existing(context: Context, id: String): PendingIntent? =
        PendingIntent.getBroadcast(
            context,
            0,
            Intent(context, ReminderAlarmReceiver::class.java)
                .setAction(ACTION_FIRE)
                .setData(reminderUri(id)),
            PendingIntent.FLAG_NO_CREATE or PendingIntent.FLAG_IMMUTABLE,
        )

    /** Forget one alarm after the system has delivered it. */
    internal fun forget(context: Context, id: String) {
        context.getSharedPreferences(STORE, Context.MODE_PRIVATE).edit().remove(id).apply()
    }

    /** The saved profile, for work that starts in a receiver rather than a screen. */
    internal suspend fun profile(context: Context): Pair<String, String>? {
        val saved = runCatching { SettingsStore(context).flow.first() }.getOrNull() ?: return null
        val token = saved.token ?: return null
        return saved.origin.takeIf { it.isNotEmpty() }?.let { it to token }
    }
}
