package tv.plurx.app.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import tv.plurx.app.BuildConfig

/** Captures B authorization; no caller or response can choose a foreign origin. */
internal class SharedLibraryClient private constructor(private val auth: Session.PlaybackAuthorization, private val transport: OkHttpClient) {
    fun requireCurrent() { require(Session.playbackAuthorization() == auth && !auth.token.isNullOrEmpty()) }
    private suspend fun request(path: String, query: Map<String, String> = emptyMap(), enabled: Boolean? = null): JsonObject = withContext(Dispatchers.IO) {
        requireCurrent()
        val builder = auth.origin.toHttpUrl().newBuilder().encodedPath("/api/v1/$path")
        query.forEach { (key, value) -> builder.addQueryParameter(key, value) }
        val url = builder.build()
        val request = Request.Builder().url(url).header("Authorization", "Bearer ${auth.token}")
        if (enabled != null) request.put(buildJsonObject { put("enabled", enabled) }.toString().toRequestBody("application/json".toMediaType()))
        transport.newCall(request.build()).execute().use { response ->
            requireCurrent(); require(response.request.url == url)
            val source = requireNotNull(response.body).source()
            source.request(4_194_305)
            val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(4_194_305))
            require(bytes.size <= 4_194_304)
            val text = bytes.toString(Charsets.UTF_8)
            if (!response.isSuccessful) throw (parseRefusal(response.code, text) ?: IllegalStateException("Server returned ${response.code}"))
            Json.parseToJsonElement(text).jsonObject
        }
    }
    suspend fun assignments(): List<SharedLibraryAssignment> {
        val rows = request("shared/libraries").getValue("libraries").jsonArray
        require(rows.size <= 2048)
        val result = rows.map { value ->
            val item = value.jsonObject
            listOf("import_id", "server_id", "catalogue_epoch", "library_id").forEach { item.string(it) }
            json.decodeFromJsonElement<SharedLibraryAssignment>(item).also { it.identity.validate() }
        }
        require(result.map { it.id }.toSet().size == result.size)
        return result
    }
    suspend fun libraries(assigned: List<SharedLibraryAssignment>): List<SharedLibraryRow> {
        val first = assigned.firstOrNull() ?: return emptyList()
        assigned.forEach { it.identity.validate(); require(it.identity.sourceId == first.identity.sourceId) }
        val wire = request("shared/imports/${first.import_id}/libraries")
        require(wire.string("import_id") == first.import_id && wire.string("server_id") == first.server_id && wire.string("catalogue_epoch") == first.catalogue_epoch)
        val rows = wire.getValue("libraries").jsonArray
        require(rows.size <= 64)
        val decoded = rows.map {
            val value = it.jsonObject; require(PlaybackFileContext.canonicalId(value.string("library_id")))
            json.decodeFromJsonElement<SharedLibraryMetadata>(value)
        }
        require(decoded.map { it.library_id }.toSet().size == decoded.size)
        val allowed = assigned.map { it.library_id }.toSet()
        return decoded.filter { it.library_id in allowed }.map { SharedLibraryRow(SharedLibraryIdentity(first.import_id, first.server_id, first.catalogue_epoch, it.library_id), it.name, first.source_name, it.kind) }
    }
    suspend fun page(library: SharedLibraryIdentity, parent: SharedPlaybackReference? = null, q: String = "", cursor: String? = null): SharedLibraryPage {
        library.validate()
        require(q.toByteArray().size <= 512 && q.none { it.code < 32 || it.code == 127 } && (cursor?.toByteArray()?.size ?: 0) <= 4096)
        val path = if (parent != null) {
            parent.validate(); require(library.contains(parent))
            "shared/imports/${parent.import_id}/items/${parent.item_id}/children"
        } else "shared/imports/${library.import_id}/libraries/${library.library_id}/items"
        val query = mutableMapOf("q" to q, "limit" to "60"); if (cursor != null) query["cursor"] = cursor
        val wire = request(path, query)
        wire.getValue("items").jsonArray.forEach { strictItem(it.jsonObject) }
        return json.decodeFromJsonElement<SharedLibraryPage>(wire).also { it.validate(library) }
    }
    suspend fun detail(reference: SharedPlaybackReference): SharedLibraryDetail {
        reference.validate()
        val wire = request("shared/imports/${reference.import_id}/items/${reference.item_id}")
        strictItem(wire.getValue("item").jsonObject)
        wire.getValue("files").jsonArray.forEach {
            val file = it.jsonObject
            listOf("file_id", "revision", "size").forEach { file.string(it) }
            val binding = file.getValue("reference").jsonObject
            binding.string("file_id"); binding.string("revision"); strictReference(binding.getValue("item").jsonObject)
        }
        return json.decodeFromJsonElement<SharedLibraryDetail>(wire).also { it.validate(reference) }
    }
    suspend fun settings(): Boolean = json.decodeFromJsonElement<SharingSetting>(request("sharing/settings")).enabled
    suspend fun save(enabled: Boolean): Boolean = json.decodeFromJsonElement<SharingSetting>(request("sharing/settings", enabled = enabled)).enabled.also { require(it == enabled) }
    suspend fun management(kind: String): JsonObject { require(kind in setOf("status", "imports", "exports")); return request("sharing/$kind") }
    @Serializable private data class SharingSetting(val enabled: Boolean)
    companion object {
        private val json = Json { ignoreUnknownKeys = true }
        fun create(): SharedLibraryClient {
            val auth = Session.playbackAuthorization()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty())
            return SharedLibraryClient(auth, Net.profileClient(requireNotNull(auth.token)))
        }
        fun forTest(transport: OkHttpClient): SharedLibraryClient {
            check(BuildConfig.DEBUG)
            val auth = Session.playbackAuthorization()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty())
            return SharedLibraryClient(auth, transport)
        }
        private fun JsonObject.string(key: String): String = getValue(key).jsonPrimitive.let { require(it.isString); it.content }
        private fun strictReference(item: JsonObject) { listOf("import_id", "server_id", "catalogue_epoch", "library_id", "item_id").forEach { item.string(it) } }
        private fun strictItem(item: JsonObject) {
            strictReference(item.getValue("reference").jsonObject)
            item["parent"]?.takeUnless { it == JsonNull }?.let { strictReference(it.jsonObject) }
        }
    }
}
