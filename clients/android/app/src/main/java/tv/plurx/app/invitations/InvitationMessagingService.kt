package tv.plurx.app.invitations

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

class InvitationMessagingService : FirebaseMessagingService() {
    override fun onMessageReceived(message: RemoteMessage) {
        if (!InvitationFirebase.configured || message.notification != null || message.data.keys != setOf("invitation_id", "category")) return
        if (message.data["category"] != INVITATION_CATEGORY) return
        val id = message.data["invitation_id"] ?: return
        InvitationNotifications.receive(this, id, "fcm")
    }
    override fun onNewToken(token: String) {
        // Only the live, explicitly enrolled same-login owner may rotate its provider token.
        InvitationRuntime.tokenRotated(token)
    }
}
