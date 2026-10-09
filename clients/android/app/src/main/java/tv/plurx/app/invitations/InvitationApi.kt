package tv.plurx.app.invitations

import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.serialization.json.*
import okhttp3.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import okhttp3.MediaType.Companion.toMediaType
import okhttp3.RequestBody.Companion.toRequestBody
import tv.plurx.app.data.Session
import tv.plurx.app.remote.RemoteWire
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.concurrent.TimeUnit

internal class InvitationHttpFailure(val status: Int, val code: String) : IOException("Cinema invitation request rejected")

/** One bounded request; cancellation owns the call, and replies cannot cross an authority change. */
internal class InvitationTransport(private val client: OkHttpClient, private val current: () -> Boolean) {
    suspend fun exchange(request: Request, brokerGeneration: String? = null): ByteArray {
        check(current())
        return suspendCancellableCoroutine { continuation ->
            val call = client.newCall(request)
            continuation.invokeOnCancellation { call.cancel() }
            call.enqueue(object : Callback {
                override fun onFailure(call: Call, e: IOException) { if (continuation.isActive) continuation.resumeWith(Result.failure(e)) }
                override fun onResponse(call: Call, response: Response) {
                    val result = runCatching {
                        response.use {
                            if (brokerGeneration != null) {
                                val values = it.headers.values("X-Cinema-Broker-Generation")
                                require(values.size == 1 && RemoteWire.uuid(values.single()) == brokerGeneration)
                            }
                            val body = requireNotNull(it.body)
                            require(body.contentLength() <= 65536)
                            val out = ByteArrayOutputStream()
                            body.byteStream().use { stream ->
                                val chunk = ByteArray(4096)
                                while (true) {
                                    val count = stream.read(chunk)
                                    if (count < 0) break
                                    require(out.size() + count <= 65536)
                                    out.write(chunk, 0, count)
                                }
                            }
                            check(current())
                            val bytes = out.toByteArray()
                            if (!it.isSuccessful) throw InvitationHttpFailure(it.code, InvitationWire.error(bytes).code)
                            bytes
                        }
                    }
                    if (continuation.isActive) continuation.resumeWith(result)
                }
            })
        }
    }
    companion object {
        fun client() = OkHttpClient.Builder().followRedirects(false).followSslRedirects(false).retryOnConnectionFailure(false).cache(null)
            .connectTimeout(10, TimeUnit.SECONDS).readTimeout(25, TimeUnit.SECONDS).callTimeout(30, TimeUnit.SECONDS).build()
    }
}

internal class InvitationApi(
    private val authorization: Session.PlaybackAuthorization,
    client: OkHttpClient = InvitationTransport.client(),
    private val isCurrent: () -> Boolean = { Session.playbackAuthorization() == authorization },
) {
    private val origin = requireNotNull(Session.canonicalOrigin(authorization.origin)).toHttpUrl()
    private val transport = InvitationTransport(client, isCurrent)
    private suspend fun request(path: String, fields: JsonObject? = null, phone: String? = null, grant: String? = null): ByteArray {
        check(isCurrent() && authorization.token != null)
        val url = origin.newBuilder().encodedPath("/api/remote/v1" + path).query(null).fragment(null).build()
        require(url.username.isEmpty() && url.password.isEmpty())
        val builder = Request.Builder().url(url).header("Authorization", "Bearer " + authorization.token).header("Cache-Control", "no-store")
        phone?.let { builder.header("X-Cinema-Phone-Secret", InvitationWire.proof(it)) }
        grant?.let { builder.header("X-Cinema-Grant-Secret", InvitationWire.proof(it)) }
        if (fields == null) builder.delete() else {
            val bytes = JsonObject(fields + ("version" to JsonPrimitive(INVITATION_VERSION))).toString().toByteArray(Charsets.UTF_8)
            require(bytes.size <= 65536)
            builder.post(bytes.toRequestBody("application/json".toMediaType()))
        }
        return transport.exchange(builder.build())
    }
    suspend fun register(id: String, name: String, secret: String? = null) = InvitationWire.phoneReply(request("/phones", buildJsonObject {
        put("installation_id", RemoteWire.uuid(id)); put("platform", "android"); InvitationWire.label(name); put("name", name)
    }, secret)).also { require(it.phone.installation_id == id && it.phone.platform == "android"); require(secret != null || it.phone_secret != null) }
    suspend fun phones(after: String? = null) = InvitationWire.phonePage(request("/phones/list", buildJsonObject {
        put("after_id", after?.let { JsonPrimitive(RemoteWire.uuid(it)) } ?: JsonNull); put("limit", 20)
    }))
    suspend fun remove(id: String) = InvitationWire.decode<InvitationDeletedReply>(request("/phones/" + RemoteWire.uuid(id)))
    suspend fun availability(id: String, generation: Long, permission: Boolean, resident: Boolean, secret: String) = InvitationWire.availability(request("/phones/" + RemoteWire.uuid(id) + "/availability", buildJsonObject {
        InvitationWire.counter(generation); require(!resident || permission)
        put("expected_phone_generation", generation); put("permission_granted", permission); put("resident_active", resident)
    }, secret)).also { require(it.phone.installation_id == id && it.phone.platform == "android" && it.phone.phone_generation > generation); require(it.phone.permission_granted == permission && it.phone.resident_active == resident) }
    suspend fun rebind(id: String, generation: Long, secret: String) = InvitationWire.availability(request("/phones/" + RemoteWire.uuid(id) + "/rebind", buildJsonObject {
        InvitationWire.counter(generation); put("expected_phone_generation", generation)
    }, secret)).also { require(it.phone.installation_id == id && it.phone.platform == "android" && it.phone.phone_generation > generation && !it.phone.permission_granted && !it.phone.resident_active) }
    suspend fun consents(id: String, secret: String, after: String? = null) = InvitationWire.consentPage(request("/invitations/consents/list", buildJsonObject {
        put("installation_id", RemoteWire.uuid(id)); put("after_receiver_id", after?.let { JsonPrimitive(RemoteWire.uuid(it)) } ?: JsonNull); put("limit", 20)
    }, secret))
    private fun generations(phone: Long, consent: Long) { InvitationWire.counter(phone); InvitationWire.counter(consent, true) }
    suspend fun consent(id: String, receiver: String, phoneGeneration: Long, consentGeneration: Long,
        enabled: Boolean, grantId: String?, mode: String?, secret: String, grant: String?): InvitationConsentReply {
        generations(phoneGeneration, consentGeneration)
        require(mode == null || mode in setOf("fcm", "android_resident"))
        require(!enabled || grantId != null && grant != null && mode != null)
        return InvitationWire.consentReply(request("/invitations/consent", buildJsonObject {
            put("installation_id", RemoteWire.uuid(id)); put("receiver_id", RemoteWire.uuid(receiver))
            put("expected_phone_generation", phoneGeneration); put("expected_consent_generation", consentGeneration)
            put("enabled", enabled); put("grant_id", grantId?.let { JsonPrimitive(RemoteWire.uuid(it)) } ?: JsonNull)
            put("transport", mode?.let(::JsonPrimitive) ?: JsonNull)
        }, secret, grant)).also {
            require(it.consent.receiver_id == receiver && it.consent.enabled == enabled)
            require(it.consent.consent_generation > consentGeneration || !enabled && consentGeneration == 0L && it.consent.consent_generation == 0L)
            if (enabled) require(it.consent.grant_id == grantId && it.consent.transport == mode)
        }
    }
    suspend fun start(id: String, receiver: String, grantId: String, phoneGeneration: Long, consentGeneration: Long,
        secret: String, grant: String): InvitationStartReply {
        generations(phoneGeneration, consentGeneration); require(consentGeneration > 0)
        return InvitationWire.start(request("/invitations/transport/start", buildJsonObject {
            put("installation_id", RemoteWire.uuid(id)); put("receiver_id", RemoteWire.uuid(receiver)); put("grant_id", RemoteWire.uuid(grantId))
            put("expected_phone_generation", phoneGeneration); put("expected_consent_generation", consentGeneration)
        }, secret, grant)).also {
            require(it.consent.receiver_id == receiver && it.consent.enabled && it.consent.grant_id == grantId && it.consent.transport == "fcm")
            require(it.consent.consent_generation > consentGeneration && it.consent.transport_generation > 0)
            if (it.ticket != null) require(it.consent.readiness.status == "transport_pending" && !it.consent.readiness.eligible)
        }
    }
    suspend fun confirm(id: String, receiver: String, grantId: String, ticket: String, phoneGeneration: Long,
        consentGeneration: Long, transportGeneration: Long, secret: String, grant: String): InvitationConsentReply {
        generations(phoneGeneration, consentGeneration); InvitationWire.counter(transportGeneration)
        return InvitationWire.consentReply(request("/invitations/transport/confirm", buildJsonObject {
            put("installation_id", RemoteWire.uuid(id)); put("receiver_id", RemoteWire.uuid(receiver)); put("ticket_id", RemoteWire.uuid(ticket))
            put("expected_phone_generation", phoneGeneration); put("expected_consent_generation", consentGeneration)
            put("expected_transport_generation", transportGeneration)
        }, secret, grant)).also {
            require(it.consent.receiver_id == receiver && it.consent.enabled && it.consent.grant_id == grantId && it.consent.transport == "fcm")
            require(it.consent.consent_generation == consentGeneration && it.consent.transport_generation == transportGeneration)
        }
    }
    suspend fun poll(id: String, revision: Long, secret: String) = InvitationWire.poll(request("/invitations/poll", buildJsonObject {
        put("installation_id", RemoteWire.uuid(id)); InvitationWire.counter(revision, true); put("after_revision", revision); put("wait_ms", 20000)
    }, secret))
    suspend fun lookup(id: String, invitation: String, secret: String) = InvitationWire.lookup(request("/invitations/lookup", buildJsonObject {
        put("installation_id", RemoteWire.uuid(id)); put("invitation_id", InvitationWire.invitation(invitation))
    }, secret))
}

/** The broker receives only its ticket proof and OS token, never a home or pairing credential. */
internal class InvitationBrokerApi(private val client: OkHttpClient = InvitationTransport.client().newBuilder().proxy(java.net.Proxy.NO_PROXY).build()) {
    suspend fun claim(ticket: InvitationTicket, token: String, current: () -> Boolean, now: () -> Long = { System.currentTimeMillis() / 1000 }): InvitationClaimReply {
        InvitationWire.ticket(ticket)
        val started = now(); InvitationWire.counter(started)
        val remaining = ticket.expires_at - started
        require(remaining in 1..120)
        val monotonicStart = System.nanoTime()
        require(token.toByteArray(Charsets.UTF_8).size in 1..4096 && token.none { Character.isISOControl(it) || it.isWhitespace() })
        val url = InvitationWire.brokerOrigin(ticket.broker_origin).toHttpUrl().newBuilder().encodedPath("/broker/v1/tickets/claim").build()
        val body = buildJsonObject {
            put("version", INVITATION_VERSION); put("ticket_id", ticket.ticket_id); put("platform", "android"); put("device_token", token)
        }.toString().toRequestBody("application/json".toMediaType())
        val request = Request.Builder().url(url).header("Authorization", "Bearer " + ticket.ticket_secret).header("Cache-Control", "no-store").post(body).build()
        return InvitationWire.claim(InvitationTransport(client, { current() && now() in started until ticket.expires_at &&
            (System.nanoTime() - monotonicStart).let { it >= 0 && it < remaining * 1_000_000_000L } }).exchange(request, ticket.broker_generation))
    }
}
