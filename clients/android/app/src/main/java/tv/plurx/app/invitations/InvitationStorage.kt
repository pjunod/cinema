package tv.plurx.app.invitations

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.remote.RemoteProfileScope
import tv.plurx.app.remote.RemoteWire
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@Serializable internal data class InvitationChoice(val receiver: String, val enabled: Boolean, val transport: String, val intentRevision: Long, val pendingSync: Boolean)
@Serializable internal data class InvitationLocalState(val scope: String, val installation: String?, val phoneSecret: String?,
    val phone: InvitationPhone?, val choices: List<InvitationChoice>, val consents: List<InvitationConsent>,
    val residentChoice: Boolean, val runRequested: Boolean, val revision: Long, val seen: Map<String, Long>,
    val loginFingerprint: String?, val registrationAttempted: Boolean, val lostProof: Boolean,
    val pendingDeletion: Boolean, val pendingAvailabilityOff: Boolean, val lastClock: Long, val residentRunId: String?)

/** Corruption is explicit, not an empty index that silently registers another orphan installation. */
internal class InvitationStorage(context: Context, private val profile: RemoteProfileScope) {
    private val preferences = context.getSharedPreferences("cinema-invitation-state", Context.MODE_PRIVATE)
    private val alias = "cinema.invitation." + profile.digest
    private val name = profile.digest
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }
    fun empty() = InvitationLocalState(profile.identity, null, null, null, emptyList(), emptyList(), false, false, 0, emptyMap(), null, false, false, false, false, 0, null)
    fun read(): InvitationLocalState {
        val encoded = preferences.getString(name, null) ?: return empty()
        require(encoded.length <= 174768)
        val bytes = Base64.decode(encoded, Base64.NO_WRAP); require(bytes.size in 29..131072)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0, 12)))
        cipher.updateAAD(profile.identity.toByteArray(Charsets.UTF_8))
        val body = RemoteWire.objectBody(cipher.doFinal(bytes.copyOfRange(12, bytes.size)), 131072)
        return RemoteWire.json.decodeFromJsonElement<InvitationLocalState>(body).also(::validate)
    }
    private fun validate(state: InvitationLocalState) {
        require(state.scope == profile.identity && state.choices.size <= 160 && state.consents.size <= 160 && state.seen.size <= 256)
        state.installation?.let(RemoteWire::uuid); state.phoneSecret?.let(InvitationWire::proof)
        require(state.phoneSecret == null || state.installation != null)
        state.phone?.let { InvitationWire.phone(it); require(it.platform == "android" && it.installation_id == state.installation) }
        state.choices.forEach { InvitationWire.counter(it.intentRevision); RemoteWire.uuid(it.receiver); require(it.transport in setOf("fcm", "android_resident")) }
        require(state.choices.map { it.receiver }.distinct().size == state.choices.size)
        state.consents.forEach(InvitationWire::consent)
        require(state.consents.map { it.receiver_id }.distinct().size == state.consents.size)
        InvitationWire.counter(state.revision, true); InvitationWire.counter(state.lastClock, true)
        require(state.loginFingerprint == null || Regex("[a-f0-9]{64}").matches(state.loginFingerprint))
        require(!state.lostProof || state.registrationAttempted && state.phoneSecret == null)
        require(!state.pendingDeletion || state.installation != null)
        state.seen.forEach { (id, expiry) -> InvitationWire.invitation(id); InvitationWire.counter(expiry) }
        state.residentRunId?.let(RemoteWire::uuid)
        require(!state.runRequested || state.residentChoice && state.residentRunId != null)
    }
    fun save(state: InvitationLocalState) {
        validate(state)
        val bytes = RemoteWire.json.encodeToString(InvitationLocalState.serializer(), state).toByteArray(Charsets.UTF_8)
        require(bytes.size <= 100000)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding"); cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(profile.identity.toByteArray(Charsets.UTF_8))
        check(preferences.edit().putString(name, Base64.encodeToString(cipher.iv + cipher.doFinal(bytes), Base64.NO_WRAP)).commit())
    }
    /** Local reset never claims to revoke an installation on the home. Human list/delete remains available. */
    fun reset() {
        check(preferences.edit().remove(name).commit())
        KeyStore.getInstance("AndroidKeyStore").apply { load(null); if (containsAlias(alias)) deleteEntry(alias) }
    }
}
