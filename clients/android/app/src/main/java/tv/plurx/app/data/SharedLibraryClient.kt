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
    private suspend fun request(path: String, query: Map<String, String> = emptyMap(), enabled: Boolean? = null, post: JsonObject? = null): JsonObject = withContext(Dispatchers.IO) {
        requireCurrent()
        val builder = auth.origin.toHttpUrl().newBuilder().encodedPath("/api/v1/$path")
        query.forEach { (key, value) -> builder.addQueryParameter(key, value) }
        val url = builder.build()
        val request = Request.Builder().url(url).header("Authorization", "Bearer ${auth.token}")
        if (enabled != null) request.put(buildJsonObject { put("enabled", enabled) }.toString().toRequestBody("application/json".toMediaType()))
        if (post != null) { require(enabled == null); request.post(post.toString().toRequestBody("application/json".toMediaType())) }
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
        val page = json.decodeFromJsonElement<SharedLibraryPage>(wire).also { it.validate(library) }
        requireCurrent(); return page.copy(items = page.items.map { it.copy(artworkSubject = artworkSubject(it)) })
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
        val detail = json.decodeFromJsonElement<SharedLibraryDetail>(wire).also { it.validate(reference) }
        requireCurrent(); return detail.copy(item = detail.item.copy(artworkSubject = artworkSubject(detail.item)))
    }
    /**
     * B-private explicit watched state (contract §5.3). B reads current Source
     * membership under current assignment and takes the next history sequence
     * inside the write, so a progress beat sent before this cannot restore the
     * old position. Source history and Local watch state are untouched.
     */
    suspend fun setWatched(reference: SharedPlaybackReference, watched: Boolean): SharedLibraryWatch {
        reference.validate()
        val wire = request("shared/imports/${reference.import_id}/items/${reference.item_id}/watched", post = buildJsonObject { put("watched", watched) })
        require(wire.getValue("updated").jsonPrimitive.let { !it.isString && it.longOrNull == 1L })
        val watch = json.decodeFromJsonElement<SharedLibraryWatch>(wire.getValue("watch"))
        require(watch.watched == watched && watch.position_ms >= 0 && watch.sequence >= 0 && watch.updated_at_ms >= 0)
        requireCurrent(); return watch
    }

    /**
     * The next episode in Source order, read only through B's viewer routes:
     * the next episode of this season, else the first episode of the next
     * season. Every page is checked against the current assignment by B and
     * against this Source's library here. The answer is a full Shared
     * reference for a fresh authorized Start, never a Local item and never an
     * inherited playback context. Null when there is no next episode.
     */
    suspend fun nextEpisode(reference: SharedPlaybackReference): SharedPlaybackReference? {
        reference.validate()
        val library = SharedLibraryIdentity(reference.import_id, reference.server_id, reference.catalogue_epoch, reference.library_id)
        val current = detail(reference).item
        if (current.kind != "episode") return null
        val season = current.parent ?: return null
        val episodes = children(library, season, "episode")
        val at = episodes.indexOf(reference)
        if (at >= 0 && at + 1 < episodes.size) return episodes[at + 1]
        val show = detail(season).item.parent ?: return null
        val seasons = children(library, show, "season")
        val next = seasons.indexOf(season).takeIf { it >= 0 }?.let { seasons.getOrNull(it + 1) } ?: return null
        return children(library, next, "episode").firstOrNull()
    }

    /** One parent's children of [kind], in the order B pages them, bounded. */
    private suspend fun children(library: SharedLibraryIdentity, parent: SharedPlaybackReference, kind: String): List<SharedPlaybackReference> {
        val rows = mutableListOf<SharedPlaybackReference>()
        var cursor: String? = null
        val seen = mutableSetOf<String>()
        repeat(MAX_CHILD_PAGES) {
            val page = page(library, parent, cursor = cursor)
            page.items.filter { it.kind == kind && it.reference !in rows }.forEach { rows += it.reference }
            cursor = page.next_cursor ?: return rows
            require(seen.add(cursor!!)) { "Shared children unavailable" }
        }
        error("Shared season unavailable")
    }
    private fun artworkSubject(item: SharedLibraryItem): SharedArtworkSubject {
        requireCurrent(); return CapturedSharedArtwork(auth, transport, item)
    }
    suspend fun settings(): Boolean = json.decodeFromJsonElement<SharingSetting>(request("sharing/settings")).enabled
    suspend fun save(enabled: Boolean): Boolean = json.decodeFromJsonElement<SharingSetting>(request("sharing/settings", enabled = enabled)).enabled.also { require(it == enabled) }
    suspend fun management(kind: String): JsonObject { require(kind in setOf("status", "imports", "exports")); return request("sharing/$kind") }
    @Serializable private data class SharingSetting(val enabled: Boolean)
    companion object {
        private val json = Json { ignoreUnknownKeys = true }
        /** Sixty a page: a season of up to 1 200 rows, then a typed refusal rather than a walk. */
        private const val MAX_CHILD_PAGES = 20
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

/** Closed subject implementations live in this authenticated client file. */
internal sealed interface SharedArtworkSubject {
    val reference: SharedPlaybackReference
    val key: String
    val generation: Long
    fun requireCurrent()
    fun descriptor(backdrop: Boolean): SharedArtworkDescriptor?
    suspend fun read(descriptor: SharedArtworkDescriptor): SharedArtworkPayload
}
internal sealed interface SharedArtworkReadPlan {
    val request: Request
    val transport: OkHttpClient
    val generation: Long
    fun requireCurrent()
}
private class CapturedSharedArtwork(private val auth: Session.PlaybackAuthorization, private val transport: OkHttpClient,
    item: SharedLibraryItem) : SharedArtworkSubject {
    private val descriptors = item.art.orEmpty().toList()
    private val poster = item.poster_url
    private val backdropUrl = item.backdrop_url
    override val reference = item.reference.copy()
    override val generation = auth.generation
    override val key = listOf(auth.origin, auth.generation.toString(), reference.import_id, reference.server_id,
        reference.catalogue_epoch, reference.library_id, reference.item_id).joinToString("|")
    override fun requireCurrent() { require(Session.playbackAuthorization() == auth && !auth.token.isNullOrEmpty()) }
    override fun descriptor(backdrop: Boolean): SharedArtworkDescriptor? {
        requireCurrent(); val path = if (backdrop) backdropUrl else poster
        return descriptors.firstOrNull { it.url == path && it.kind == (if (backdrop) "backdrop" else "poster") && it.variant == (if (backdrop) "w780" else "w300") }
    }
    override suspend fun read(descriptor: SharedArtworkDescriptor): SharedArtworkPayload {
        requireCurrent(); descriptor.validate(reference); require(descriptor in descriptors)
        val url = auth.origin.toHttpUrl().newBuilder().encodedPath(descriptor.url).build()
        val request = Request.Builder().url(url).header("Authorization", "Bearer ${auth.token}").build()
        val current = this
        val closedTransport = transport.newBuilder().followRedirects(false).followSslRedirects(false).cache(null)
            .cookieJar(okhttp3.CookieJar.NO_COOKIES).authenticator(okhttp3.Authenticator.NONE).build()
        return SharedArtworkPayload.fetch(CapturedSharedArtworkPlan(request, closedTransport, generation) { current.requireCurrent() })
    }
}
private class CapturedSharedArtworkPlan(override val request: Request, override val transport: OkHttpClient,
    override val generation: Long, private val current: () -> Unit) : SharedArtworkReadPlan {
    override fun requireCurrent() = current()
}

/** Sealed interfaces alone permit package-local implementations. The actual
 * transport boundary accepts only the private authenticated factory product. */
internal fun requireAuthenticatedSharedArtworkPlan(plan: SharedArtworkReadPlan) {
    require(plan is CapturedSharedArtworkPlan); plan.requireCurrent()
}
