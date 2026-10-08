package tv.plurx.app.invitations

import android.content.Context
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import tv.plurx.app.data.Session
import tv.plurx.app.data.SettingsStore
import tv.plurx.app.remote.RemoteProfileScope
import java.security.MessageDigest

internal object InvitationLogin {
    fun fingerprint(token: String): String = MessageDigest.getInstance("SHA-256").digest(token.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
    /** Local secure-login bootstrap only. No network or account selection precedes notification admission. */
    fun matches(context: Context, profile: RemoteProfileScope, expected: String?): Boolean = runCatching {
        if (expected == null) return false
        val saved = runBlocking(Dispatchers.IO) { withTimeout(500) { SettingsStore(context).flow.first() } }
        val token = saved.token ?: return false
        if (saved.instanceId == null || saved.userId == null) return false
        if (RemoteProfileScope.create(saved.origin, saved.instanceId, saved.userId) != profile || fingerprint(token) != expected) return false
        val live = Session.playbackAuthorization()
        if (live.origin.isNotEmpty() && Session.canonicalOrigin(live.origin) != profile.origin) return false
        if (live.token != null && fingerprint(requireNotNull(live.token)) != expected) return false
        true
    }.getOrDefault(false)
}
