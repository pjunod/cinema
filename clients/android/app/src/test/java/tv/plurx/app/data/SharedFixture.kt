package tv.plurx.app.data

import kotlinx.serialization.json.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.OkHttpClient
import okhttp3.Protocol
import okhttp3.Request
import okhttp3.Response
import okhttp3.ResponseBody.Companion.toResponseBody
import okio.Buffer

/**
 * A synthetic B for Shared playback tests: the actual authenticated client
 * talks to an interceptor that answers each route the way the server's wire
 * types do. It qualifies client protocol behaviour only, never a Source,
 * a relay or device playback.
 */
internal class SharedFixture(itemId: String) {
    val reference = SharedPlaybackReference("11111111-1111-4111-8111-111111111111", "22222222-2222-4222-8222-222222222222",
        "33333333-3333-4333-8333-333333333333", "9007199254740993", itemId)
    val base = "/api/v1/shared/imports/${reference.import_id}/files/${"L".repeat(236)}"
    val revision = "a".repeat(64)
    val file = "9223372036854775807"
    /** Every request and renderer change, in the order it happened. */
    val log = mutableListOf<String>()
    val bodies = mutableListOf<Pair<String, String>>()
    var starts = 0
    /** Answer for the n-th control exchange (1-based) given the request body. */
    var control: (Int, JsonObject) -> Pair<Int, String> = { _, body -> 200 to accepted(body) }
    var controls = 0
    var startReply: (Int, JsonObject) -> String = { index, body -> if (body["presentation"]?.jsonPrimitive?.content == "direct") directReply(index) else hlsReply(index) }
    var decisionWire: (String) -> String = { decision("remux") }

    fun login() { Session.origin = "https://b.test"; Session.token = "shared-bearer" }
    fun session(index: Int) = "aaaaaaaa-aaaa-4aaa-8aaa-%012d".format(index)
    fun generation(index: Int) = "bbbbbbbb-bbbb-4bbb-8bbb-%012d".format(index)
    fun binding() = buildJsonObject {
        put("item", Json.encodeToJsonElement(reference)); put("file_id", file); put("revision", revision); put("lifecycle_generation", 5)
    }
    fun response(request: Request, text: String, status: Int = 200): Response = Response.Builder().request(request).protocol(Protocol.HTTP_1_1)
        .code(status).message("fixture").body(text.toResponseBody("application/json".toMediaType())).build()
    suspend fun context(): PlaybackFileContext {
        val detail = buildJsonObject { put("lifecycle_generation", 5); put("files", buildJsonArray { add(buildJsonObject {
            put("file_id", file); put("revision", revision); put("file_base", base); put("reference", binding())
        }) }) }.toString()
        return PlaybackFileContext.authenticatedDetailForTest(reference, file, OkHttpClient.Builder().addInterceptor { response(it.request(), detail) }.build())
    }
    fun caps() = capsDocument(videoCodecCaps(listOf(VideoDecoderLimit("h264", 2160), VideoDecoderLimit("hevc", 2160))),
        listOf("aac"), emptySet(), emptyList(), ClientInfo("android", "fixture", "shared fixture"))
    fun decision(method: String, offset: Long = 0, transcodeAudio: Boolean = false): String = buildJsonObject {
        val mode = if (method == "direct_play") "direct" else method
        val url = if (method == "direct_play") "$base/direct" else "$base/stream.mp4"
        put("file_id", file); put("reference", binding()); put("method", method); put("play_url", url)
        put("delivery", buildJsonObject { put("mode", mode); put("url", url); put("sessions_url", "$base/hls/sessions") })
        put("audio", buildJsonArray { add(buildJsonObject { put("index", 1); put("codec", "aac"); put("channels", 2) }); add(buildJsonObject { put("index", 2); put("codec", "ac3"); put("channels", 6) }) })
        put("subtitles", buildJsonArray { add(buildJsonObject { put("index", 4); put("codec", "subrip"); put("native", true) }) })
        put("audio_offset_ms", offset); put("transcode_audio", transcodeAudio); put("delivered_dynamic_range", "sdr")
    }.toString()
    fun hlsReply(index: Int) = buildJsonObject {
        val session = session(index)
        put("session_id", session); put("playlist_url", "/api/v1/hls/$session/master.m3u8"); put("vod", true); put("start_seconds", 0); put("duration_ms", 90_000)
        put("control", buildJsonObject { put("protocol", "plurx-playback-control-v1"); put("url", "/api/v1/hls/$session/control"); put("generation", generation(index)); put("control_epoch", 1); put("next_exchange_ms", 5_000); put("lease_timeout_ms", 300_000) })
    }.toString()
    fun directReply(index: Int, mime: String = "video/x-matroska") = buildJsonObject {
        val session = session(index)
        put("presentation", "direct"); put("session_id", session); put("url", "$base/direct?session=$session"); put("length", 4_096); put("mime", mime)
    }.toString()
    fun accepted(body: JsonObject, preparation: String? = "none") = buildJsonObject {
        put("protocol", "plurx-playback-control-v1"); put("generation", body.getValue("generation")); put("control_epoch", body.getValue("control_epoch"))
        put("accepted_sequence", body.getValue("sequence")); put("action", buildJsonObject { put("type", "none") })
        put("delivery", buildJsonObject { preparation?.let { put("preparation", it) } })
    }.toString()
    fun status(index: Int) = buildJsonObject {
        put("subject", "shared"); put("reference", binding()); put("session_id", session(index)); put("incarnation_id", generation(index)); put("control_epoch", 1)
        put("status", buildJsonObject {
            SharedPlaybackStatus.requiredCounters.forEach { put(it, 0) }
            put("target_height", 720); put("encoder", "copy"); put("playlist_shape", "vod"); put("producer_state", "ready"); put("server_ready_state", "ready")
            put("admitted", true); put("suspended", false); put("final", false)
        })
    }.toString()
    fun transport(): OkHttpClient = OkHttpClient.Builder().addInterceptor { chain ->
        val request = chain.request(); val path = request.url.encodedPath
        val text = request.body?.let { val buffer = Buffer(); it.writeTo(buffer); buffer.readUtf8() } ?: ""
        val name = when {
            path == "$base/hls/sessions" -> "start"
            path == "$base/decision" -> "decision${request.url.query?.let { "?$it" } ?: ""}"
            path.endsWith("/control") -> "control ${path.removePrefix("/api/v1/hls/").removeSuffix("/control")}"
            path.endsWith("/status") -> "status ${path.removePrefix("/api/v1/hls/").removeSuffix("/status")}"
            path.endsWith("/progress") -> "progress"
            request.method == "DELETE" -> "delete ${path.removePrefix("/api/v1/hls/")}"
            else -> "unexpected $path"
        }
        log += name; bodies += name to text
        when {
            name == "start" -> response(request, startReply(++starts, Json.parseToJsonElement(text).jsonObject))
            name.startsWith("decision") -> response(request, decisionWire(request.url.query ?: ""))
            name.startsWith("control") -> control(++controls, Json.parseToJsonElement(text).jsonObject).let { (status, reply) -> response(request, reply, status) }
            name.startsWith("status") -> response(request, status(starts))
            name == "progress" -> response(request, "{}")
            name.startsWith("delete") -> response(request, "", 204)
            else -> response(request, "", 404)
        }
    }.build()
    fun body(name: String, occurrence: Int = 0): JsonObject = Json.parseToJsonElement(bodies.filter { it.first == name }[occurrence].second).jsonObject
    fun plan(context: PlaybackFileContext, method: String = "remux", selection: SharedSelection = SharedSelection(PlaybackQuality.Auto),
             allowDirect: Boolean = false, resumeMs: Long = 12_500, wire: String = decision(method)): SharedPlaybackPlan {
        val result = SharedDecisionClient.Result(SharedDecision.decode(wire), caps())
        return sharedPlaybackPlan(SharedPlaybackSubject(context, "Shared film", resumeMs, 7), result, selection,
            "shared-player", "cccccccc-cccc-4ccc-8ccc-cccccccccccc", allowDirect)
    }
}
