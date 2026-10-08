package tv.plurx.app.remote

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.serialization.json.*
import java.security.KeyStore
import java.security.MessageDigest
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/** Only encrypted authenticated ciphertext is stored in preferences; no plaintext fallback. */
internal class RemoteSecretStorage(context: Context, identity: String) {
    data class Receiver(val id: String, val secret: String)
    data class Grant(val receiverId: String, val id: String, val secret: String)
    private val scope = MessageDigest.getInstance("SHA-256").digest(identity.toByteArray()).joinToString("") { "%02x".format(it) }
    private val alias = "cinema.remote." + scope
    private val preferences = context.getSharedPreferences("cinema-remote-secrets", Context.MODE_PRIVATE)
    companion object {
        fun validSecret(value: String): Boolean = Regex("[A-Za-z0-9_-]{43}").matches(value) && runCatching { Base64.decode(value, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING).size == 32 }.getOrDefault(false)
    }
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }
    private fun read(name: String): JsonObject? = runCatching {
        val encoded = preferences.getString(scope + ":" + name, null) ?: return null
        val packed = Base64.decode(encoded, Base64.NO_WRAP)
        require(packed.size in 29..32768)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, packed.copyOfRange(0, 12)))
        cipher.updateAAD((scope + ":" + name).toByteArray())
        RemoteWire.objectBody(cipher.doFinal(packed.copyOfRange(12, packed.size)), 32768)
    }.getOrNull()
    private fun write(name: String, value: JsonObject) {
        val bytes = value.toString().toByteArray(); require(bytes.size <= 16000)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding"); cipher.init(Cipher.ENCRYPT_MODE, key()); cipher.updateAAD((scope + ":" + name).toByteArray())
        check(preferences.edit().putString(scope + ":" + name, Base64.encodeToString(cipher.iv + cipher.doFinal(bytes), Base64.NO_WRAP)).commit())
    }
    val receiver get() = read("receiver")?.let { value -> runCatching { Receiver(RemoteWire.uuid(value.string("id")), value.string("secret")).also { require(validSecret(it.secret)) } }.getOrNull() }
    fun saveReceiver(value: Receiver) { require(validSecret(value.secret)); write("receiver", buildJsonObject { put("id", value.id); put("secret", value.secret) }) }
    val grants: List<Grant> get() = read("grants")?.get("grants")?.jsonArray?.take(20)?.mapNotNull { value -> runCatching { val o = value.jsonObject; Grant(RemoteWire.uuid(o.string("receiver_id")), RemoteWire.uuid(o.string("id")), o.string("secret")).also { require(validSecret(it.secret)) } }.getOrNull() } ?: emptyList()
    fun saveGrant(value: Grant) { require(validSecret(value.secret)); writeGrants(grants.filter { it.receiverId != value.receiverId } + value) }
    private fun writeGrants(values: List<Grant>) { require(values.size <= 20); write("grants", buildJsonObject { put("grants", JsonArray(values.map { buildJsonObject { put("receiver_id", it.receiverId); put("id", it.id); put("secret", it.secret) } })) }) }
    fun forgetGrant(id: String) { writeGrants(grants.filter { it.id != id }) }
    fun clearReceiver() { preferences.edit().remove(scope + ":receiver").commit() }
    fun clear() { preferences.edit().remove(scope + ":receiver").remove(scope + ":grants").commit(); KeyStore.getInstance("AndroidKeyStore").apply { load(null); if (containsAlias(alias)) deleteEntry(alias) } }
}
