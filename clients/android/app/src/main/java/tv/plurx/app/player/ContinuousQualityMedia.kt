package tv.plurx.app.player

import java.io.IOException
import java.net.URI
import java.security.MessageDigest
import java.util.Locale
import kotlinx.serialization.json.*

/** Resolve media only inside the captured parent and verify its bytes against
 * the immutable init or an accepted physical reservation before extraction. */
internal class ContinuousQualityMedia(origin: String, schedulePath: String, family: JsonObject, private val protocol: ContinuousQualityProtocol) {
    private val origin = URI(origin).also {
        require(it.scheme?.lowercase(Locale.ROOT) in setOf("http", "https") && it.host != null &&
            it.rawUserInfo == null && it.rawQuery == null && it.rawFragment == null)
    }
    private val family = Json.parseToJsonElement(family.toString()).jsonObject.also {
        require(ContinuousQualityWire.family(it))
    }
    private val parent = schedulePath.removeSuffix("quality-schedule").also {
        require(schedulePath.endsWith("quality-schedule"))
        val id = it.removePrefix("/api/v1/hls/").removeSuffix("/")
        require(it == "/api/v1/hls/$id/" && ContinuousQualityWire.uuid(id))
    }
    private val video = this.family.getValue("video").jsonArray.map { it.jsonObject }
    private val audio = this.family.obj("audio")
    private val resourcePattern = Regex("(video|audio)/([0-9a-f]{64})/(?:init/([0-9a-f]{64})\\.mp4|segment/([0-9]+)\\.m4s)")

    data class Resource(val role: String, val row: JsonObject, val initialization: Boolean, val segment: Long?) {
        val rendition: String get() = requireNotNull(row.text("rendition_id"))
        fun videoFrontier(): Long {
            check(role == "video" && !initialization)
            return try { Math.multiplyExact(requireNotNull(segment), requireNotNull(row.number("segment_ticks"))) }
                catch (_: ArithmeticException) { throw IOException("Continuous video frontier overflow") }
                .also { if (it > ContinuousQualityWire.MAX_SAFE_INTEGER) throw IOException("Continuous video frontier bound") }
        }
    }
    data class Authorized(val interval: JsonObject, val transactionIds: Set<String>)

    /** Ordinary playlists are handled by Media3. A claimed family media path
     * with a foreign rendition or init identity is a refusal, never ordinary. */
    fun resource(url: String): Resource? {
        val parsed = URI(url)
        if (!sameOrigin(parsed) || parsed.rawUserInfo != null || parsed.rawFragment != null || !parsed.rawPath.startsWith(parent)) return null
        val match = resourcePattern.matchEntire(parsed.rawPath.removePrefix(parent)) ?: return null
        val role = match.groupValues[1]
        val rendition = match.groupValues[2]
        val row = if (role == "video") video.singleOrNull { it.text("rendition_id") == rendition } else audio
        if (row == null || row.text("rendition_id") != rendition) throw IOException("Continuous media outside the captured family")
        val init = match.groupValues[3].takeIf { it.isNotEmpty() }
        if (init != null && init != row.text("init_id")) throw IOException("Continuous init identity")
        val segment = match.groupValues[4].takeIf { it.isNotEmpty() }?.toLongOrNull()
        if (init == null && (segment == null || segment !in 0..ContinuousQualityWire.MAX_SAFE_INTEGER)) throw IOException("Continuous segment index bound")
        return Resource(role, row, init != null, segment)
    }

    fun authorize(resource: Resource, bytes: ByteArray): Authorized? {
        if (bytes.isEmpty() || bytes.size > MAX_MEDIA_BYTES) throw IOException("Continuous media payload bound")
        if (resource.role !in setOf("video", "audio") || resource.initialization != (resource.segment == null))
            throw IOException("Continuous media resource shape")
        // Resource instances cannot substitute a descriptor from another owner.
        val descriptor = if (resource.role == "video") video.singleOrNull { it.text("rendition_id") == resource.rendition } else audio
        if (descriptor == null || descriptor != resource.row) throw IOException("Continuous media descriptor owner")
        val artifact = digest(bytes)
        if (resource.initialization) {
            if (artifact != resource.row.text("init_id")) throw IOException("Continuous init byte identity")
            return null
        }
        val ledger = protocol.ledger ?: throw IOException("Continuous reservation missing")
        if (ledger.obj("attachment")?.get("family_id") != family["family_id"]) throw IOException("Continuous reservation family identity")
        val owners = ledger.getValue("transactions").jsonArray.map { it.jsonObject }
        val owned = if (resource.role == "video") owners.flatMap { owner ->
            owner.getValue("reserved").jsonArray.map { owner to it.jsonObject }
        } else (ledger["shared_audio_reserved"] as? JsonArray).orEmpty().map { null to it.jsonObject }
        val matches = owned.filter { (owner, pin) ->
            pin.text("artifact_id") == artifact && pin.text("rendition_id") == resource.rendition &&
                pin["timescale"] == resource.row["timescale"] && pin.number("byte_length") == bytes.size.toLong() &&
                (owner == null || pin["artifact_id"] !in owner.getValue("disposed").jsonArray)
        }
        val fields = setOf("artifact_id", "rendition_id", "timescale", "from_tick", "through_tick", "byte_length")
        if (matches.isEmpty() || matches.map { it.second.filterKeys(fields::contains) }.toSet().size != 1) throw IOException("Continuous media has no exact undisposed reservation")
        val pin = matches.first().second
        if (resource.role == "video") {
            val start = resource.videoFrontier()
            val through = try { Math.addExact(start, requireNotNull(resource.row.number("segment_ticks"))) }
                catch (_: ArithmeticException) { throw IOException("Continuous video interval overflow") }
            if (pin.number("from_tick") != start || requireNotNull(pin.number("through_tick")) > through) {
                throw IOException("Continuous video segment interval identity")
            }
        }
        return Authorized(JsonObject(pin.filterKeys(fields::contains)), matches.mapNotNull { it.first?.text("transaction_id") }.toSet())
    }

    private fun sameOrigin(uri: URI): Boolean = uri.scheme?.lowercase(Locale.ROOT) == origin.scheme.lowercase(Locale.ROOT) &&
        uri.host?.lowercase(Locale.ROOT) == origin.host.lowercase(Locale.ROOT) && port(uri) == port(origin)
    private fun port(uri: URI): Int = if (uri.port >= 0) uri.port else if (uri.scheme.equals("https", ignoreCase = true)) 443 else 80

    companion object {
        const val MAX_MEDIA_BYTES = 16 * 1024 * 1024
        fun digest(bytes: ByteArray): String = MessageDigest.getInstance("SHA-256").digest(bytes)
            .joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }
    }
}
