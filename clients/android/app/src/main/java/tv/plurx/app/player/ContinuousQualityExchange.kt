package tv.plurx.app.player

import java.io.ByteArrayOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicBoolean
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import okhttp3.Call
import okhttp3.Callback
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.Request
import okhttp3.RequestBody.Companion.toRequestBody
import okhttp3.Response
import tv.plurx.app.data.Net

/** Keep the creating profile's authority, reject redirects, and read at most
 * one bounded receipt. This client never follows the process-wide login. */
internal class ContinuousQualityExchange(origin: String, schedulePath: String, token: String) : AutoCloseable {
    private val closed = AtomicBoolean()
    private val calls = ConcurrentHashMap.newKeySet<Call>()
    private val originUrl = origin.toHttpUrl().also {
        require(it.username.isEmpty() && it.password.isEmpty() && it.query == null && it.fragment == null)
    }
    private val scheduleUrl = requireNotNull(originUrl.resolve(schedulePath)).also {
        require(Regex("/api/v1/hls/[0-9a-fA-F-]{36}/quality-schedule").matches(schedulePath))
        val session = schedulePath.removePrefix("/api/v1/hls/").removeSuffix("/quality-schedule")
        require(ContinuousQualityWire.uuid(session))
        require(it.scheme == originUrl.scheme && it.host == originUrl.host && it.port == originUrl.port)
        require(it.query == null && it.fragment == null)
    }
    private val http = Net.profileClient(token).newBuilder().callTimeout(14, TimeUnit.SECONDS).build()

    suspend fun exchange(body: JsonObject): JsonObject {
        check(!closed.get()) { "Continuous quality exchange closed" }
        val bytes = body.toString().toByteArray(Charsets.UTF_8)
        require(bytes.size <= MAX_BYTES)
        val request = Request.Builder().url(scheduleUrl)
            .post(bytes.toRequestBody("application/json".toMediaType())).build()
        return suspendCancellableCoroutine { continuation ->
            val call = http.newCall(request)
            calls.add(call)
            if (closed.get()) call.cancel()
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) {
                    calls.remove(call)
                    continuation.resumeWith(Result.failure(e))
                }
                override fun onResponse(call: Call, response: Response) {
                    val result = runCatching {
                        response.use {
                            if (!it.isSuccessful) {
                                val seconds = it.header("Retry-After")?.toLongOrNull()?.coerceIn(0, 1) ?: 1L
                                throw ContinuousQualityHttpFailure(it.code, seconds * 1000)
                            }
                            val payload = it.body ?: throw IOException("Continuous quality response body missing")
                            if (payload.contentLength() > MAX_BYTES) throw IOException("Continuous quality response bound")
                            val bytesOut = ByteArrayOutputStream()
                            payload.byteStream().use { stream ->
                                val buffer = ByteArray(4096)
                                while (true) {
                                    val read = stream.read(buffer)
                                    if (read < 0) break
                                    if (bytesOut.size() + read > MAX_BYTES) throw IOException("Continuous quality response bound")
                                    bytesOut.write(buffer, 0, read)
                                }
                            }
                            val decoder = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
                                .onUnmappableCharacter(CodingErrorAction.REPORT)
                            Json.parseToJsonElement(decoder.decode(ByteBuffer.wrap(bytesOut.toByteArray())).toString()) as? JsonObject
                                ?: throw IOException("Continuous quality response shape")
                        }
                    }
                    calls.remove(call)
                    continuation.resumeWith(if (closed.get()) Result.failure(IOException("Continuous quality exchange closed")) else result)
                }
            })
        }
    }

    override fun close() {
        if (closed.compareAndSet(false, true)) {
            calls.forEach { it.cancel() }
            http.dispatcher.cancelAll()
            http.connectionPool.evictAll()
        }
    }

    companion object { private const val MAX_BYTES = 270_336 }
}
