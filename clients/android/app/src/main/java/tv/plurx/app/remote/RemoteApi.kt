package tv.plurx.app.remote

import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.*
import okhttp3.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Session
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.concurrent.TimeUnit

internal class RemoteHttpFailure(val outcome: RemoteOutcome, val status: Int, val code: String = outcome.wire) : IOException("Cinema remote request rejected")
internal class RemoteApi(private val authorization: Session.PlaybackAuthorization) {
    companion object {
        fun serverFailure(status: Int, value: JsonObject): RemoteHttpFailure {
            val code = value["code"]?.jsonPrimitive?.contentOrNull?.takeIf { Regex("[a-z_]{1,80}").matches(it) }
            val fallback = when (status) { 401, 403 -> RemoteOutcome.Unauthorized; 409, 429 -> RemoteOutcome.Busy; 400 -> RemoteOutcome.Invalid; else -> RemoteOutcome.Unavailable }
            return RemoteHttpFailure(code?.let(RemoteOutcome::parse) ?: fallback, status, code ?: fallback.wire)
        }
    }
    private val origin = requireNotNull(Session.canonicalOrigin(authorization.origin)).toHttpUrl()
    private val http = OkHttpClient.Builder().followRedirects(false).followSslRedirects(false).cache(null)
        .connectTimeout(10, TimeUnit.SECONDS).readTimeout(25, TimeUnit.SECONDS).callTimeout(30, TimeUnit.SECONDS).build()
    val current get() = Session.playbackAuthorization() == authorization
    private suspend fun request(path: String, method: String = "POST", fields: JsonObject? = null, proof: Pair<String, String>? = null): JsonObject {
        check(current && authorization.token != null)
        val url = origin.newBuilder().encodedPath("/api/remote/v1" + path).query(null).fragment(null).build()
        require(url.scheme == origin.scheme && url.host == origin.host && url.port == origin.port && url.username.isEmpty() && url.password.isEmpty())
        val body = fields?.let { JsonObject(it + ("version" to JsonPrimitive(REMOTE_VERSION))).toString().toByteArray(Charsets.UTF_8) }
        require(body == null || body.size <= if (path == "/presence") 64 * 1024 else 16 * 1024)
        val builder = Request.Builder().url(url).header("Authorization", "Bearer " + authorization.token).header("Cache-Control", "no-store")
        if (proof != null) { require(proof.first in setOf("X-Cinema-Receiver-Secret", "X-Cinema-Grant-Secret", "X-Cinema-Pairing-Secret") && RemoteSecretStorage.validSecret(proof.second)); builder.header(proof.first, proof.second) }
        builder.method(method, if (method == "POST") requireNotNull(body).toRequestBody("application/json".toMediaType()) else null)
        return suspendCancellableCoroutine { continuation ->
            val call = http.newCall(builder.build())
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) { if (continuation.isActive) continuation.resumeWith(Result.failure(e)) }
                override fun onResponse(call: Call, response: Response) {
                    val result = runCatching {
                        response.use {
                            val payload = it.body ?: throw IOException("Missing remote response")
                            require(payload.contentLength() <= 64 * 1024)
                            val out = ByteArrayOutputStream()
                            payload.byteStream().use { stream ->
                                val buffer = ByteArray(4096)
                                while (true) { val count = stream.read(buffer); if (count < 0) break; require(out.size() + count <= 64 * 1024); out.write(buffer, 0, count) }
                            }
                            check(current)
                            val value = RemoteWire.objectBody(out.toByteArray())
                            require(value.string("version") == REMOTE_VERSION)
                            if (!it.isSuccessful) {
                                throw serverFailure(it.code, value)
                            }
                            if ("commands" in value) RemoteWire.pollCommands(value)
                            value
                        }
                    }
                    if (continuation.isActive) continuation.resumeWith(result)
                }
            })
        }
    }
    private fun body(target: RemoteTarget, fields: JsonObject = JsonObject(emptyMap())) = JsonObject(fields + ("target" to RemoteWire.targetJson(target)))
    private fun receiver(secret: String) = "X-Cinema-Receiver-Secret" to secret
    private fun grant(secret: String) = "X-Cinema-Grant-Secret" to secret
    suspend fun register(name: String) = request("/receivers", fields = buildJsonObject { put("name", RemoteWire.safeLabel(name, 80, "Android TV")); put("platform", "android_tv") })
    suspend fun devices() = request("/receivers", "GET")
    suspend fun session(id: String, foreground: String, secret: String) = request("/sessions", fields = buildJsonObject { put("receiver_id", id); put("foreground_id", foreground) }, proof = receiver(secret))
    suspend fun presence(target: RemoteTarget, state: JsonObject, secret: String) = request("/presence", fields = body(target, buildJsonObject { put("state", state) }), proof = receiver(secret))
    suspend fun poll(target: RemoteTarget, delivery: Long, revision: Long, secret: String) = request("/poll", fields = body(target, buildJsonObject { put("after_delivery_id", delivery); put("after_response_revision", revision); put("wait_ms", 20000) }), proof = receiver(secret))
    suspend fun ack(target: RemoteTarget, outcomes: List<RemoteReceiverGuard.Ack>, secret: String) = request("/ack", fields = body(target, buildJsonObject { put("outcomes", JsonArray(outcomes.map { buildJsonObject { put("control_epoch", it.control_epoch); put("sequence", it.sequence); put("outcome", it.outcome) } })) }), proof = receiver(secret))
    suspend fun state(target: RemoteTarget, grantId: String, revision: Long, secret: String, waitMs: Int = 20000) = request("/state", fields = body(target, buildJsonObject { put("grant_id", grantId); put("after_revision", revision); put("wait_ms", waitMs.also { require(it in 0..20000) }) }), proof = grant(secret))
    suspend fun control(target: RemoteTarget, grantId: String, action: String, epoch: String?, secret: String) = request("/control", fields = body(target, buildJsonObject { put("grant_id", grantId); put("action", action); put("control_epoch", epoch?.let(::JsonPrimitive) ?: JsonNull) }), proof = grant(secret))
    suspend fun send(command: RemoteCommand, secret: String): JsonObject { command.validate(); return request("/commands", fields = command.json(), proof = grant(secret)) }
    suspend fun pairStart(target: RemoteTarget, secret: String) = request("/pairing/start", fields = body(target), proof = receiver(secret))
    suspend fun pairClaim(target: RemoteTarget, challenge: String?, code: String, name: String) = request("/pairing/claim", fields = body(target, buildJsonObject { put("challenge_id", challenge?.let(::JsonPrimitive) ?: JsonNull); put("code", code); put("controller_name", RemoteWire.safeLabel(name, 80, "Android phone")) }))
    suspend fun pairResult(target: RemoteTarget, pending: String, secret: String) = request("/pairing/result", fields = body(target, buildJsonObject { put("pending_id", pending) }), proof = "X-Cinema-Pairing-Secret" to secret)
    suspend fun pairApprove(target: RemoteTarget, pending: String, approve: Boolean, secret: String) = request("/pairing/approve", fields = body(target, buildJsonObject { put("pending_id", pending); put("approve", approve) }), proof = receiver(secret))
    suspend fun grants() = request("/grants", "GET")
    suspend fun revokeGrant(id: String) = request("/grants/" + RemoteWire.uuid(id), "DELETE")
    suspend fun unregister(id: String) = request("/receivers/" + RemoteWire.uuid(id), "DELETE")
}
