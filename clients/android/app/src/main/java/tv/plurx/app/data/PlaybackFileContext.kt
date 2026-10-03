package tv.plurx.app.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import tv.plurx.app.BuildConfig
import java.util.UUID

@Serializable
data class SharedPlaybackReference(
    val import_id: String, val server_id: String, val catalogue_epoch: String,
    val library_id: String, val item_id: String,
) {
    fun validate() {
        require(listOf(import_id, server_id, catalogue_epoch).all(PlaybackFileContext::canonicalUuid))
        require(listOf(library_id, item_id).all(PlaybackFileContext::canonicalId))
    }
}

/** Only an authenticated B detail fetch can construct a Shared context. */
class PlaybackFileContext private constructor(
    val reference: SharedPlaybackReference?, val sourceFileId: String,
    val revision: String?, val fileBase: String, val sessionId: String? = null,
    private val authorization: Session.PlaybackAuthorization? = null,
) {
    val sourceKey: String get() = if (reference == null) sourceFileId else listOf(
        authorization!!.generation, reference.import_id, reference.server_id, reference.catalogue_epoch,
        reference.library_id, reference.item_id, sourceFileId, revision, fileBase,
    ).joinToString("|")

    private fun requireCurrent() {
        if (reference != null) {
            val bound = requireNotNull(authorization)
            require(Session.playbackAuthorization() == bound && bound.token != null)
        }
    }
    fun withSession(id: String): PlaybackFileContext {
        requireCurrent()
        require(Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(id))
        return PlaybackFileContext(reference, sourceFileId, revision, fileBase, id, authorization)
    }
    fun path(resource: String): String {
        requireCurrent()
        require(Regex("(decision|hls/sessions|direct|stream\\.mp4|subs/[0-9]{1,6}(\\.vtt|/overlay\\.json|/overlay/[0-9a-f]{64}/objects/[0-9a-f]{64}\\.png)?|chapters/[0-9]{1,6}/thumb)").matches(resource))
        if (reference != null && resource in setOf("direct", "stream.mp4")) require(sessionId != null)
        val session = if (sessionId != null && resource !in setOf("decision", "hls/sessions")) "?session=$sessionId" else ""
        return "$fileBase/$resource$session"
    }
    fun apiPath(resource: String): String = path(resource).removePrefix("/api/v1/")

    companion object {
        fun canonicalId(value: String): Boolean = value.toLongOrNull()?.let { it >= 0 && it.toString() == value } == true
        fun canonicalUuid(value: String): Boolean = runCatching { UUID.fromString(value).toString() == value }.getOrDefault(false)
        fun local(id: Long): PlaybackFileContext = local(id.toString())
        fun local(id: String): PlaybackFileContext {
            require(canonicalId(id))
            return PlaybackFileContext(null, id, null, "/api/v1/files/$id")
        }
        suspend fun authenticatedDetail(reference: SharedPlaybackReference, fileId: String): PlaybackFileContext {
            val auth = Session.playbackAuthorization()
            return fetch(reference, fileId, auth, Net.profileClient(requireNotNull(auth.token)))
        }
        /** Mock HTTP seam is unavailable in release builds; never accepts a raw locator. */
        internal suspend fun authenticatedDetailForTest(reference: SharedPlaybackReference, fileId: String,
                                                       client: OkHttpClient): PlaybackFileContext {
            check(BuildConfig.DEBUG)
            return fetch(reference, fileId, Session.playbackAuthorization(), client)
        }
        private suspend fun fetch(reference: SharedPlaybackReference, fileId: String,
                                  auth: Session.PlaybackAuthorization, client: OkHttpClient): PlaybackFileContext {
            reference.validate()
            require(canonicalId(fileId) && !auth.token.isNullOrEmpty() && Session.canonicalOrigin(auth.origin) != null)
            val url = "${auth.origin}/api/v1/shared/imports/${reference.import_id}/items/${reference.item_id}"
            val request = Request.Builder().url(url).header("Authorization", "Bearer ${auth.token}").build()
            val detail = withContext(Dispatchers.IO) {
                client.newCall(request).execute().use { response ->
                    require(response.code == 200 && response.request.url == request.url && response.priorResponse == null)
                    val body = requireNotNull(response.body)
                    require(body.contentLength() <= 1_048_576)
                    // Bound actual bytes even when Content-Length is absent.
                    val source = body.source()
                    source.request(1_048_577)
                    val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(1_048_577))
                    require(bytes.size <= 1_048_576)
                    Json.parseToJsonElement(bytes.toString(Charsets.UTF_8)).jsonObject
                }
            }
            require(Session.playbackAuthorization() == auth)
            fun JsonObject.string(key: String): String = getValue(key).jsonPrimitive.let { require(it.isString); it.content }
            val files = detail.getValue("files").jsonArray.map { it.jsonObject }
            val file = files.filter { it.string("file_id") == fileId }.single()
            val binding = file.getValue("reference").jsonObject
            val actual = Json.decodeFromJsonElement<SharedPlaybackReference>(binding.getValue("item"))
            require(actual == reference && binding.string("file_id") == fileId)
            val revision = file.string("revision")
            require(Regex("[0-9a-f]{64}").matches(revision) && binding.string("revision") == revision)
            val base = file.string("file_base")
            val prefix = "/api/v1/shared/imports/${reference.import_id}/files/"
            require(base.startsWith(prefix) && Regex("[A-Za-z0-9_-]{1,2048}").matches(base.removePrefix(prefix)))
            return PlaybackFileContext(reference, fileId, revision, base, authorization = auth)
        }
    }
}
