package tv.plurx.app.data

import kotlinx.serialization.Serializable

@Serializable
internal data class SharedLibraryIdentity(val import_id: String, val server_id: String, val catalogue_epoch: String, val library_id: String) {
    val id get() = listOf(import_id, server_id, catalogue_epoch, library_id).joinToString("|")
    val sourceId get() = listOf(import_id, server_id, catalogue_epoch).joinToString("|")
    fun reference(item: String) = SharedPlaybackReference(import_id, server_id, catalogue_epoch, library_id, item)
    fun validate() = reference("0").validate()
    fun contains(reference: SharedPlaybackReference) = this.reference(reference.item_id) == reference
}
@Serializable
internal data class SharedLibraryAssignment(val import_id: String, val server_id: String, val catalogue_epoch: String,
    val library_id: String, val source_name: String, val availability: String) {
    val identity get() = SharedLibraryIdentity(import_id, server_id, catalogue_epoch, library_id)
    val id get() = identity.id
}
@Serializable
internal data class SharedLibraryMetadata(val library_id: String, val name: String, val kind: String, val anime: Boolean)
internal data class SharedLibraryRow(val identity: SharedLibraryIdentity, val name: String, val sourceName: String, val kind: String)
@Serializable
internal data class SharedLibraryItem(val source: String, val reference: SharedPlaybackReference, val parent: SharedPlaybackReference? = null,
    val title: String, val kind: String, val year: Int? = null, val overview: String? = null, val genres: List<String> = emptyList(),
    val season_number: Int? = null, val episode_number: Int? = null,
    val art: List<SharedArtworkDescriptor>? = null, val poster_url: String? = null, val backdrop_url: String? = null,
    @kotlinx.serialization.Transient val artworkSubject: SharedArtworkSubject? = null) {
    val id get() = listOf(reference.import_id, reference.server_id, reference.catalogue_epoch, reference.library_id, reference.item_id).joinToString("|")
    val hasChildren get() = kind in setOf("series", "season", "album", "artist", "collection")
    fun validate(library: SharedLibraryIdentity) {
        reference.validate(); require(source == "shared" && library.contains(reference) && title.toByteArray().size <= 512 && genres.size <= 64)
        parent?.let { it.validate(); require(library.contains(it)) }
        SharedArtworkDescriptor.validate(art, poster_url, backdrop_url, reference)
    }
}
@Serializable
internal data class SharedLibraryPage(val items: List<SharedLibraryItem>, val next_cursor: String? = null,
    val catalogue_revision: Long, val scope_generation: Long, val catalogue_generation: Long) {
    fun validate(library: SharedLibraryIdentity) {
        require(items.size <= 200 && (next_cursor?.toByteArray()?.size ?: 0) <= 4096 && catalogue_revision >= 0 && scope_generation >= 0 && catalogue_generation >= 0)
        items.forEach { it.validate(library) }
    }
}
@Serializable
internal data class SharedLibraryFile(val file_id: String, val revision: String, val reference: SharedPlaybackFileReference,
    val size: String, val duration_ms: Long? = null, val container: String? = null, val video_codec: String? = null,
    val width: Int? = null, val height: Int? = null, val file_base: String? = null)
@Serializable
internal data class SharedLibraryWatch(val position_ms: Long, val duration_ms: Long? = null, val watched: Boolean,
    val sequence: Long, val updated_at_ms: Long)
@Serializable
internal data class SharedLibraryDetail(val item: SharedLibraryItem, val files: List<SharedLibraryFile>,
    val watch: SharedLibraryWatch? = null, val delivery_status: String, val lifecycle_generation: Long? = null) {
    fun validate(expected: SharedPlaybackReference) {
        val library = SharedLibraryIdentity(expected.import_id, expected.server_id, expected.catalogue_epoch, expected.library_id)
        item.validate(library)
        require(item.reference == expected && files.size <= 64 && files.map { it.file_id }.toSet().size == files.size)
        files.forEach {
            require(PlaybackFileContext.canonicalId(it.file_id) && PlaybackFileContext.canonicalId(it.size) && Regex("[0-9a-f]{64}").matches(it.revision))
            require(it.reference.item == expected && it.reference.file_id == it.file_id && it.reference.revision == it.revision)
        }
        watch?.let { require(it.position_ms >= 0 && it.sequence >= 0 && it.updated_at_ms >= 0 && (it.duration_ms == null || it.duration_ms >= 0)) }
    }
}

/** Readiness has no input into selection or Save availability. */
internal data class SharedSharingDraft(val enabled: Boolean = false, val revision: Long = 0) {
    fun choose(value: Boolean) = copy(enabled = value, revision = revision + 1)
    fun received(value: Boolean, requestedAt: Long) = if (revision == requestedAt) copy(enabled = value) else this
}
internal data class SharedBrowseAccumulator(val items: List<SharedLibraryItem> = emptyList(), val nextCursor: String? = null,
    private val seen: Set<String> = emptySet()) {
    fun append(page: SharedLibraryPage, requestedCursor: String?, library: SharedLibraryIdentity): SharedBrowseAccumulator {
        page.validate(library)
        require(requestedCursor == nextCursor && (page.next_cursor == null || page.next_cursor != requestedCursor))
        val keys = seen.toMutableSet()
        val additions = page.items.filter { keys.add(it.id) }
        return SharedBrowseAccumulator(items + additions, page.next_cursor, keys)
    }
}
