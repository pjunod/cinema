package tv.plurx.app.remote

import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonPrimitive
import tv.plurx.app.data.Session
import java.security.MessageDigest

/** Framed origin/instance/account identity; notification bytes never choose it. */
internal data class RemoteProfileScope(val origin: String, val instance: String, val account: Long) {
    init {
        require(Session.canonicalOrigin(origin) == origin && origin.toByteArray(Charsets.UTF_8).size <= 1024)
        require(instance.toByteArray(Charsets.UTF_8).size in 1..128 && instance.none { Character.isISOControl(it) })
        require(account in 1..REMOTE_MAX_INTEGER)
    }
    val identity: String get() = "cinema-profile-v2:" + JsonArray(listOf(JsonPrimitive(origin), JsonPrimitive(instance), JsonPrimitive(account))).toString()
    val digest: String get() = MessageDigest.getInstance("SHA-256").digest(identity.toByteArray(Charsets.UTF_8)).joinToString("") { "%02x".format(it) }
    companion object {
        fun create(origin: String, instance: String, account: Long) = RemoteProfileScope(requireNotNull(Session.canonicalOrigin(origin)), instance, account)
    }
}
