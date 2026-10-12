package tv.plurx.app.diagnostics

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import java.io.File
import java.security.KeyStore
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

@Serializable
internal data class DiagnosticDraft(
    val capture: DiagnosticCapture, val category: String = "other", val description: String = "", val title: String? = null,
    val serverEvents: List<DiagnosticEvent> = emptyList(), val components: List<DiagnosticComponent> = emptyList(),
    val connection: List<DiagnosticEvent> = emptyList(), val consented: Boolean = false, val origin: String? = null,
    val state: String = "local", val reference: String? = null, val handoff: DiagnosticOffer? = null,
    val binding: DiagnosticBinding? = null, val manifest: JsonObject? = null,
) { val id get() = capture.id }

/** Private, backup-excluded files with a bounded in-memory fallback. Only this
 * IO owner reads/writes files. Authorities are Keystore encrypted separately. */
internal class DiagnosticStore(private val root: File) {
    private val mutex = Mutex()
    private val memory = linkedMapOf<String, DiagnosticDraft>()
    private val archives = linkedMapOf<String, ByteArray>()
    private val ringMemory = mutableMapOf<String, DiagnosticCapture>()
    private val retired = mutableSetOf<String>()
    private fun directory(scope: String): File {
        require(scope == "anonymous" || Regex("[a-f0-9]{64}").matches(scope)); require(scope !in retired)
        val directory = File(root, scope); check(directory.mkdirs() || directory.isDirectory); return directory
    }
    private fun id(id: String) { require(UUID.fromString(id).toString() == id) }
    private fun read(file: File, limit: Int): ByteArray {
        require(file.isFile && file.canonicalFile.parentFile == requireNotNull(file.parentFile).canonicalFile && file.length() <= limit)
        return file.inputStream().use { input ->
            val output = java.io.ByteArrayOutputStream(); val buffer = ByteArray(16384)
            while (true) { val count=input.read(buffer); if (count < 0) break; require(output.size()+count <= limit); output.write(buffer,0,count) }
            output.toByteArray()
        }
    }
    private fun write(file: File, bytes: ByteArray) {
        val temporary = File(file.parentFile, file.name + ".writing")
        temporary.outputStream().use { it.write(bytes); it.fd.sync() }
        check(temporary.renameTo(file)); file.setReadable(false, false); file.setWritable(false, false); file.setReadable(true, true); file.setWritable(true, true)
    }
    private fun prune() {
        val folders = root.listFiles().orEmpty(); require(folders.size <= 32)
        val files = folders.filter { it.isDirectory && it.canonicalFile.parentFile == root.canonicalFile }.flatMap { folder -> folder.listFiles().orEmpty().also { require(it.size <= 32) }.filter { it.isFile && it.name != "ring.json" } }
        for (file in files) if (file.length() > 8388608 || file.lastModified() < System.currentTimeMillis() - 86400000 || file.name.endsWith(".writing")) file.delete()
        val rows = files.filter { it.exists() }.groupBy { it.parentFile.name + "/" + it.name.substringBeforeLast('.') }.toMutableMap()
        var total = rows.values.flatten().sumOf { it.length() }
        while (rows.size > 10 || total > 33554432) {
            val oldest = rows.minByOrNull { it.value.minOf { file -> file.lastModified() } } ?: break
            for (file in oldest.value) { total -= file.length(); file.delete() }
            rows.remove(oldest.key)
        }
    }
    suspend fun activate(scope: String) = mutex.withLock { retired.remove(scope) }
    suspend fun save(draft: DiagnosticDraft): String = withContext(Dispatchers.IO) { mutex.withLock {
        if (draft.capture.namespace in retired) return@withLock "unavailable"
        id(draft.id); require(draft.capture.events.size <= 10000 && draft.serverEvents.size <= 10000 && draft.connection.size <= 100)
        require(draft.capture.events.all { it.valid() } && draft.serverEvents.all { it.valid() } && draft.connection.all { it.valid() })
        val bytes = DiagnosticJson.encodeToString(DiagnosticDraft.serializer(), draft).toByteArray(); require(bytes.size <= 8388608)
        memory[draft.id] = draft; while (memory.size > 10) memory.remove(memory.keys.first())
        runCatching { val folder = directory(draft.capture.namespace); prune(); write(File(folder, draft.id + ".json"), bytes); prune(); "available" }.getOrDefault("memory_only")
    } }
    suspend fun saveRing(capture: DiagnosticCapture): String = withContext(Dispatchers.IO) { mutex.withLock {
        if (capture.namespace in retired) return@withLock "unavailable"
        ringMemory.clear(); ringMemory[capture.namespace] = capture
        runCatching { write(File(directory(capture.namespace), "ring.json"), DiagnosticJson.encodeToString(DiagnosticCapture.serializer(), capture).toByteArray()); "available" }.getOrDefault("memory_only")
    } }
    suspend fun ring(scope: String): DiagnosticCapture? = withContext(Dispatchers.IO) { mutex.withLock {
        runCatching { DiagnosticJson.decodeFromString(DiagnosticCapture.serializer(), read(File(directory(scope), "ring.json"), 2500000).toString(Charsets.UTF_8)).also { require(it.namespace == scope && it.events.size <= 10000 && it.events.all { event -> event.valid() }) }.copy(persistence = "available") }.getOrNull() ?: ringMemory[scope]
    } }
    suspend fun drafts(scope: String): List<DiagnosticDraft> = withContext(Dispatchers.IO) { mutex.withLock {
        runCatching {
            val folder = directory(scope); prune()
            folder.listFiles().orEmpty().take(32).filter { it.name != "ring.json" && it.extension == "json" }.forEach { file ->
                runCatching { DiagnosticJson.decodeFromString(DiagnosticDraft.serializer(), read(file, 8388608).toString(Charsets.UTF_8)) }.getOrNull()?.takeIf { it.capture.namespace == scope && it.id + ".json" == file.name && it.capture.events.all { event -> event.valid() } }?.let { memory[it.id] = it }
            }
        }
        memory.values.filter { it.capture.namespace == scope && it.capture.incidentMs >= System.currentTimeMillis() - 86400000 }.sortedByDescending { it.capture.incidentMs }.take(10)
    } }
    suspend fun saveArchive(id: String, scope: String, bytes: ByteArray): String = withContext(Dispatchers.IO) { mutex.withLock {
        id(id); require(bytes.size <= 8388608); if (scope in retired) return@withLock "unavailable"
        archives.clear(); archives[id] = bytes
        runCatching { write(File(directory(scope), id + ".zip"), bytes); prune(); "available" }.getOrDefault("memory_only")
    } }
    suspend fun archive(id: String, scope: String): ByteArray? = withContext(Dispatchers.IO) { mutex.withLock { id(id); if (scope in retired) null else archives[id] ?: runCatching { read(File(directory(scope), id + ".zip"), 8388608) }.getOrNull() } }
    suspend fun delete(id: String, scope: String) = withContext(Dispatchers.IO) { mutex.withLock {
        id(id); memory.remove(id); archives.remove(id)
        runCatching { val folder = directory(scope); File(folder, id + ".json").delete(); File(folder, id + ".zip").delete() }
    } }
    suspend fun clearRing(scope: String) = withContext(Dispatchers.IO) { mutex.withLock { ringMemory.remove(scope); runCatching { File(directory(scope), "ring.json").delete() }; Unit } }
    suspend fun clear(scope: String) = withContext(Dispatchers.IO) { mutex.withLock {
        memory.entries.removeAll { it.value.capture.namespace == scope }; archives.clear(); ringMemory.remove(scope)
        runCatching { directory(scope).deleteRecursively() }; retired += scope
    } }
}

/** Each account owns an authenticated-encryption key and AAD namespace. No
 * authority ever falls back to plaintext files, preferences or report ZIPs. */
internal class DiagnosticSecrets(context: Context, private val scope: String) {
    private val preferences = context.getSharedPreferences("cinema-diagnostic-secrets", Context.MODE_PRIVATE)
    private val alias = "cinema.diagnostics." + scope
    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }; (store.getKey(alias, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").apply {
            init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).build())
        }.generateKey()
    }
    @Synchronized fun read(name: String): JsonObject? = runCatching {
        val text = preferences.getString(scope + ":" + name, null) ?: return null; require(text.length <= 8192)
        val bytes = Base64.decode(text, Base64.NO_WRAP); require(bytes.size in 29..6144)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding"); cipher.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(128, bytes.copyOfRange(0,12))); cipher.updateAAD((scope + ":" + name).toByteArray())
        DiagnosticJson.parseToJsonElement(cipher.doFinal(bytes.copyOfRange(12,bytes.size)).toString(Charsets.UTF_8)).jsonObject
    }.getOrNull()
    @Synchronized fun save(name: String, value: JsonObject) {
        val bytes = value.toString().toByteArray(); require(bytes.size <= 4096)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding"); cipher.init(Cipher.ENCRYPT_MODE, key()); cipher.updateAAD((scope + ":" + name).toByteArray())
        check(preferences.edit().putString(scope + ":" + name, Base64.encodeToString(cipher.iv + cipher.doFinal(bytes), Base64.NO_WRAP)).commit())
    }
    @Synchronized fun delete(name: String) { check(preferences.edit().remove(scope + ":" + name).commit()) }
    @Synchronized fun clear() {
        val editor = preferences.edit(); preferences.all.keys.filter { it.startsWith(scope + ":") }.forEach { editor.remove(it) }; check(editor.commit())
        KeyStore.getInstance("AndroidKeyStore").apply { load(null); if (containsAlias(alias)) deleteEntry(alias) }
    }
}
