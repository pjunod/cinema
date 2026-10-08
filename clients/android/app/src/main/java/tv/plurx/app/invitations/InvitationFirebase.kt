package tv.plurx.app.invitations

import android.content.Context
import com.google.firebase.FirebaseApp
import com.google.firebase.FirebaseOptions
import com.google.firebase.messaging.FirebaseMessaging
import kotlinx.coroutines.suspendCancellableCoroutine
import tv.plurx.app.BuildConfig

internal object InvitationFirebase {
    @Volatile private var ready = false
    fun initialize(context: Context) {
        val values = listOf(BuildConfig.CINEMA_FIREBASE_PROJECT_ID, BuildConfig.CINEMA_FIREBASE_APPLICATION_ID,
            BuildConfig.CINEMA_FIREBASE_API_KEY, BuildConfig.CINEMA_FIREBASE_SENDER_ID)
        if (values.any { it.isEmpty() }) return
        ready = runCatching {
            if (FirebaseApp.getApps(context).none { it.name == FirebaseApp.DEFAULT_APP_NAME }) {
                FirebaseApp.initializeApp(context, FirebaseOptions.Builder().setProjectId(values[0])
                    .setApplicationId(values[1]).setApiKey(values[2]).setGcmSenderId(values[3]).build())
            }
            FirebaseMessaging.getInstance().isAutoInitEnabled = false
            true
        }.getOrDefault(false)
    }
    val configured get() = ready
    suspend fun token(): String {
        check(ready) { "Firebase is not configured in this build" }
        return suspendCancellableCoroutine { continuation ->
            FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
                if (continuation.isActive) continuation.resumeWith(if (task.isSuccessful) runCatching { requireNotNull(task.result) } else Result.failure(task.exception ?: IllegalStateException("Firebase token unavailable")))
            }
        }
    }
}
