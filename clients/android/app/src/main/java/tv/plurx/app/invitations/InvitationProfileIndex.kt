package tv.plurx.app.invitations

import android.content.Context
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.remote.RemoteProfileScope
import tv.plurx.app.remote.RemoteWire

@Serializable internal data class InvitationSavedProfile(val origin: String, val instance: String, val account: Long) {
    fun scope() = RemoteProfileScope.create(origin, instance, account).also { require(it.origin == origin) }
}

/** The non-secret bounded index enumerates cleanup obligations without selecting another account. */
internal class InvitationProfileIndex(context: Context) {
    private val preferences = context.getSharedPreferences("cinema-invitation-state", Context.MODE_PRIVATE)
    companion object { private val lock = Any() }
    fun read(): List<InvitationSavedProfile> = synchronized(lock) {
        val encoded = preferences.getString("profiles", null) ?: return@synchronized emptyList()
        require(encoded.toByteArray(Charsets.UTF_8).size <= 16384)
        val body = RemoteWire.objectBody(encoded.toByteArray(Charsets.UTF_8), 16384)
        require(body.keys == setOf("profiles"))
        val values = RemoteWire.json.decodeFromJsonElement<List<InvitationSavedProfile>>(requireNotNull(body["profiles"]))
        require(values.size <= 8 && values.map { it.scope().digest }.distinct().size == values.size)
        values
    }
    fun add(scope: RemoteProfileScope) = synchronized(lock) {
        val values = read()
        if (values.any { it.scope() == scope }) return@synchronized
        require(values.size < 8) { "Saved Cinema profile limit reached; remove a saved profile explicitly." }
        val next = values + InvitationSavedProfile(scope.origin, scope.instance, scope.account)
        val text = buildJsonObject { put("profiles", RemoteWire.json.encodeToJsonElement(next)) }.toString()
        check(preferences.edit().putString("profiles", text).commit())
    }
    fun remove(scope: RemoteProfileScope) = synchronized(lock) {
        val next = read().filter { it.scope() != scope }
        val text = buildJsonObject { put("profiles", RemoteWire.json.encodeToJsonElement(next)) }.toString()
        check(preferences.edit().putString("profiles", text).commit())
    }
    fun active(): RemoteProfileScope? = synchronized(lock) {
        val value = preferences.getString("active", null) ?: return@synchronized null
        require(value.toByteArray(Charsets.UTF_8).size <= 2048)
        val body = RemoteWire.objectBody(value.toByteArray(Charsets.UTF_8), 2048)
        RemoteWire.json.decodeFromJsonElement<InvitationSavedProfile>(body).scope().also { scope -> require(read().any { it.scope() == scope }) }
    }
    fun activate(scope: RemoteProfileScope?) = synchronized(lock) {
        val edit = preferences.edit()
        if (scope == null) edit.remove("active") else {
            require(read().any { it.scope() == scope })
            edit.putString("active", RemoteWire.json.encodeToString(InvitationSavedProfile.serializer(), InvitationSavedProfile(scope.origin, scope.instance, scope.account)))
        }
        check(edit.commit())
    }
    /** Explicit local recovery loses the index; it does not assert any remote installation was revoked. */
    fun resetCorruptIndex() = synchronized(lock) { check(preferences.edit().remove("profiles").remove("active").commit()) }
}
