package tv.plurx.app.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import tv.plurx.app.BuildConfig
import java.util.UUID
import java.net.URI
import java.net.URLDecoder
import java.net.URLEncoder
import java.nio.charset.StandardCharsets

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

    fun localId(expected: Long? = null): Long {
        require(reference == null)
        val id = requireNotNull(sourceFileId.toLongOrNull())
        require(id.toString() == sourceFileId && (expected == null || expected == id))
        return id
    }
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
    fun path(resource: String, query: Map<String, String> = emptyMap()): String {
        requireCurrent()
        require(Regex("(decision|hls/sessions|direct|stream\\.mp4|subs/[0-9]{1,6}(\\.vtt|/overlay\\.json|/overlay/[0-9a-f]{64}/objects/[0-9a-f]{64}\\.png)?|chapters/[0-9]{1,6}/thumb)").matches(resource))
        if (reference != null && resource in setOf("direct", "stream.mp4")) require(sessionId != null)
        if (reference != null) validateQuery(resource, query)
        val parameters = query.toMutableMap()
        if (sessionId != null && resource !in setOf("decision", "hls/sessions")) parameters["session"] = sessionId
        val encoded = parameters.entries.joinToString("&") { (key, value) -> "${encode(key)}=${encode(value)}" }
        return "$fileBase/$resource" + if (encoded.isEmpty()) "" else "?$encoded"
    }
    fun apiPath(resource: String, query: Map<String, String> = emptyMap()): String = path(resource, query).removePrefix("/api/v1/")
    fun translatedDeliveryPath(value: String): String {
        if (reference == null) return value
        requireCurrent()
        val uri = URI(value)
        require(uri.scheme == null && uri.rawAuthority == null && uri.rawFragment == null)
        require(uri.rawPath in setOf("$fileBase/direct", "$fileBase/stream.mp4"))
        val query = linkedMapOf<String, String>()
        uri.rawQuery?.split('&')?.forEach { pair ->
            val parts = pair.split('=', limit = 2); require(parts.size == 2)
            val key = URLDecoder.decode(parts[0], StandardCharsets.UTF_8.toString())
            val text = URLDecoder.decode(parts[1], StandardCharsets.UTF_8.toString())
            require(key !in query); query[key] = text
        }
        if ("session" in query) require(sessionId != null && query.remove("session") == sessionId)
        return path(uri.rawPath.removePrefix("$fileBase/"), query)
    }

    internal fun validateSharedReference(item: SharedPlaybackReference, file: String, revision: String) {
        requireCurrent()
        require(reference != null && reference == item && sourceFileId == file && this.revision == revision)
        require(canonicalId(file) && Regex("[0-9a-f]{64}").matches(revision))
    }
    /** Descriptive validation neither binds a session nor admits delivery. */
    internal fun validateDescriptiveUrl(value: String) {
        requireCurrent()
        require(reference != null && '%' !in value)
        val uri = URI(value)
        require(uri.scheme == null && uri.rawAuthority == null && uri.rawFragment == null)
        require(uri.rawPath in listOf("$fileBase/direct", "$fileBase/stream.mp4", "$fileBase/hls/sessions"))
        val query = closedMetadataQuery(uri)
        query.forEach { (key, text) ->
            if (key == "session") require(sessionId != null && text == sessionId)
            else require(uri.rawPath == "$fileBase/stream.mp4" && key == "audio" && canonicalId(text) && text.toLong() <= 4095)
        }
    }
    internal fun validateSessionPlaylist(value: String, session: String) {
        requireCurrent()
        require(reference != null && sessionId == session && Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(session))
        require('%' !in value && value.length <= 512)
        val uri = URI(value)
        require(uri.scheme == null && uri.rawAuthority == null && uri.rawFragment == null)
        require(uri.rawPath in listOf("master.m3u8", "index.m3u8", "video.m3u8").map { "/api/v1/hls/$session/$it" })
        require((uri.rawQuery?.length ?: 0) <= 256)
        closedMetadataQuery(uri).forEach { (key, text) ->
            require(when (key) {
                "native" -> text in setOf("0", "1")
                "subtitle" -> text == "-1" || (Regex("[0-9]+").matches(text) && text.toLongOrNull()?.let { it <= 4095 } == true)
                "diagnostic" -> text in setOf("video-only", "video-only-codecs", "video-only-range", "video-only-hdr")
                else -> false
            })
        }
    }
    private fun closedMetadataQuery(uri: URI): Map<String, String> {
        val raw = uri.rawQuery ?: return emptyMap()
        require(raw.isNotEmpty())
        val entries = raw.split('&').map {
            val parts = it.split('='); require(parts.size == 2 && parts[0].isNotEmpty() && parts[1].isNotEmpty())
            parts[0] to parts[1]
        }
        require(entries.map { it.first }.toSet().size == entries.size)
        return entries.toMap()
    }
    companion object {
        private fun encode(value: String): String = URLEncoder.encode(value, StandardCharsets.UTF_8.toString()).replace("+", "%20")
        private fun validateQuery(resource: String, query: Map<String, String>) {
            val capabilities = setOf("client", "device", "profile", "vcodec", "vmaxheight", "acodec", "container", "maxheight", "hdr", "dv", "dvprofile", "dvhls", "hdr10t")
            val allowed = when {
                resource == "decision" -> capabilities + setOf("force", "audio", "subtitle", "audio_offset_ms", "achannels", "capver", "hdrtypes", "dvdecoders", "dvraw", "dvstatus")
                resource == "stream.mp4" -> capabilities + setOf("force", "audio", "audio_offset_ms", "start", "stream")
                resource.startsWith("chapters/") -> setOf("v")
                else -> emptySet()
            }
            fun integer(text: String, low: Long, high: Long): Boolean = text.toLongOrNull()?.let { it.toString() == text && it in low..high } == true
            fun match(text: String, pattern: String): Boolean = Regex(pattern).matches(text)
            for ((key, text) in query) {
                require(key in allowed)
                val valid = when (key) {
                    "hdr", "dv", "dvhls", "hdr10t" -> text in setOf("0", "1")
                    "vcodec", "acodec", "container" -> text.length <= 256 && (text.isEmpty() || text.split(',').all { match(it, "[A-Za-z0-9_-]{1,32}") })
                    "dvprofile" -> text.length <= 64 && (text.isEmpty() || text.split(',').all { integer(it, 0, 255) })
                    "vmaxheight" -> text.length <= 256 && (text.isEmpty() || text.split(',').all {
                        val parts = it.split(':'); parts.size == 2 && match(parts[0], "[A-Za-z0-9_-]{1,32}") && integer(parts[1], 1, 65535)
                    })
                    "client", "profile" -> match(text, "[A-Za-z0-9_.-]{1,64}")
                    "device", "capver", "hdrtypes", "dvdecoders", "dvraw", "dvstatus" -> text.length <= 256 && text.none { it.code < 32 || it.code == 127 }
                    "achannels" -> integer(text, 1, 16)
                    "force" -> text in setOf("auto", "original", "transcode")
                    "stream" -> match(text, "[A-Za-z0-9_-]{1,200}")
                    "start" -> match(text, "(0|[1-9][0-9]{0,12})(\\.[0-9]{1,3})?")
                    "v" -> match(text, "-?(0|[1-9][0-9]{0,19})")
                    "subtitle" -> integer(text, -1, 999999)
                    "audio_offset_ms" -> integer(text, -15000, 15000)
                    "maxheight" -> integer(text, 0, 65535)
                    else -> integer(text, 0, 999999)
                }
                require(valid)
            }
        }
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
                    require(body.contentLength() <= 4_194_304)
                    // Bound actual bytes even when Content-Length is absent.
                    val source = body.source()
                    source.request(4_194_305)
                    val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(4_194_305))
                    require(bytes.size <= 4_194_304)
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
