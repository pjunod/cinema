package tv.plurx.app.invitations

import android.Manifest
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import tv.plurx.app.MainActivity

internal object InvitationNotifications {
    const val CHANNEL = "cinema_invitations"
    const val EXTRA_ID = "cinema_invitation_id"
    val lock = Any()
    fun channel(context: Context) {
        context.getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(CHANNEL, "Cinema screen invitations", NotificationManager.IMPORTANCE_DEFAULT))
    }
    fun permission(context: Context): Boolean {
        if (Build.VERSION.SDK_INT >= 33 && context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) return false
        val manager = context.getSystemService(NotificationManager::class.java)
        return NotificationManagerCompat.from(context).areNotificationsEnabled() && manager.getNotificationChannel(CHANNEL)?.importance != NotificationManager.IMPORTANCE_NONE
    }
    enum class Receipt { Visible, Consumed, Unavailable }
    fun receive(context: Context, id: String, transport: String, expiry: Long? = null): Boolean = receiveOwned(context, id, transport, expiry) == Receipt.Visible
    fun receiveOwned(context: Context, id: String, transport: String, expiry: Long? = null): Receipt = synchronized(lock) {
        runCatching {
            val profile = InvitationProfileIndex(context).active() ?: return Receipt.Consumed
            val storage = InvitationStorage(context, profile)
            val state = storage.read()
            val phone = state.phone ?: return Receipt.Consumed
            if (state.pendingDeletion || state.lostProof || state.phoneSecret == null || state.loginFingerprint == null) return Receipt.Consumed
            val offered = state.consents.filter { consent -> consent.enabled && consent.readiness.eligible && state.choices.any {
                it.receiver == consent.receiver_id && it.enabled && !it.pendingSync && it.transport == consent.transport
            } }
            val gate = InvitationAdmission()
            gate.restore(state.seen, state.lastClock)
            if (!gate.validClock()) return Receipt.Unavailable
            val eligible = InvitationAdmission.State(state.installation, InvitationLogin.matches(context, profile, state.loginFingerprint), phone.permission_granted && permission(context), true,
                offered.any { it.transport == "fcm" }, state.runRequested && phone.resident_active && offered.any { it.transport == "android_resident" })
            if (!gate.admit(eligible, id, transport, expiry)) return if (state.seen.size >= 256 && id !in state.seen) Receipt.Unavailable else Receipt.Consumed
            // Durable dedupe precedes the sole notification owner. A failed commit suppresses display.
            storage.save(state.copy(seen = gate.snapshot(), lastClock = gate.clock()))
            channel(context)
            val intent = Intent(context, MainActivity::class.java).putExtra(EXTRA_ID, id)
                .setData(android.net.Uri.parse("cinema-invitation:" + id)).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_CLEAR_TOP)
            val pending = PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT)
            val notification = NotificationCompat.Builder(context, CHANNEL).setSmallIcon(android.R.drawable.ic_dialog_info)
                .setContentTitle("Cinema is open on a screen").setContentText("Tap to open its remote.").setContentIntent(pending).setAutoCancel(true).build()
            context.getSystemService(NotificationManager::class.java).notify(id, 1, notification)
            Receipt.Visible
        }.getOrDefault(Receipt.Unavailable)
    }
}
