package tv.plurx.app.player

import android.util.Log
import java.io.IOException
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import tv.plurx.app.data.CreateSessionReq
import tv.plurx.app.data.HlsStart
import tv.plurx.app.data.Net
import tv.plurx.app.data.QualityCandidate

/** One captured creating profile. The returned parent owns this transport
 * through terminal reconciliation; ordinary fallback closes it immediately. */
internal class ContinuousEnrollment(origin: String, token: String) : AutoCloseable {
    val profile = ContinuousProfileHttp(origin, token)
    data class Start(val playback: HlsStart, val family: JsonObject, val generation: String,
        val epoch: Long, val schedulePath: String, val primaryRendition: String, val intent: CreateSessionReq)
    private val attempts = LinkedHashMap<String, String>()

    /** The bounded reason this start keeps the ordinary path, or null when a
     * compatible encoded family may be negotiated. */
    private fun declineReason(fileId: Long, body: CreateSessionReq): String? {
        val intent = body.intent ?: return "no_intent"
        return when {
            fileId <= 0 -> "file"
            body.caps == null -> "no_capabilities"
            body.copy == true -> "copy"
            body.hdr10 == true -> "hdr10"
            body.subtitle_burn != null -> "burned_subtitles"
            body.height == null || body.height <= 0 -> "no_height"
            body.subtitle != null || intent.selection.subtitles.mode == SubtitleMode.NATIVE -> "native_subtitles"
            body.previous_session_id != null -> "replacement"
            body.presentation != "vod" -> "presentation"
            intent.selection.quality == QualitySelection.Original -> "original"
            intent.selection.codec !in setOf(CodecPolicy.AUTO, CodecPolicy.H264) -> "codec"
            intent.selection.dynamic_range !in setOf(DynamicRangePolicy.AUTO, DynamicRangePolicy.SDR) -> "dynamic_range"
            else -> null
        }
    }

    suspend fun open(fileId: Long, body: CreateSessionReq): Start? {
        val declined = declineReason(fileId, body)
        if (declined != null) {
            Log.i("PlurxPlayback", "continuous enrollment declined: $declined")
            return null
        }
        val intent = requireNotNull(body.intent)
        val request = Json.parseToJsonElement(Net.json.encodeToString(body)).jsonObject
        val catalog = try { profile.request("/api/v1/files/$fileId/hls/continuous-candidates", buildJsonObject {
            put("version", 1); put("start", request)
        }) } catch (error: ContinuousQualityHttpFailure) {
            if (error.status in setOf(404, 405)) return null else throw error
        }
        if (catalog.number("version") != 1L) throw IOException("Continuous catalog version")
        val candidates = (catalog["candidates"] as? JsonArray)?.takeIf { it.size <= 32 }
            ?.map { Net.json.decodeFromJsonElement<QualityCandidate>(it) } ?: throw IOException("Continuous candidate bound")
        if (candidates.any { !it.hasValidIdentity } || candidates.map { it.id }.toSet().size != candidates.size) throw IOException("Continuous candidate identity")
        val pairs = (catalog["pairs"] as? JsonArray)?.takeIf { it.size <= 32 }?.map { it.jsonObject }
            ?: throw IOException("Continuous pair bound")
        val requested = (intent.selection.quality as? QualitySelection.AutoCandidate)?.candidateId
        val candidate = candidates.firstOrNull { it.id == requested && it.target_height == body.height }
            ?: candidates.firstOrNull { it.route == "encode" && it.target_height == body.height && it.grade == "sdr" && it.decoder_compatible }
            ?: return null.also { Log.i("PlurxPlayback", "continuous enrollment declined: no_candidate height=${body.height}") }
        val pair = pairs.firstOrNull { it.text("primary_candidate_id") == candidate.id }
            ?: return null.also { Log.i("PlurxPlayback", "continuous enrollment declined: no_pair candidates=${candidates.size}") }
        val companion = candidates.singleOrNull { it.id == pair.text("companion_candidate_id") } ?: throw IOException("Continuous companion candidate")
        if (companion.id == candidate.id || companion.route != "encode" || companion.grade != "sdr" || !companion.decoder_compatible ||
            candidate.route != "encode" || candidate.grade != "sdr" || !candidate.decoder_compatible) return null
        val key = body.request_id ?: throw IOException("Continuous start request identity missing")
        val generation = attempts.getOrPut(key) {
            if (attempts.size >= 16) attempts.remove(attempts.keys.first())
            ContinuousQualityWire.newId()
        }
        val selection = if (intent.selection.quality is QualitySelection.Manual) QualitySelection.Manual(candidate.target_height)
            else QualitySelection.AutoCandidate(candidate.target_height, candidate.id)
        val start = body.copy(intent = intent.copy(selection = intent.selection.copy(quality = selection)),
            quality_auto = selection !is QualitySelection.Manual)
        val response = try { profile.request("/api/v1/files/$fileId/hls/continuous-sessions", buildJsonObject {
            put("version", 1); put("controlled", true); put("family_generation", generation)
            put("primary_candidate_id", candidate.id); put("companion_candidate_id", companion.id)
            put("start", Json.parseToJsonElement(Net.json.encodeToString(start)))
        }) } catch (error: ContinuousQualityHttpFailure) {
            if (error.status in setOf(404, 405)) return null else throw error
        }
        val playback = response.obj("playback")
        val session = playback?.text("session_id")?.takeIf(ContinuousQualityWire::uuid)
        try {
            if (response.number("version") != 1L || session == null) throw IOException("Continuous playback bootstrap")
            val quality = response.obj("quality") ?: throw IOException("Continuous quality bootstrap")
            val schedule = "/api/v1/hls/$session/quality-schedule"
            if (!ContinuousQualityWire.uuid(quality.text("generation") ?: "") || quality.number("control_epoch") !in 1..ContinuousQualityWire.MAX_SAFE_INTEGER ||
                quality.text("schedule_url") != schedule || quality.text("family_url") != "/api/v1/hls/$session/quality-family") throw IOException("Continuous bootstrap identity")
            val family = profile.request("/api/v1/hls/$session/quality-family", limit = 32768)
            if (!ContinuousQualityWire.family(family) || family.text("mode") != "controlled") throw IOException("Continuous family descriptor")
            val rows = family.getValue("video").jsonArray.map { it.jsonObject }
            val primary = rows.singleOrNull { it.text("candidate_id") == candidate.id } ?: throw IOException("Continuous primary binding")
            if (rows.none { it.text("candidate_id") == companion.id } || primary.number("height") != candidate.height.toLong() ||
                primary.number("width") != candidate.width.toLong()) throw IOException("Continuous family catalog binding")
            val hls = Net.json.decodeFromJsonElement<HlsStart>(requireNotNull(playback))
            if (hls.playlist_url != "/api/v1/hls/$session/master.m3u8" || !hls.vod) throw IOException("Continuous playback presentation")
            return Start(hls.copy(quality_candidates = continuousQualityBoundCatalog(candidates, family)), family, requireNotNull(quality.text("generation")),
                requireNotNull(quality.number("control_epoch")), schedule, requireNotNull(primary.text("rendition_id")), start)
        } catch (error: Exception) {
            if (session != null) withContext(NonCancellable) { withTimeoutOrNull(2000) {
                try { Net.api(profile.origin, profile.http).endHlsSession(session) } catch (_: Exception) { /* Reaper remains fallback. */ }
            } }
            if (error is CancellationException) throw error
            throw error
        }
    }
    override fun close() = profile.close()
}


/** Costs describe the attached continuous presentation, including its shared soundtrack. */
internal fun continuousQualityBoundCatalog(candidates: List<QualityCandidate>, family: JsonObject?): List<QualityCandidate> {
    if (family == null) return candidates
    val audio = family.obj("audio")?.number("peak_bps") ?: 0L
    val costs = family.getValue("video").jsonArray.associate { value ->
        val row = value.jsonObject
        val video = row.number("peak_bps") ?: throw IOException("Continuous video delivery budget")
        val peak = video + audio
        if (video <= 0 || audio < 0 || peak !in 1..ContinuousQualityWire.MAX_SAFE_INTEGER) {
            throw IOException("Continuous delivery budget bound")
        }
        requireNotNull(row.text("candidate_id")) to peak
    }
    return candidates.map { row -> costs[row.id]?.let { row.copy(peak_bps = it) } ?: row }
}
