package tv.plurx.app.data

import kotlinx.serialization.Serializable

@Serializable internal data class SharedSharingEndpoint(val ipv4: String, val ipv6: String? = null, val ts_fqdn: String, val port: Int, val spki_sha256: String) {
    fun validate() {
        val parts = ipv4.split('.')
        val bytes = parts.mapNotNull(String::toIntOrNull)
        require(parts.size == 4 && bytes.size == 4 && parts.zip(bytes).all { (text, value) -> text == value.toString() && value in 0..255 })
        require(bytes[0] == 100 && bytes[1] in 64..127 && port in 1..65535 && spki_sha256.matches(Regex("^[0-9a-f]{64}$")))
        require(ts_fqdn.endsWith(".ts.net"))
        val prefix = ts_fqdn.removeSuffix(".ts.net"); val labels = prefix.split('.')
        require(prefix.toByteArray().size <= 240 && labels.size >= 2 && labels.all { it.matches(Regex("^[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?$")) })
        if (ipv6 != null) {
            require(ipv6.contains(':') && ipv6.matches(Regex("^[0-9a-fA-F:]+$")))
            val address = java.net.InetAddress.getByName(ipv6)
            require(address is java.net.Inet6Address && address.address.take(6).map { it.toInt() and 255 } == listOf(0xfd, 0x7a, 0x11, 0x5c, 0xa1, 0xe0))
        }
    }
}
@Serializable internal data class SharedSharingEndpointManifest(val revision: Long, val endpoints: List<SharedSharingEndpoint>)
@Serializable internal data class SharedSharingExportGrant(val id: String, val recipient_server_id: String, val state: String,
    val scope_generation: Long, val credential_generation: Long, val catalogue_generation: Long, val mutation_generation: Long, val pending_expires_at_ms: Long)
@Serializable internal data class SharedSharingExport(val grant: SharedSharingExportGrant, val recipient_name: String,
    val invitation_id: String, val claim_id: String, val library_ids: List<String>, val pairing_code: String) {
    fun validate() {
        SharedSharingValidation.ids(library_ids)
        require(listOf(grant.id, grant.recipient_server_id, invitation_id, claim_id).all(PlaybackFileContext::canonicalUuid))
        require(listOf(grant.scope_generation, grant.credential_generation, grant.catalogue_generation, grant.mutation_generation).all { it >= 1 })
        require(grant.state in listOf("pending", "active", "disabled", "revoked") && SharedSharingValidation.code(pairing_code))
    }
}
@Serializable internal data class SharedSharingImportSummary(val id: String, val source_server_id: String, val catalogue_epoch: String,
    val source_name: String, val claim_id: String, val remote_grant_id: String? = null, val state: String,
    val assignment_generation: Long, val lifecycle_generation: Long, val endpoint_generation: Long,
    val observed_endpoint_revision: Long? = null, val endpoints: List<SharedSharingEndpoint>)
@Serializable internal data class SharedSharingImport(val `import`: SharedSharingImportSummary, val pairing_code: String) {
    fun validate() {
        val row = `import`
        row.endpoints.forEach { it.validate() }
        require(listOf(row.id, row.source_server_id, row.catalogue_epoch, row.claim_id).all(PlaybackFileContext::canonicalUuid))
        require(row.remote_grant_id?.let(PlaybackFileContext::canonicalUuid) ?: true)
        require(row.state != "active" || row.remote_grant_id != null)
        require(listOf(row.assignment_generation, row.lifecycle_generation, row.endpoint_generation).all { it >= 1 })
        require(row.endpoints.size in 1..4 && SharedSharingValidation.code(pairing_code))
    }
}
@Serializable internal data class SharedSharingInvitation(val id: String, val invitation: String, val expires_at_ms: Long)
@Serializable internal data class SharedSharingLocalLibrary(val id: Long, val name: String, val kind: String)
@Serializable internal data class SharedSharingViewer(val id: Long, val username: String, val is_admin: Boolean)
@Serializable internal data class SharedSharingAssignmentGroup(val library_id: String, val user_ids: List<Long>)
@Serializable internal data class SharedSharingAssignmentSnapshot(val state: String, val import_id: String, val server_id: String, val catalogue_epoch: String,
    val lifecycle_generation: Long, val expected_assignment_generation: Long, val assignments: List<SharedSharingAssignmentGroup>) {
    fun validate(row: SharedSharingImportSummary) {
        require(import_id == row.id && server_id == row.source_server_id && catalogue_epoch == row.catalogue_epoch)
        require(lifecycle_generation == row.lifecycle_generation && expected_assignment_generation == row.assignment_generation && state == row.state)
        SharedSharingValidation.assignments(assignments)
    }
}
/** Complete admin Source scope, never inferred from the viewer's assigned subset. */
@Serializable internal data class SharedSharingSourceLibrary(val library_id: String, val name: String, val kind: String, val anime: Boolean)
@Serializable internal data class SharedSharingSourceLibrariesSnapshot(val state: String, val import_id: String,
    val server_id: String, val catalogue_epoch: String, val lifecycle_generation: Long,
    val expected_assignment_generation: Long, val libraries: List<SharedSharingSourceLibrary>) {
    fun validate(row: SharedSharingImportSummary) {
        require(state == "active" && state == row.state && import_id == row.id && server_id == row.source_server_id && catalogue_epoch == row.catalogue_epoch)
        require(lifecycle_generation == row.lifecycle_generation && expected_assignment_generation == row.assignment_generation)
        SharedSharingValidation.ids(libraries.map { it.library_id })
        require(libraries.all { it.kind in listOf("movies", "shows") && it.name.toByteArray(Charsets.UTF_8).size <= 256 })
    }
}

/** Keeps historical groups and missing viewers until the operator explicitly removes them. */
internal class SharedSharingAssignmentMatrix(row: SharedSharingImportSummary,
    val snapshot: SharedSharingAssignmentSnapshot, scope: SharedSharingSourceLibrariesSnapshot,
    currentViewers: List<SharedSharingViewer>) {
    data class Library(val id: String, val name: String, val outsideScope: Boolean)
    val libraries: List<Library>
    val viewers: List<SharedSharingViewer>
    var groups: List<SharedSharingAssignmentGroup> = snapshot.assignments.toList(); private set
    var revision = 0L; private set
    init {
        snapshot.validate(row); scope.validate(row)
        require(currentViewers.size <= 4096 && currentViewers.map { it.id }.toSet().size == currentViewers.size && currentViewers.all { it.id >= 0 })
        val currentIds = scope.libraries.map { it.library_id }.toSet()
        libraries = scope.libraries.map { Library(it.library_id, it.name, false) } + snapshot.assignments.filter { it.library_id !in currentIds }.map { Library(it.library_id, "Outside current Source scope · ${it.library_id}", true) }
        val viewerIds = currentViewers.map { it.id }.toSet()
        viewers = currentViewers + (snapshot.assignments.flatMap { it.user_ids }.toSet() - viewerIds).sorted().map { SharedSharingViewer(it, "Unavailable viewer · $it", false) }
    }
    fun contains(library: String, viewer: Long): Boolean = groups.firstOrNull { it.library_id == library }?.user_ids?.contains(viewer) ?: false
    fun set(library: String, viewer: Long, enabled: Boolean) {
        require(libraries.any { it.id == library } && viewers.any { it.id == viewer })
        val ids = groups.firstOrNull { it.library_id == library }?.user_ids.orEmpty().toMutableSet()
        if (enabled) ids.add(viewer) else ids.remove(viewer)
        val group = SharedSharingAssignmentGroup(library, ids.sorted())
        val replacement = if (groups.any { it.library_id == library }) groups.map { if (it.library_id == library) group else it } else groups + group
        SharedSharingValidation.assignments(replacement); groups = replacement; revision++
    }
    fun removeOutsideScope(library: String) {
        require(libraries.any { it.id == library && it.outsideScope })
        groups = groups.filterNot { it.library_id == library }; revision++
    }
    fun accepts(requestedRevision: Long): Boolean = revision == requestedRevision
}

internal object SharedSharingValidation {
    fun ids(values: List<String>) { require(values.size <= 64 && values.toSet().size == values.size && values.all(PlaybackFileContext::canonicalId)) }
    fun code(value: String): Boolean = value.matches(Regex("^[0-9a-f]{16}$"))
    fun assignments(values: List<SharedSharingAssignmentGroup>) {
        ids(values.map { it.library_id })
        require(values.all { it.user_ids.size <= 256 && it.user_ids.toSet().size == it.user_ids.size && it.user_ids.all { id -> id >= 0 } })
    }
}

/** Transient, account-bound pairing material; request completion never overwrites a newer edit. */
internal class SharedSharingSecretDraft private constructor(beforeObserver: (() -> Unit)?) {
    constructor() : this(null)
    data class Snapshot(val revision: Long, val invitation: String, val pairingCode: String)
    private val lock = Any()
    private val invalidation = kotlinx.coroutines.flow.MutableStateFlow(0L)
    val invalidations: kotlinx.coroutines.flow.StateFlow<Long> = invalidation
    private val authorization = Session.playbackAuthorization()
    private var active = true
    private var revision = 0L
    private var invitationValue = ""
    private var codeValue = ""
    private var observation: Long? = null
    init {
        beforeObserver?.invoke()
        val weak = java.lang.ref.WeakReference(this)
        val registration = Session.observeAuthorizationChanges { weak.get()?.retire() }
        observation = registration.id
        if (registration.generation != authorization.generation || !current()) retire()
    }
    private fun current(): Boolean = Session.playbackAuthorization() == authorization && authorization.token != null
    fun edit(invitation: String? = null, pairingCode: String? = null) = synchronized(lock) {
        if (!active || !current()) { clearLocked(); return@synchronized }
        if (invitation != null) invitationValue = invitation
        if (pairingCode != null) codeValue = pairingCode
        revision++; invalidation.value = revision
    }
    fun snapshot(): Snapshot? = synchronized(lock) {
        if (!active || !current()) { clearLocked(); null } else Snapshot(revision, invitationValue, codeValue)
    }
    fun accepts(requestedRevision: Long): Boolean = synchronized(lock) {
        if (!active || !current()) { clearLocked(); false } else revision == requestedRevision
    }
    fun clear(requestedRevision: Long) = synchronized(lock) {
        if (active && current() && revision == requestedRevision) { invitationValue = ""; codeValue = ""; revision++; invalidation.value = revision }
    }
    private fun clearLocked() { active = false; invitationValue = ""; codeValue = ""; revision++ }
    fun retire() {
        synchronized(lock) { clearLocked(); invalidation.value = revision }
    }
    fun leave() {
        retire()
        observation?.let(Session::removeAuthorizationObserver); observation = null
    }
    companion object {
        fun forTest(beforeObserver: () -> Unit): SharedSharingSecretDraft {
            check(tv.plurx.app.BuildConfig.DEBUG); return SharedSharingSecretDraft(beforeObserver)
        }
    }
}
