package tv.plurx.app.livetv

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.serialization.Serializable
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import okhttp3.HttpUrl
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Net
import tv.plurx.app.data.Session
import java.util.concurrent.TimeUnit

@Serializable
data class LiveTvChannel(
    val id: String,
    val guide_number: String,
    val guide_name: String,
    val favorite: Boolean = false,
    val drm: Boolean = false,
    val support: String = "ready",
) {
    val title: String get() = "$guide_number · $guide_name"
    val watchable: Boolean get() = !drm && support == "ready"
}

@Serializable
data class LiveTvLineup(val channels: List<LiveTvChannel>, val freshness: String = "fresh")

@Serializable
data class LiveTvStarted(val session_id: String, val channel: LiveTvChannel, val live: Boolean = false)

@Serializable
data class LiveTvStatus(val state: String)

@Serializable
data class LiveTvSettings(
    val live_tv_enabled: Boolean = false,
    val live_tv_device_ipv4: String = "",
    val live_tv_owner_node_id: String = "",
    val live_tv_max_sessions: Int = 2,
    val live_tv_output_height: Int = 720,
    val live_tv_config_generation: Long = 0,
    val live_tv_transition_from_owner_node_id: String = "",
    val live_tv_transition_drain_before: Long = 0,
)

@Serializable
data class LiveTvReadiness(val ready: Boolean, val generation: Long, val checks: List<LiveTvCheck>)

@Serializable
data class LiveTvCheck(val id: String, val ready: Boolean, val message: String)

class LiveTvFailure(val code: String) : Exception(liveTvMessage(code))

internal fun liveTvMessage(code: String): String = when (code) {
    "live_tv_disabled" -> "Live TV is disabled. An administrator can enable it in Settings → Developer."
    "tuner_capacity" -> "All Live TV slots are busy. Stop another session and try again."
    "owner_unavailable" -> "The tuner owner is unavailable. Check its network and cluster health."
    "start_outcome_unknown" -> "Start or cleanup is unconfirmed. Wait 90 seconds before retrying; restarting or changing profiles does not bypass this safety period."
    "storage_unavailable" -> "Live TV cannot safely save its pending-start marker. Check device storage and restart the app."
    "drm_unsupported" -> "DRM-protected channels are unsupported. Select an unprotected channel."
    "codec_unsupported" -> "This stream's audio or video cannot be decoded by the server or this device. Try an ATSC 1.0 channel."
    "capability_expired" -> "The live session expired. Select the channel again."
    "admin_required" -> "An administrator account is required to configure Live TV."
    "settings_conflict" -> "Settings changed on another client. Reload before editing."
    "channel_not_found" -> "This channel is no longer in the saved lineup. Reload channels."
    "startup_timeout" -> "The tuner did not produce playable media in time."
    "invalid_settings" -> "Check the private IPv4 address, voter node ID, session budget and output height."
    else -> "The live stream could not continue. Stop and select a channel again."
}

sealed interface LiveTvSettingsChange {
    data class Configure(val ipv4: String, val owner: String, val sessions: Int, val height: Int) : LiveTvSettingsChange
    data class Enabled(val enabled: Boolean) : LiveTvSettingsChange
    data class FencedOwner(val owner: String, val cutoff: Long) : LiveTvSettingsChange

    fun body(generation: Long): JsonObject = buildJsonObject {
        put("live_tv_config_generation", generation)
        when (val change = this@LiveTvSettingsChange) {
            is Configure -> {
                put("live_tv_device_ipv4", change.ipv4)
                put("live_tv_owner_node_id", change.owner)
                put("live_tv_max_sessions", change.sessions)
                put("live_tv_output_height", change.height)
            }
            is Enabled -> put("live_tv_enabled", change.enabled)
            is FencedOwner -> putJsonObject("live_tv_fenced_owner") {
                put("owner_node_id", change.owner)
                put("drain_before_generation", change.cutoff)
                put("stopped_and_restart_prevented", true)
            }
        }
    }
}

interface LiveTvRequests {
    suspend fun start(channel: String): LiveTvStarted
    suspend fun release(capability: String)
}

/** Immutable profile-bound API. Narrow capabilities never inherit Session.token. */
class LiveTvApi(origin: String, private val token: String) : LiveTvRequests {
    private val base = (Session.canonicalOrigin(origin) ?: throw LiveTvFailure("invalid_settings")).toHttpUrl()
    private val client: OkHttpClient = Net.capabilityClient.newBuilder()
        .connectTimeout(8, TimeUnit.SECONDS).readTimeout(45, TimeUnit.SECONDS)
        .callTimeout(45, TimeUnit.SECONDS).build()
    val mediaClient: OkHttpClient = Net.capabilityClient.newBuilder()
        .connectTimeout(8, TimeUnit.SECONDS).readTimeout(15, TimeUnit.SECONDS)
        .callTimeout(20, TimeUnit.SECONDS).build()

    private fun url(vararg parts: String): HttpUrl = base.newBuilder()
        .addPathSegments("api/v1").apply { parts.forEach(::addPathSegment) }.build()

    internal fun playlistUrl(capability: String): String {
        if (capability.isEmpty() || capability.length > 1024) throw LiveTvFailure("start_outcome_unknown")
        return url("live-tv", "sessions", capability, "index.m3u8").toString()
    }

    private suspend fun request(
        target: HttpUrl, method: String = "GET", authenticated: Boolean = false,
        body: JsonObject? = null, timeout: Long = 45, starting: Boolean = false,
    ): String = withContext(Dispatchers.IO) {
        try {
            val payload = if (method in setOf("POST", "PUT")) {
                (body?.let { Net.json.encodeToString(it) } ?: "{}").toRequestBody("application/json".toMediaType())
            } else null
            val request = Request.Builder().url(target).method(method, payload)
                .apply { if (authenticated) header("Authorization", "Bearer $token") }.build()
            val call = client.newCall(request)
            call.timeout().timeout(timeout, TimeUnit.SECONDS)
            call.execute().use { response ->
                if (method == "DELETE" && response.code in setOf(404, 410)) return@withContext ""
                val source = response.body?.source()
                if (source?.request(1_048_577) == true) throw LiveTvFailure(if (starting) "start_outcome_unknown" else "stream_failed")
                val text = source?.readUtf8().orEmpty()
                if (!response.isSuccessful) {
                    val code = runCatching { Net.json.decodeFromString<JsonObject>(text)["code"]?.jsonPrimitive?.content }.getOrNull()
                    throw LiveTvFailure(code ?: when {
                        response.code in setOf(401, 403) -> "admin_required"
                        starting -> "start_outcome_unknown"
                        else -> "stream_failed"
                    })
                }
                text
            }
        } catch (error: LiveTvFailure) {
            throw error
        } catch (cancelled: CancellationException) {
            // Cancellation is this coroutine being told to stop, not the tuner
            // failing. Converting it into a typed failure breaks structured
            // concurrency and would let a cancelled start be reported as an
            // ambiguous outcome, arming the barrier for a request that never
            // reached the owner.
            throw cancelled
        } catch (_: Exception) {
            // Never surface a transport exception containing a capability URL.
            throw LiveTvFailure(if (starting) "start_outcome_unknown" else "stream_failed")
        }
    }

    suspend fun lineup(): LiveTvLineup = Net.json.decodeFromString(request(url("live-tv", "channels"), authenticated = true))
    override suspend fun start(channel: String): LiveTvStarted = try {
        Net.json.decodeFromString(request(url("live-tv", "channels", channel, "sessions"), "POST", authenticated = true, starting = true))
    } catch (error: LiveTvFailure) { throw error }
    catch (cancelled: CancellationException) { throw cancelled }
    catch (_: Exception) { throw LiveTvFailure("start_outcome_unknown") }

    override suspend fun release(capability: String) {
        repeat(2) { attempt ->
            try {
                request(url("live-tv", "sessions", capability), "DELETE", timeout = 8)
                return
            } catch (error: LiveTvFailure) { if (attempt == 1) throw error }
        }
    }
    suspend fun keepalive(capability: String) { request(url("live-tv", "sessions", capability, "keepalive"), "PUT", timeout = 15) }
    suspend fun status(capability: String): LiveTvStatus = Net.json.decodeFromString(request(url("live-tv", "sessions", capability, "status"), timeout = 15))
    suspend fun settings(): LiveTvSettings = Net.json.decodeFromString(request(url("settings"), authenticated = true))
    suspend fun save(settings: LiveTvSettings, change: LiveTvSettingsChange): LiveTvSettings = Net.json.decodeFromString(
        request(url("settings"), "PUT", authenticated = true, body = change.body(settings.live_tv_config_generation)),
    )
    suspend fun readiness(): LiveTvReadiness = Net.json.decodeFromString(request(url("live-tv", "readiness", "refresh"), "POST", authenticated = true))
}
