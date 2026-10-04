package tv.plurx.app.data

import android.content.Context
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.*
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.MediaType.Companion.toMediaType
import tv.plurx.app.BuildConfig
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import kotlin.coroutines.coroutineContext

/** Captured authenticated B account; Source IDs never enter a Local Decision. */
internal class SharedDecisionClient private constructor(private val auth: Session.PlaybackAuthorization, private val transport: OkHttpClient) {
    private fun requireCurrent() { require(Session.playbackAuthorization() == auth && !auth.token.isNullOrEmpty()) }
    fun playlistUrl(playback: SharedStartedPlayback): String {
        requireCurrent(); playback.start.validated(playback.context)
        val reference = requireNotNull(playback.context.reference)
        playback.context.validateSharedReference(reference, playback.context.sourceFileId, requireNotNull(playback.context.revision))
        require(playback.context.sessionId == playback.start.response.session_id)
        return auth.origin + playback.start.response.playlist_url
    }
    suspend fun status(playback: SharedStartedPlayback): SharedPlaybackStatus {
        playlistUrl(playback)
        val request = Request.Builder().url("${auth.origin}/api/v1/hls/${playback.start.response.session_id}/status")
            .header("Authorization", "Bearer ${auth.token}").build()
        val reply = readResponse(request, auth, transport, maxBytes = 65_536) { playlistUrl(playback) }
        requireCurrent()
        return SharedPlaybackStatus.decode(reply.bytes, playback)
    }
    /** The byte URL of a started direct play, played with no account headers. */
    fun directUrl(direct: SharedStartedDirect): String {
        requireCurrent(); direct.validateBound()
        return auth.origin + direct.url
    }
    class ControlReply(val status: Int, val text: String)
    /** One control exchange on the B tuple's own route; the status is the answer. */
    suspend fun control(playback: SharedStartedPlayback, body: String): ControlReply {
        playlistUrl(playback)
        val path = requireNotNull(playback.start.response.control).url
        require(path == "/api/v1/hls/${playback.sessionId}/control" && body.toByteArray().size <= 65_536)
        val request = Request.Builder().url(auth.origin + path).header("Authorization", "Bearer ${auth.token}")
            .post(body.toRequestBody("application/json".toMediaType())).build()
        val reply = readResponse(request, auth, transport, statuses = setOf(200, 400, 409, 410, 422, 425, 429, 503), maxBytes = 16_384) { playlistUrl(playback) }
        requireCurrent()
        return ControlReply(reply.status, strictUtf8(reply.bytes))
    }
    /** Best-effort B End reply is never a physical retirement proof. */
    suspend fun end(playback: SharedBoundSession) {
        requireCurrent(); playback.validateBound()
        val request = Request.Builder().url("${auth.origin}/api/v1/hls/${playback.sessionId}")
            .header("Authorization", "Bearer ${auth.token}").delete().build()
        readResponse(request, auth, transport, statuses = setOf(200, 202, 204), maxBytes = 16_384) { requireCurrent() }
    }
    suspend fun orderedProgress(playback: SharedBoundSession, initialWatchSequence: Long,
                                positionMs: Long, durationMs: Long?, watched: Boolean = false): SharedProgressResult? {
        requireCurrent()
        val reference = requireNotNull(playback.context.reference)
        playback.context.validateSharedReference(reference, playback.context.sourceFileId, requireNotNull(playback.context.revision))
        playback.validateBound()
        val key = WatchKey(reference.server_id, reference.catalogue_epoch, reference.item_id)
        val entry = progressMutex.withLock {
            if (progressAccount != auth) { progressAccount = auth; progressEntries.clear() }
            if (progressActive >= 4) return null
            if (key !in progressEntries) {
                if (progressEntries.size >= 256) {
                    val victim = progressEntries.filter { !it.value.busy && it.value.order.pending == null }.minByOrNull { it.value.lastUse }?.key ?: return null
                    progressEntries.remove(victim)
                }
                progressEntries[key] = ProgressEntry(SharedProgressOrder(initialWatchSequence))
            }
            val entry = progressEntries.getValue(key)
            if (entry.busy) return null
            entry.busy = true; progressActive++; progressSerial++; entry.lastUse = progressSerial; entry
        }
        try {
            if (entry.order.needsResync || entry.order.pending?.let { it.session_id != playback.context.sessionId } == true) {
                val text = detail(reference, auth, transport); requireCurrent()
                val detail = Net.json.decodeFromString<SharedLibraryDetail>(text); detail.validate(reference)
                require(detail.lifecycle_generation == playback.context.lifecycleGeneration)
                val fresh = detail.watch?.sequence ?: 0
                if (!entry.order.needsResync) entry.order = SharedProgressOrder(maxOf(entry.order.sequence, fresh))
                else entry.order.resync(fresh)
            }
            val beat = entry.order.beat(playback.sessionId, positionMs, durationMs, watched)
            val result = progress(playback, beat); requireCurrent(); entry.order.complete(beat, result)
            if (result == SharedProgressResult.Acknowledged && (beat.position_ms != positionMs || beat.duration_ms != durationMs || beat.watched != watched)) return SharedProgressResult.PreviousBeatAcknowledged
            return result
        } finally {
            withContext(NonCancellable) { progressMutex.withLock { entry.busy = false; progressActive-- } }
        }
    }
    suspend fun progress(playback: SharedBoundSession, beat: SharedProgressBeat): SharedProgressResult {
        requireCurrent(); beat.validate()
        val context = playback.context
        val reference = requireNotNull(context.reference)
        require(context.sessionId == beat.session_id && playback.sessionId == beat.session_id)
        context.validateSharedReference(reference, context.sourceFileId, requireNotNull(context.revision))
        playback.validateBound()
        val bytes = Net.json.encodeToString(beat); require(bytes.toByteArray().size <= 1024)
        val request = Request.Builder().url("${auth.origin}/api/v1/shared/imports/${reference.import_id}/items/${reference.item_id}/progress")
            .header("Authorization", "Bearer ${auth.token}").post(bytes.toRequestBody("application/json".toMediaType())).build()
        val response = readResponse(request, auth, transport, statuses = setOf(200, 409), maxBytes = 16_384) { requireCurrent() }
        requireCurrent()
        if (response.status == 200) {
            Json.parseToJsonElement(strictUtf8(response.bytes)).jsonObject
            return SharedProgressResult.Acknowledged
        }
        val refusal = Json.parseToJsonElement(strictUtf8(response.bytes)).jsonObject
        val code = refusal["code"]?.jsonPrimitive?.content
        require(code == "sharing_progress_stale" || code == "sharing_progress_conflict")
        val current = refusal["current_sequence"]?.let { value ->
            if (value == JsonNull) null else { val primitive = value.jsonPrimitive; require(!primitive.isString); requireNotNull(primitive.longOrNull) }
        }
        require(current == null || current in 0..9_007_199_254_740_991L)
        return SharedProgressResult.ResyncRequired(current)
    }
    suspend fun decision(context: PlaybackFileContext, device: Context, query: Map<String, String> = emptyMap()): Result {
        val caps = Caps.snapshot(device).document
        return execute(context, caps, query)
    }
    /** Initial authenticated Shared Start; unsupported Local control fields are
     * refused without rewriting the original request or issuing a network call. */
    suspend fun start(context: PlaybackFileContext, body: CreateSessionReq): SharedStartedPlayback {
        require(body.presentation == "vod")
        val (text, retained) = postStart(context, body)
        return SharedStart.decode(text).bindInitial(context, retained)
    }
    /** Shared direct play: the same ordinary Start with `presentation: direct`
     * and none of the HLS-only fields the Source refuses for raw bytes. */
    suspend fun startDirect(context: PlaybackFileContext, body: CreateSessionReq): SharedStartedDirect {
        require(body.presentation == "direct" && body.copy == null && body.height == null && body.native_subtitles == null
            && body.subtitle == null && body.block_budget_secs == null)
        val (text, retained) = postStart(context, body)
        return SharedStartedDirect.decode(text, context, retained)
    }
    private suspend fun postStart(context: PlaybackFileContext, body: CreateSessionReq): Pair<String, CreateSessionReq> {
        coroutineContext.ensureActive(); requireCurrent()
        val reference = requireNotNull(context.reference)
        require(context.sessionId == null && requireNotNull(context.lifecycleGeneration) > 0)
        context.validateSharedReference(reference, context.sourceFileId, requireNotNull(context.revision))
        require(body.intent == null && body.previous_session_id == null && body.control_sequence == null && body.reopen_reason == null
            && body.subtitle_burn == null && body.hdr10 != true && body.preserve_dolby_vision != true) { "This Shared playback change is not available yet." }
        require(body.caps?.v == 2 && body.playback_id.isNotEmpty()
            && body.playback_id.toByteArray().size <= 128 && body.playback_id.none { it.code < 32 || it.code == 127 })
        require(body.request_id?.let { Regex("[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}").matches(it) } == true)
        val encoded = Net.json.encodeToString(body); require(encoded.toByteArray().size <= 24_576)
        val retained = Net.json.decodeFromString<CreateSessionReq>(encoded)
        val request = Request.Builder().url(auth.origin + context.path("hls/sessions"))
            .header("Authorization", "Bearer ${auth.token}").post(encoded.toRequestBody("application/json".toMediaType())).build()
        val bytes = read(request, auth, transport, initialStart = true) { requireCurrent(); context.path("hls/sessions") }
        coroutineContext.ensureActive(); requireCurrent()
        return strictUtf8(bytes) to retained
    }
    /** A reopen asks again with the plan's retained caps, never a fresh probe. */
    suspend fun redecide(context: PlaybackFileContext, caps: DeviceCaps, query: Map<String, String>): Result = execute(context, caps, query)
    suspend fun decisionForTest(context: PlaybackFileContext, caps: DeviceCaps, query: Map<String, String> = emptyMap()): Result {
        check(BuildConfig.DEBUG); return execute(context, caps, query)
    }
    class Result(val decision: SharedDecision, val caps: DeviceCaps)
    private suspend fun execute(context: PlaybackFileContext, caps: DeviceCaps, query: Map<String, String>): Result {
        coroutineContext.ensureActive(); requireCurrent()
        val reference = requireNotNull(context.reference)
        context.validateSharedReference(reference, context.sourceFileId, requireNotNull(context.revision))
        require(requireNotNull(context.lifecycleGeneration) > 0 && caps.v == 2)
        val parameters = query.toMap()
        parameters.forEach { (key, value) ->
            val integer = value.toIntOrNull(); val canonical = integer?.toString() == value
            require(when (key) {
                "force" -> value in setOf("auto", "original", "transcode")
                "audio" -> canonical && integer!! in 0..4095
                "subtitle" -> canonical && integer!! in -1..4095
                "audio_offset_ms" -> canonical && integer!! in -15000..15000
                else -> false
            })
        }
        // Snapshot the exact serialized capabilities; mutable caller collections
        // cannot alter the retained facts while the request is suspended.
        val encoded = Json.encodeToString(DecisionCapsReq(caps))
        require(encoded.toByteArray().size <= 131_072)
        val boundCaps = Json.decodeFromString<DecisionCapsReq>(encoded).caps
        val request = Request.Builder().url(auth.origin + context.path("decision", parameters))
            .header("Authorization", "Bearer ${auth.token}").post(encoded.toRequestBody("application/json".toMediaType())).build()
        val bytes = read(request, auth, transport) { requireCurrent(); context.path("decision") }
        coroutineContext.ensureActive(); requireCurrent()
        val text = strictUtf8(bytes)
        val result = SharedDecision.decode(text).validated(context); requireCurrent()
        return Result(result, boundCaps)
    }
    private data class WatchKey(val server: String, val epoch: String, val item: String)
    private class ProgressEntry(var order: SharedProgressOrder, var busy: Boolean = false, var lastUse: Long = 0)
    companion object {
        private val progressMutex = Mutex()
        private var progressAccount: Session.PlaybackAuthorization? = null
        private val progressEntries = mutableMapOf<WatchKey, ProgressEntry>()
        private var progressActive = 0
        private var progressSerial = 0L

        fun create(): SharedDecisionClient {
            val auth = Session.playbackAuthorization()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty())
            return SharedDecisionClient(auth, Net.profileClient(requireNotNull(auth.token)))
        }
        fun forTest(transport: OkHttpClient): SharedDecisionClient {
            check(BuildConfig.DEBUG)
            val auth = Session.playbackAuthorization()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty())
            return SharedDecisionClient(auth, transport)
        }
        internal suspend fun detail(reference: SharedPlaybackReference, auth: Session.PlaybackAuthorization, transport: OkHttpClient): String {
            reference.validate()
            require(Session.canonicalOrigin(auth.origin) != null && !auth.token.isNullOrEmpty() && Session.playbackAuthorization() == auth)
            val request = Request.Builder().url("${auth.origin}/api/v1/shared/imports/${reference.import_id}/items/${reference.item_id}")
                .header("Authorization", "Bearer ${auth.token}").build()
            return strictUtf8(read(request, auth, transport) { require(Session.playbackAuthorization() == auth) })
        }
        private fun strictUtf8(bytes: ByteArray): String = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
        private data class HTTPResponse(val bytes: ByteArray, val status: Int)
        private suspend fun read(request: Request, auth: Session.PlaybackAuthorization, transport: OkHttpClient, initialStart: Boolean = false, current: () -> Unit): ByteArray =
            readResponse(request, auth, transport, initialStart, current = current).bytes
        @OptIn(InternalCoroutinesApi::class)
        private suspend fun readResponse(request: Request, auth: Session.PlaybackAuthorization, transport: OkHttpClient, initialStart: Boolean = false, statuses: Set<Int> = setOf(200), maxBytes: Int = 4_194_304, current: () -> Unit): HTTPResponse = withContext(Dispatchers.IO) {
            ensureActive(); current()
            val builder = transport.newBuilder().followRedirects(false).followSslRedirects(false).cache(null)
                .cookieJar(okhttp3.CookieJar.NO_COOKIES).authenticator(okhttp3.Authenticator.NONE)
            if (initialStart) builder.callTimeout(310, java.util.concurrent.TimeUnit.SECONDS).readTimeout(310, java.util.concurrent.TimeUnit.SECONDS)
            val client = builder.build()
            val call = client.newCall(request)
            val registration = Session.observeAuthorizationChanges { call.cancel() }
            val cancellation = coroutineContext[Job]?.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { if (it != null) call.cancel() }
            try {
                require(registration.generation == auth.generation); current(); ensureActive()
                call.execute().use { response ->
                    current(); ensureActive()
                    require(response.request.url == request.url && response.priorResponse == null)
                    if (response.code !in statuses) throw IllegalStateException("Server returned ${response.code}")
                    val body = requireNotNull(response.body); require(body.contentLength() <= maxBytes)
                    val source = body.source(); source.request(maxBytes.toLong() + 1)
                    val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(maxBytes.toLong() + 1))
                    require(bytes.size <= maxBytes); current(); ensureActive(); HTTPResponse(bytes, response.code)
                }
            } catch (error: Exception) {
                ensureActive(); throw error
            } finally {
                cancellation?.dispose(); Session.removeAuthorizationObserver(registration.id); call.cancel()
            }
        }
    }
}
