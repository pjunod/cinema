package tv.plurx.app.data

import android.content.Context
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.InternalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.serialization.encodeToString
import kotlinx.serialization.json.Json
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
    suspend fun decision(context: PlaybackFileContext, device: Context, query: Map<String, String> = emptyMap()): Result {
        val caps = Caps.snapshot(device).document
        return execute(context, caps, query)
    }
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
    companion object {
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
        @OptIn(InternalCoroutinesApi::class)
        private suspend fun read(request: Request, auth: Session.PlaybackAuthorization, transport: OkHttpClient, current: () -> Unit): ByteArray = withContext(Dispatchers.IO) {
            ensureActive(); current()
            val client = transport.newBuilder().followRedirects(false).followSslRedirects(false).cache(null)
                .cookieJar(okhttp3.CookieJar.NO_COOKIES).authenticator(okhttp3.Authenticator.NONE).build()
            val call = client.newCall(request)
            val registration = Session.observeAuthorizationChanges { call.cancel() }
            val cancellation = coroutineContext[Job]?.invokeOnCompletion(onCancelling = true, invokeImmediately = true) { if (it != null) call.cancel() }
            try {
                require(registration.generation == auth.generation); current(); ensureActive()
                call.execute().use { response ->
                    current(); ensureActive()
                    require(response.request.url == request.url && response.priorResponse == null)
                    if (response.code != 200) throw IllegalStateException("Server returned ${response.code}")
                    val body = requireNotNull(response.body); require(body.contentLength() <= 4_194_304)
                    val source = body.source(); source.request(4_194_305)
                    val bytes = source.buffer.readByteArray(source.buffer.size.coerceAtMost(4_194_305))
                    require(bytes.size <= 4_194_304); current(); ensureActive(); bytes
                }
            } catch (error: Exception) {
                ensureActive(); throw error
            } finally {
                cancellation?.dispose(); Session.removeAuthorizationObserver(registration.id); call.cancel()
            }
        }
    }
}
