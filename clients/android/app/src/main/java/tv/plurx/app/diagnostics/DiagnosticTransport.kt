package tv.plurx.app.diagnostics

import android.content.Context
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import okhttp3.*
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Session
import tv.plurx.app.remote.StrictRemoteJson
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.net.URI
import java.security.SecureRandom
import java.util.UUID
import java.util.concurrent.TimeUnit
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** This support client never inherits Net's Cinema bearer interceptor. */
internal class DiagnosticTransport(private val app: Context, private val namespace: String, val authorization: Session.PlaybackAuthorization = Session.playbackAuthorization()) {
    class Failure(val status: Int = 0, message: String = "Report connection failed. Retry or save this local report.") : Exception(message)
    companion object {
        private val client = OkHttpClient.Builder().callTimeout(10, TimeUnit.SECONDS).connectTimeout(5, TimeUnit.SECONDS).readTimeout(10, TimeUnit.SECONDS)
            .followRedirects(false).followSslRedirects(false).cookieJar(CookieJar.NO_COOKIES).cache(null)
            .dispatcher(Dispatcher().apply { maxRequests = 4; maxRequestsPerHost = 4 }).build()
        fun origin(text: String, development: Boolean = false): String {
            val uri = URI(text); require(uri.userInfo == null && uri.rawQuery == null && uri.rawFragment == null && (uri.path.isEmpty() || uri.path == "/"))
            require(uri.scheme == "https" || development && uri.scheme == "http" && uri.host in setOf("localhost","127.0.0.1","[::1]","::1"))
            return requireNotNull(Session.canonicalOrigin(text))
        }
        private val hex = Regex("[a-f0-9]{64}")
    }
    private val preferences = app.getSharedPreferences("cinema-diagnostics", Context.MODE_PRIVATE)
    private val secrets = DiagnosticSecrets(app, namespace)
    private val calls = mutableSetOf<Call>()
    @Volatile private var cancelled = false
    fun cancel() { cancelled = true; synchronized(calls) { calls.toList().forEach { it.cancel() }; calls.clear() } }
    fun current() { if (cancelled || Session.playbackAuthorization().generation != authorization.generation) throw Failure(message = "The account or report operation changed. Reopen this account's report.") }
    private fun supportOrigin(text: String) = origin(text, preferences.getBoolean("development_local",false))
    private suspend fun request(origin: String, path: String, method: String = "POST", body: ByteArray? = null, grant: String? = null, cinema: Boolean = false, chunkHash: String? = null, limit: Int = 262144, raw: Boolean = false, cleanup: Boolean = false, listener: EventListener? = null): ByteArray {
        val retiredCleanup = cleanup && cinema && method == "DELETE" && origin == authorization.origin && body == null && grant == null && Regex("/api/v1/diagnostics/snapshots/[a-f0-9-]{36}").matches(path)
        if (!retiredCleanup) current(); currentCoroutineContext().ensureActive()
        val base = if (cinema) requireNotNull(Session.canonicalOrigin(origin)) else supportOrigin(origin)
        require(path.startsWith('/') && !path.startsWith("//") && (body?.size ?: 0) <= 1048576)
        val builder = Request.Builder().url(base + path).header("Cache-Control","no-store")
        if (grant != null) { require(hex.matches(grant)); builder.header("Authorization","Bearer $grant") }
        else if (cinema && authorization.token != null) builder.header("Authorization","Bearer ${authorization.token}")
        if (chunkHash != null) builder.header("X-Chunk-Sha256",chunkHash)
        val contentType = (if (chunkHash == null) "application/json" else "application/octet-stream").toMediaType()
        val payload = body?.toRequestBody(contentType) ?: if (method in setOf("POST","PUT")) ByteArray(0).toRequestBody(contentType) else null
        val call = (if (listener == null) client else client.newBuilder().eventListener(listener).build()).newCall(builder.method(method,payload).build()); synchronized(calls) { calls += call }
        return try {
            suspendCancellableCoroutine { continuation ->
                continuation.invokeOnCancellation { call.cancel() }
                call.enqueue(object : Callback {
                    override fun onFailure(call: Call, e: IOException) { if (continuation.isActive) continuation.resumeWithException(e) }
                    override fun onResponse(call: Call, response: Response) {
                        response.use {
                            try {
                                if (!retiredCleanup) current(); val body = it.body; require(body == null || body.contentLength() < 0 || body.contentLength() <= limit)
                                val output = ByteArrayOutputStream()
                                body?.byteStream()?.use { input -> val buffer = ByteArray(16384); while (true) { val count = input.read(buffer); if (count < 0) break; require(output.size() + count <= limit); if (!retiredCleanup) current(); output.write(buffer,0,count) } }
                                if (!retiredCleanup) current(); if (!it.isSuccessful) throw Failure(it.code,"Support refused the request (HTTP ${it.code}). The local report remains available.")
                                val bytes = output.toByteArray(); if (!raw && bytes.isNotEmpty()) StrictRemoteJson.check(bytes.toString(Charsets.UTF_8))
                                if (continuation.isActive) continuation.resume(bytes)
                            } catch (error: Exception) { if (continuation.isActive) continuation.resumeWithException(error) }
                        }
                    }
                })
            }
        } finally { synchronized(calls) { calls -= call } }
    }
    private suspend fun json(origin: String, path: String, method: String = "POST", body: JsonObject? = null, grant: String? = null, cinema: Boolean = false, limit: Int = 262144): JsonObject {
        val bytes = request(origin,path,method,body?.toString()?.toByteArray(),grant,cinema,limit=limit)
        return if (bytes.isEmpty()) buildJsonObject {} else DiagnosticJson.parseToJsonElement(bytes.toString(Charsets.UTF_8)).jsonObject
    }
    private fun installation(): String {
        val current = preferences.getString("installation",null); if (current != null && runCatching { UUID.fromString(current).toString() == current }.getOrDefault(false)) return current
        return UUID.randomUUID().toString().also { check(preferences.edit().putString("installation",it).commit()) }
    }
    private fun enrollment(): JsonObject? = secrets.read("enrollment")?.takeIf {
        (it["expires"]?.jsonPrimitive?.longOrNull ?: 0) > System.currentTimeMillis() && hex.matches(it["secret"]?.jsonPrimitive?.content.orEmpty())
    }
    suspend fun provision() = withContext(Dispatchers.IO) {
        if (namespace == "anonymous" || authorization.token == null || enrollment() != null) return@withContext
        val value = json(authorization.origin,"/api/v1/diagnostics/enrollments",body=buildJsonObject { put("installation_id",installation()) },cinema=true)
        val origin = supportOrigin(value.getValue("origin").jsonPrimitive.content); val credential = value.getValue("authorization").jsonObject
        val secret = credential.getValue("enrollment").jsonPrimitive.content; val expires = credential.getValue("expires_in_ms").jsonPrimitive.long
        require(hex.matches(secret) && expires in 1..86400000); current()
        secrets.save("enrollment",buildJsonObject { put("origin",origin); put("secret",secret); put("expires",System.currentTimeMillis()+expires) })
    }
    suspend fun supportOrigin(): String = withContext(Dispatchers.IO) {
        enrollment()?.let { return@withContext supportOrigin(it.getValue("origin").jsonPrimitive.content) }
        val support = json(authorization.origin,"/api/v1/diagnostics/capabilities",method="GET",cinema=true).getValue("support").jsonObject
        require(support.getValue("configured").jsonPrimitive.boolean); supportOrigin(support.getValue("origin").jsonPrimitive.content)
    }
    suspend fun revokeEnrollment() = withContext(Dispatchers.IO) {
        runCatching { withTimeout(5000) { json(authorization.origin,"/api/v1/diagnostics/enrollments",method="DELETE",body=buildJsonObject { put("installation_id",installation()) },cinema=true) } }
        if (runCatching { current() }.isSuccess) secrets.delete("enrollment")
    }
    suspend fun invitation(origin: String, code: String, id: String) = withContext(Dispatchers.IO) {
        require(Regex("[A-Z2-9]{10}").matches(code)); val destination = supportOrigin(origin)
        val proof = secrets.read(id + ":invitation-proof")?.get("proof")?.jsonPrimitive?.content ?: ByteArray(32).also { SecureRandom().nextBytes(it) }.joinToString("") { "%02x".format(it) }.also { secrets.save(id + ":invitation-proof",buildJsonObject { put("proof",it) }) }
        val value = json(destination,"/support/v1/invitations/redeem",body=buildJsonObject { put("code",code); put("proof",proof) }); val secret = value.getValue("invitation").jsonPrimitive.content
        require(hex.matches(secret)); current(); secrets.save(id + ":invitation",buildJsonObject { put("origin",destination); put("secret",secret) })
    }
    suspend fun snapshot(capture: DiagnosticCapture, includeSystem: Boolean = false): Pair<List<DiagnosticEvent>,List<DiagnosticComponent>> {
        if (authorization.token == null || capture.namespace == "anonymous") throw Failure()
        var id: String? = null
        try {
            return withTimeout(6000) {
                val created = json(authorization.origin,"/api/v1/diagnostics/snapshots",body=buildJsonObject {
                    put("incident_at_ms",capture.incidentMs); put("window_ms",600000); put("include_system",includeSystem)
                    capture.events.lastOrNull { "correlation" in it.facts }?.facts?.get("correlation")?.let { put("correlation",it) }
                },cinema=true)
                id = created.getValue("id").jsonPrimitive.content; require(UUID.fromString(id).toString() == id)
                while (true) {
                    val value = json(authorization.origin,"/api/v1/diagnostics/snapshots/$id",method="GET",cinema=true,limit=2097152)
                    require(value.getValue("version").jsonPrimitive.content == DiagnosticWire.VERSION)
                    if (value.getValue("state").jsonPrimitive.content != "pending") {
                        val events = DiagnosticJson.decodeFromJsonElement<List<DiagnosticEvent>>(value.getValue("events")); val components = DiagnosticJson.decodeFromJsonElement<List<DiagnosticComponent>>(value.getValue("components"))
                        require(events.size <= 10000 && events.all { it.valid() } && components.all { it.valid() })
                        return@withTimeout events to components
                    }
                    delay(250)
                }
                @Suppress("UNREACHABLE_CODE") error("snapshot unavailable")
            }
        } finally {
            val cleanupID = id
            if (cleanupID != null) withContext(NonCancellable) {
                val cleanup = DiagnosticTransport(app,namespace,authorization)
                runCatching { withTimeout(1000) { cleanup.request(authorization.origin,"/api/v1/diagnostics/snapshots/$cleanupID",method="DELETE",cinema=true,cleanup=true) } }; cleanup.cancel()
            }
        }
    }
    suspend fun connectionChecks(): List<DiagnosticEvent> = withContext(Dispatchers.IO) {
        current(); val began=android.os.SystemClock.elapsedRealtime(); val run=UUID.randomUUID().toString()
        val events=mutableListOf<DiagnosticEvent>(); var bytes=0
        fun event(facts: JsonObject) { events += DiagnosticEvent(run,(events.size+1).toString(),DiagnosticFields.utc(),(android.os.SystemClock.elapsedRealtime()-began).coerceIn(0,15000),"network","request",facts) }
        val phases=java.util.concurrent.ConcurrentHashMap<String,Long>(); val starts=java.util.concurrent.ConcurrentHashMap<String,Long>()
        val listener=object: EventListener() {
            private fun begin(phase: String) { starts[phase]=android.os.SystemClock.elapsedRealtime() }
            private fun end(phase: String) { starts.remove(phase)?.let { phases[phase]=(android.os.SystemClock.elapsedRealtime()-it).coerceIn(0,15000) } }
            override fun dnsStart(call: Call, domainName: String) { begin("dns") }
            override fun dnsEnd(call: Call, domainName: String, inetAddressList: List<java.net.InetAddress>) { end("dns") }
            override fun connectStart(call: Call, inetSocketAddress: java.net.InetSocketAddress, proxy: java.net.Proxy) { begin("connect") }
            override fun connectEnd(call: Call, inetSocketAddress: java.net.InetSocketAddress, proxy: java.net.Proxy, protocol: Protocol?) { end("connect") }
            override fun secureConnectStart(call: Call) { begin("tls") }
            override fun secureConnectEnd(call: Call, handshake: Handshake?) { end("tls") }
        }
        try {
            withTimeout(15000) {
                val paths=listOf("/healthz" to "connect","/readyz" to "readiness","/api/v1/server" to "readiness") + if (authorization.token != null) listOf("/api/v1/me" to "auth") else emptyList()
                for ((path,phase) in paths) {
                    current(); if (bytes >= 2097152) break
                    val start=android.os.SystemClock.elapsedRealtime(); var status=0; var failure: String?=null
                    try { bytes += request(authorization.origin,path,method="GET",cinema=true,limit=2097152-bytes,raw=true,listener=listener).size }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (error: Exception) { current(); status=(error as? Failure)?.status ?: 0; failure=when(error) { is java.net.UnknownHostException -> "dns"; is javax.net.ssl.SSLException -> "tls"; is java.net.ConnectException -> "connect"; is Failure -> if (status == 401 || status == 403) "authentication" else "http"; else -> "network" } }
                    event(buildJsonObject { put("phase",phase); put("method","GET"); put("observed",failure == null); put("duration_ms",(android.os.SystemClock.elapsedRealtime()-start).coerceIn(0,15000)); if (status != 0) put("status",status); failure?.let { put("failure",it) } })
                }
            }
        } catch (timeout: TimeoutCancellationException) { currentCoroutineContext().ensureActive(); event(buildJsonObject { put("phase","unavailable"); put("observed",false); put("reason","timeout") }) }
        current()
        if (bytes < 2097152 && android.os.SystemClock.elapsedRealtime()-began < 15000) {
            val wall=System.currentTimeMillis(); val before=android.os.SystemClock.elapsedRealtime()
            val reply=runCatching { withTimeout((15000-(before-began)).coerceAtLeast(1)) { json(authorization.origin,"/api/v1/diagnostics/capabilities",method="GET",cinema=true,limit=2097152-bytes) } }.getOrNull()
            val duration=android.os.SystemClock.elapsedRealtime()-before; val server=reply?.get("server_time_ms")?.jsonPrimitive?.longOrNull
            if (server != null) { val offset=server-wall-duration/2; if (offset in -604800000..604800000 && duration <= 15000) event(buildJsonObject { put("observed",true); put("clock_ahead_ms",offset.coerceAtLeast(0)); put("clock_behind_ms",(-offset).coerceAtLeast(0)); put("clock_uncertainty_ms",duration+1) }) }
        }
        current()
        for (phase in listOf("dns","connect","tls")) event(buildJsonObject { put("phase",phase); put("observed",phases.containsKey(phase)); phases[phase]?.let { put("duration_ms",it) } ?: put("reason","context_unavailable") })
        event(buildJsonObject { put("phase","delivery"); put("observed",false); put("reason","not_requested") }); events
    }
    suspend fun stage(archive: DiagnosticArchive, original: DiagnosticDraft, remember: suspend (DiagnosticDraft) -> Unit): DiagnosticDraft = withContext(Dispatchers.IO) {
        require(original.consented && original.capture.namespace == namespace && original.id == archive.binding.report_id)
        var draft = original; val origin = supportOrigin(requireNotNull(draft.origin)); val binding = DiagnosticJson.encodeToJsonElement(archive.binding).jsonObject
        var authority = secrets.read(draft.id) ?: buildJsonObject {}
        if ("upload" !in authority) {
            val invitation = secrets.read(draft.id + ":invitation"); val enrollment = enrollment()
            val grant = when {
                invitation != null -> { require(invitation.getValue("origin").jsonPrimitive.content == origin); json(origin,"/support/v1/grants",body=binding,grant=invitation.getValue("secret").jsonPrimitive.content) }
                enrollment != null && enrollment.getValue("origin").jsonPrimitive.content == origin -> json(origin,"/support/v1/grants",body=binding,grant=enrollment.getValue("secret").jsonPrimitive.content)
                else -> { val reply = json(authorization.origin,"/api/v1/diagnostics/upload-grants",body=binding,cinema=true); require(supportOrigin(reply.getValue("origin").jsonPrimitive.content) == origin); reply.getValue("grant").jsonObject }
            }
            val upload = grant.getValue("upload_grant").jsonPrimitive.content; require(hex.matches(upload)); current()
            authority = JsonObject(authority + ("upload" to JsonPrimitive(upload))); secrets.save(draft.id,authority)
        }
        if ("management" !in authority) {
            val value = json(origin,"/support/v1/uploads",body=binding,grant=authority.getValue("upload").jsonPrimitive.content)
            require(value.getValue("upload_id").jsonPrimitive.content == draft.id && value.getValue("chunk_bytes").jsonPrimitive.int == 1048576)
            val management = value.getValue("management_grant").jsonPrimitive.content; require(hex.matches(management)); current()
            authority = JsonObject(authority + ("management" to JsonPrimitive(management))); secrets.save(draft.id,authority)
        }
        val management = authority.getValue("management").jsonPrimitive.content
        val status = json(origin,"/support/v1/uploads/${draft.id}/status",method="GET",grant=management)
        if (status.getValue("state").jsonPrimitive.content in setOf("staged","submitted")) {
            draft = draft.copy(state=status.getValue("state").jsonPrimitive.content,reference=status["reference"]?.jsonPrimitive?.content); remember(draft); return@withContext draft
        }
        val refresh = json(origin,"/support/v1/uploads/${draft.id}/refresh",grant=management); val upload = refresh.getValue("upload_grant").jsonPrimitive.content; require(hex.matches(upload))
        authority = JsonObject(authority + ("upload" to JsonPrimitive(upload))); current(); secrets.save(draft.id,authority)
        val count = (archive.bytes.size + 1048575) / 1048576; val received = status.getValue("received").jsonArray.map { it.jsonPrimitive.int }.toSet(); require(received.all { it in 0 until count })
        for (index in 0 until count) if (index !in received) {
            current(); val bytes = archive.bytes.copyOfRange(index*1048576,minOf(archive.bytes.size,(index+1)*1048576))
            request(origin,"/support/v1/uploads/${draft.id}/chunks/$index",method="PUT",body=bytes,grant=upload,chunkHash=DiagnosticFields.hash(bytes)); current()
            draft = draft.copy(state="uploading"); remember(draft)
        }
        val complete = json(origin,"/support/v1/uploads/${draft.id}/complete",grant=upload); val state = complete.getValue("state").jsonPrimitive.content; require(state in setOf("staged","submitted"))
        draft = draft.copy(state=state); remember(draft); draft
    }
    private fun management(draft: DiagnosticDraft) = requireNotNull(secrets.read(draft.id)?.get("management")?.jsonPrimitive?.content)
    suspend fun submit(draft: DiagnosticDraft): DiagnosticDraft = withContext(Dispatchers.IO) {
        require(draft.consented); val value = json(requireNotNull(draft.origin),"/support/v1/reports/${draft.id}/submit",body=buildJsonObject { put("description",draft.description) },grant=management(draft))
        require(value.getValue("state").jsonPrimitive.content == "submitted"); val reference = value.getValue("reference").jsonPrimitive.content; require(Regex("[A-Za-z0-9-]{1,64}").matches(reference)); current(); draft.copy(state="submitted",reference=reference)
    }
    suspend fun offer(draft: DiagnosticDraft): DiagnosticDraft = withContext(Dispatchers.IO) {
        require(draft.consented); val value = json(requireNotNull(draft.origin),"/support/v1/handoffs",body=buildJsonObject { put("report_id",draft.id) },grant=management(draft))
        // Service replies contain extra role metadata; normalize only this UI projection.
        val offer = DiagnosticOffer(value.getValue("id").jsonPrimitive.content,value.getValue("manual_code").jsonPrimitive.content,value.getValue("url").jsonPrimitive.content,value.getValue("expires_in_ms").jsonPrimitive.long)
        require(UUID.fromString(offer.id).toString() == offer.id && Regex("[A-Z2-9]{10}").matches(offer.manual_code) && offer.url.startsWith(supportOrigin(draft.origin) + "/#handoff=") && offer.url.length <= 1024 && offer.expires_in_ms in 1..600000)
        current(); draft.copy(handoff=offer)
    }
    suspend fun handoffStatus(draft: DiagnosticDraft) = withContext(Dispatchers.IO) { json(requireNotNull(draft.origin),"/support/v1/handoffs/${requireNotNull(draft.handoff).id}/status",method="GET",grant=management(draft)) }
    suspend fun approve(draft: DiagnosticDraft, comparison: String) = withContext(Dispatchers.IO) {
        require(Regex("[A-Z2-9]{6}").matches(comparison)); json(requireNotNull(draft.origin),"/support/v1/handoffs/${requireNotNull(draft.handoff).id}/approve",body=buildJsonObject { put("comparison",comparison) },grant=management(draft))
    }
    suspend fun delete(draft: DiagnosticDraft, keep: Boolean = false) = withContext(Dispatchers.IO) {
        if (secrets.read(draft.id)?.get("management") == null) return@withContext
        if (draft.handoff != null) json(requireNotNull(draft.origin),"/support/v1/handoffs/${draft.handoff.id}",method="DELETE",body=buildJsonObject { put("keep_draft",keep) },grant=management(draft))
        else if (!keep) json(requireNotNull(draft.origin),"/support/v1/reports/${draft.id}",method="DELETE",grant=management(draft))
        Unit
    }
}
