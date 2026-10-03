package tv.plurx.app.data

import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import tv.plurx.app.BuildConfig

internal class SharedSharingManagementClient private constructor(private val auth: Session.PlaybackAuthorization, private val transport: OkHttpClient) {
    fun requireCurrent() { require(Session.playbackAuthorization() == auth && !auth.token.isNullOrEmpty()) }
    private fun id(value: String): String { require(PlaybackFileContext.canonicalUuid(value)); return value }
    private suspend fun request(path: String, method: String = "GET", body: JsonObject? = null, after: String? = null): JsonElement = withContext(Dispatchers.IO) {
        requireCurrent()
        val builder = auth.origin.toHttpUrl().newBuilder().encodedPath("/api/v1/$path")
        if (after != null) builder.addQueryParameter("after", id(after))
        val url = builder.build()
        val request = Request.Builder().url(url).header("Authorization", "Bearer ${auth.token}")
        val payload = body?.toString()?.toByteArray()?.also { require(it.size <= 16_384) { "Sharing update exceeds the server’s 16 KiB mutation limit. No request was sent." } }?.toRequestBody("application/json".toMediaType())
        request.method(method, payload)
        transport.newCall(request.build()).execute().use { response ->
            requireCurrent(); require(response.request.url == url && response.priorResponse == null)
            if (response.code == 401 || response.code == 403) SharedSharingSecretDraft.retireAuthorization(auth.generation)
            val source = requireNotNull(response.body).source()
            val limit = if (path == "libraries" || path == "users" || path.endsWith("/assignments")) 4_194_304L else 131_072L
            source.request(limit + 1); val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(limit + 1))
            require(bytes.size <= limit)
            val text = bytes.toString(Charsets.UTF_8)
            if (!response.isSuccessful) {
                throw (parseRefusal(response.code, text) ?: IllegalStateException("Server returned ${response.code}"))
            }
            Json.parseToJsonElement(text)
        }
    }
    suspend fun libraries(): List<SharedSharingLocalLibrary> = json.decodeFromJsonElement<List<SharedSharingLocalLibrary>>(request("libraries")).also {
        require(it.size <= 4096 && it.map { row -> row.id }.toSet().size == it.size && it.all { row -> row.id >= 0 })
    }.filter { it.kind in listOf("movies", "shows") }
    suspend fun viewers(): List<SharedSharingViewer> = json.decodeFromJsonElement<List<SharedSharingViewer>>(request("users")).also {
        require(it.size <= 4096 && it.map { row -> row.id }.toSet().size == it.size && it.all { row -> row.id >= 0 })
    }
    private fun decodeImport(wire: JsonElement): SharedSharingImport = json.decodeFromJsonElement<SharedSharingImport>(wire).also { it.validate() }
    suspend fun imports(): List<SharedSharingImport> {
        val rows = request("sharing/imports").jsonObject.getValue("imports").jsonArray.map(::decodeImport)
        require(rows.size <= 32 && rows.map { it.`import`.id }.toSet().size == rows.size); return rows
    }
    data class ExportPage(val rows: List<SharedSharingExport>, val next: String?)
    suspend fun exports(after: String? = null): ExportPage {
        val wire = request("sharing/exports", after = after).jsonObject
        val rows = wire.getValue("exports").jsonArray.map { value ->
            val objectValue = value.jsonObject
            objectValue.getValue("library_ids").jsonArray.forEach { require(it.jsonPrimitive.isString) }
            json.decodeFromJsonElement<SharedSharingExport>(objectValue).also { it.validate() }
        }
        require(rows.size <= 32 && rows.map { it.grant.id }.toSet().size == rows.size)
        val next = wire["next"]?.takeUnless { it == JsonNull }?.jsonPrimitive?.let { require(it.isString); id(it.content) }
        require(next == null || next != after); return ExportPage(rows, next)
    }
    private fun invitation(value: String): String {
        require(value.startsWith("cinema-share-v1:") && value.toByteArray().size <= 10939 && value.removePrefix("cinema-share-v1:").matches(Regex("^[A-Za-z0-9_-]{1,10923}$"))) { "Paste a valid cinema-share-v1 invitation." }; return value
    }
    suspend fun invite(libraries: List<String>, ttlSeconds: Long = 86400): SharedSharingInvitation {
        SharedSharingValidation.ids(libraries); require(libraries.isNotEmpty() && ttlSeconds in 1..604800)
        val wire = request("sharing/invitations", "POST", buildJsonObject {
            put("library_ids", JsonArray(libraries.map(::JsonPrimitive))); put("ttl_seconds", ttlSeconds)
        })
        return json.decodeFromJsonElement<SharedSharingInvitation>(wire).also { id(it.id); invitation(it.invitation) }
    }
    suspend fun importSource(value: String): SharedSharingImport = decodeImport(request("sharing/imports", "POST", buildJsonObject { put("invitation", invitation(value)) }))
    suspend fun rePair(row: SharedSharingImportSummary, value: String): SharedSharingImport {
        require(row.lifecycle_generation >= 1)
        return decodeImport(request("sharing/imports/${id(row.id)}/re-pair", "POST", buildJsonObject {
            put("expected_lifecycle_generation", row.lifecycle_generation); put("invitation", invitation(value))
        })).also { require(it.`import`.id == row.id && it.`import`.source_server_id == row.source_server_id && it.`import`.catalogue_epoch == row.catalogue_epoch) }
    }
    suspend fun rotate(row: SharedSharingImportSummary): SharedSharingImport {
        require(row.state == "active") { "Source must be active to rotate its credential." }
        return decodeImport(request("sharing/imports/${id(row.id)}/rotate", "POST", buildJsonObject {})).also { require(it.`import`.id == row.id && it.`import`.source_server_id == row.source_server_id && it.`import`.catalogue_epoch == row.catalogue_epoch) }
    }
    private suspend fun mutation(path: String, method: String, body: JsonObject? = null, resultKey: String = "updated") {
        require(request(path, method, body).jsonObject.getValue(resultKey).jsonPrimitive.boolean)
    }
    suspend fun approve(row: SharedSharingExport, enteredCode: String) {
        row.validate(); require(SharedSharingValidation.code(enteredCode) && enteredCode == row.pairing_code) { "Enter the matching 16-character pairing code shown on the importing server." }
        mutation("sharing/exports/${id(row.grant.id)}/approve", "POST", buildJsonObject {
            put("expected_mutation_generation", row.grant.mutation_generation); put("pairing_code", enteredCode)
        })
    }
    suspend fun scope(row: SharedSharingExport, libraries: List<String>) {
        row.validate(); SharedSharingValidation.ids(libraries)
        mutation("sharing/exports/${id(row.grant.id)}/libraries", "PUT", buildJsonObject {
            put("expected_mutation_generation", row.grant.mutation_generation); put("library_ids", JsonArray(libraries.map(::JsonPrimitive)))
        })
    }
    suspend fun cancelInvitation(value: String) { mutation("sharing/invitations/${id(value)}", "DELETE", resultKey = "cancelled") }
    suspend fun revoke(value: String) { mutation("sharing/exports/${id(value)}", "DELETE", resultKey = "revoked") }
    suspend fun disconnect(value: String) { mutation("sharing/imports/${id(value)}", "DELETE", resultKey = "disabled") }
    suspend fun endpoints(): SharedSharingEndpointManifest? {
        val wire = request("sharing/endpoints").jsonObject["manifest"]?.takeUnless { it == JsonNull } ?: return null
        return json.decodeFromJsonElement<SharedSharingEndpointManifest>(wire).also {
            require(it.revision >= 0 && it.endpoints.size in 1..4); it.endpoints.forEach { endpoint -> endpoint.validate() }
        }
    }
    private fun endpointBody(endpoints: List<SharedSharingEndpoint>): JsonArray {
        require(endpoints.size in 1..4); endpoints.forEach { it.validate() }
        return JsonArray(endpoints.map { endpoint -> buildJsonObject {
            put("ipv4", endpoint.ipv4); put("ipv6", endpoint.ipv6?.let(::JsonPrimitive) ?: JsonNull)
            put("ts_fqdn", endpoint.ts_fqdn); put("port", endpoint.port); put("spki_sha256", endpoint.spki_sha256)
        } })
    }
    suspend fun saveManifest(expectedRevision: Long, endpoints: List<SharedSharingEndpoint>) {
        require(expectedRevision in 0 until Long.MAX_VALUE)
        mutation("sharing/endpoints", "PUT", buildJsonObject { put("expected_revision", expectedRevision); put("endpoints", endpointBody(endpoints)) })
    }
    suspend fun saveSourceEndpoints(row: SharedSharingImportSummary, endpoints: List<SharedSharingEndpoint>, confirmNewPins: Boolean) {
        require(row.endpoint_generation in 1 until Long.MAX_VALUE && row.state in listOf("claiming", "pending", "active"))
        val oldPins = row.endpoints.map { it.spki_sha256 }.toSet()
        require(confirmNewPins || endpoints.all { it.spki_sha256 in oldPins }) { "Review and explicitly confirm every new TLS pin before saving." }
        mutation("sharing/imports/${id(row.id)}/endpoints", "PUT", buildJsonObject {
            put("expected_endpoint_generation", row.endpoint_generation); put("endpoints", endpointBody(endpoints)); put("confirm_new_pins", confirmNewPins)
        })
    }
    suspend fun assignments(row: SharedSharingImportSummary): SharedSharingAssignmentSnapshot {
        val wire = request("sharing/imports/${id(row.id)}/assignments")
        wire.jsonObject.getValue("assignments").jsonArray.forEach { require(it.jsonObject.getValue("library_id").jsonPrimitive.isString) }
        return json.decodeFromJsonElement<SharedSharingAssignmentSnapshot>(wire).also { it.validate(row) }
    }
    suspend fun sourceLibraries(row: SharedSharingImportSummary): SharedSharingSourceLibrariesSnapshot {
        require(row.state == "active")
        val wire = request("sharing/imports/${id(row.id)}/libraries")
        wire.jsonObject.getValue("libraries").jsonArray.forEach { require(it.jsonObject.getValue("library_id").jsonPrimitive.isString) }
        return json.decodeFromJsonElement<SharedSharingSourceLibrariesSnapshot>(wire).also { it.validate(row) }
    }
    suspend fun saveAssignments(snapshot: SharedSharingAssignmentSnapshot, row: SharedSharingImportSummary, groups: List<SharedSharingAssignmentGroup>) {
        snapshot.validate(row); SharedSharingValidation.assignments(groups)
        mutation("sharing/imports/${id(row.id)}/assignments", "PUT", buildJsonObject {
            put("expected_assignment_generation", snapshot.expected_assignment_generation)
            put("assignments", JsonArray(groups.map { group -> buildJsonObject {
                put("library_id", group.library_id); put("user_ids", JsonArray(group.user_ids.map(::JsonPrimitive)))
            } }))
        })
    }
    companion object {
        private val json = Json { ignoreUnknownKeys = true }
        fun create(): SharedSharingManagementClient {
            val auth = Session.playbackAuthorization(); require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty())
            return SharedSharingManagementClient(auth, Net.profileClient(requireNotNull(auth.token)))
        }
        fun forTest(transport: OkHttpClient): SharedSharingManagementClient {
            check(BuildConfig.DEBUG); val auth = Session.playbackAuthorization()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty()); return SharedSharingManagementClient(auth, transport)
        }
    }
}
