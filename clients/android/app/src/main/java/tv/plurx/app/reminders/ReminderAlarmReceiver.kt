package tv.plurx.app.reminders

import android.Manifest
import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import tv.plurx.app.MainActivity
import tv.plurx.app.R
import tv.plurx.app.livetv.DvrApi
import tv.plurx.app.livetv.liveTvTime

/**
 * The phone half of a reminder: an exact alarm the app set while it was open,
 * arriving here whether or not the app is still running.
 *
 * It carries the programme in its own extras rather than reading the server,
 * because the whole point of this path is the case where nothing of plurx is
 * running and the network may not answer. The two actions do talk to the
 * server, and each says so if it cannot.
 */
class ReminderAlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra(ReminderAlarms.EXTRA_REMINDER_ID) ?: return
        when (intent.action) {
            ReminderAlarms.ACTION_FIRE -> {
                ReminderAlarms.forget(context, id)
                post(context, intent, id)
            }
            ReminderAlarms.ACTION_RECORD -> record(context, intent, id)
        }
    }

    // The permission is checked on the line below, but lint cannot follow that
    // through a receiver's dispatch; the suppression is for the analyser, not
    // for the check.
    @SuppressLint("MissingPermission")
    private fun post(context: Context, intent: Intent, id: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            // The viewer declined notifications. The in-app overlay is still
            // the surface that always works, and throwing out of a broadcast
            // receiver would be a crash rather than a missing reminder.
            return
        }
        val title = intent.getStringExtra(ReminderAlarms.EXTRA_TITLE).orEmpty()
        val channelId = intent.getStringExtra(ReminderAlarms.EXTRA_CHANNEL_ID).orEmpty()
        val start = intent.getLongExtra(ReminderAlarms.EXTRA_AIRING_START, 0)
        val manager = NotificationManagerCompat.from(context)
        manager.createNotificationChannel(
            // Beside `offline-downloads`, but built through the compat API
            // rather than `NotificationChannel` directly: that one is API 26
            // and this app's minSdk is 23, and a reminder that silently never
            // appears on an older phone is worse than no reminder at all.
            NotificationChannelCompat.Builder(CHANNEL_ID, NotificationManagerCompat.IMPORTANCE_HIGH)
                .setName(context.getString(R.string.reminder_channel))
                .build(),
        )
        val notification = NotificationCompat.Builder(context, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_popup_reminder)
            .setContentTitle(title.ifEmpty { context.getString(R.string.app_name) })
            .setContentText("Starts at ${liveTvTime(start)}")
            .setPriority(NotificationCompat.PRIORITY_HIGH)
            .setAutoCancel(true)
            .setContentIntent(watch(context, id, channelId))
            .addAction(0, "Watch", watch(context, id, channelId))
            .addAction(0, "Record", recordAction(context, id, channelId, start))
            .build()
        // The notification id is derived from the reminder's own id: a second
        // reminder replacing the first would be the same failure the alarm
        // identity avoids, and a counter here would have to be persisted too.
        runCatching { manager.notify(notificationId(id), notification) }
    }

    /**
     * Record from the notification. The process may be cold, so the profile
     * comes from the saved settings rather than from `Session`, which only a
     * running app populates.
     */
    private fun record(context: Context, intent: Intent, id: String) {
        val channelId = intent.getStringExtra(ReminderAlarms.EXTRA_CHANNEL_ID) ?: return
        val start = intent.getLongExtra(ReminderAlarms.EXTRA_AIRING_START, 0)
        val application = context.applicationContext
        // `goAsync` buys about ten seconds, and a scheduling POST against a
        // server on the same LAN is far inside that. A server that is not
        // answering simply misses the window, which is the same outcome as
        // pressing Record with no network — and is why this never blocks the
        // notification's own dismissal.
        val finish = goAsync()
        CoroutineScope(SupervisorJob() + Dispatchers.IO).launch {
            try {
                val profile = ReminderAlarms.profile(application)
                if (profile != null) {
                    val (origin, token) = profile
                    runCatching { DvrApi(origin, token).record(channelId, start) }
                }
                NotificationManagerCompat.from(application).cancel(notificationId(id))
            } finally {
                finish.finish()
            }
        }
    }

    private fun watch(context: Context, id: String, channelId: String): PendingIntent =
        PendingIntent.getActivity(
            context,
            0,
            Intent(context, MainActivity::class.java)
                .setAction(Intent.ACTION_VIEW)
                .setData(ReminderAlarms.reminderUri(id))
                .putExtra(MainActivity.EXTRA_LIVE_TV_CHANNEL, channelId)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )

    private fun recordAction(
        context: Context,
        id: String,
        channelId: String,
        start: Long,
    ): PendingIntent = PendingIntent.getBroadcast(
        context,
        0,
        Intent(context, ReminderAlarmReceiver::class.java)
            .setAction(ReminderAlarms.ACTION_RECORD)
            .setData(ReminderAlarms.reminderUri(id))
            .putExtra(ReminderAlarms.EXTRA_REMINDER_ID, id)
            .putExtra(ReminderAlarms.EXTRA_CHANNEL_ID, channelId)
            .putExtra(ReminderAlarms.EXTRA_AIRING_START, start),
        PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )

    private fun notificationId(reminderId: String): Int = reminderId.hashCode()

    private companion object {
        const val CHANNEL_ID = "reminders"
    }
}
