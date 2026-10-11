package tv.plurx.app.invitations

import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import tv.plurx.app.remote.*
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID

internal const val INVITATION_VERSION = "cinema.invitation.v1"
internal const val INVITATION_CATEGORY = "CINEMA_REMOTE_INVITATION"

@Serializable internal data class InvitationPhone(val installation_id: String, val name: String, val platform: String,
    val phone_generation: Long, val created_at: Long, val permission_granted: Boolean, val resident_active: Boolean)
@Serializable internal data class InvitationReadiness(val eligible: Boolean, val status: String, val provider_delivery_verified: Boolean)
@Serializable internal data class InvitationConsent(val receiver_id: String, val grant_id: String?, val enabled: Boolean, val transport: String?,
    val consent_generation: Long, val transport_generation: Long, val readiness: InvitationReadiness)
@Serializable internal data class InvitationTicket(val ticket_id: String, val ticket_secret: String, val expires_at: Long, val broker_origin: String, val broker_generation: String)
@Serializable internal data class InvitationPhoneReply(val version: String, val phone: InvitationPhone, val phone_secret: String?)
@Serializable internal data class InvitationPhonePage(val version: String, val phones: List<InvitationPhone>, val next_cursor: String?)
@Serializable internal data class InvitationConsentReply(val version: String, val consent: InvitationConsent)
@Serializable internal data class InvitationConsentPage(val version: String, val consents: List<InvitationConsent>, val next_cursor: String?)
@Serializable internal data class InvitationStartReply(val version: String, val consent: InvitationConsent, val ticket: InvitationTicket?)
@Serializable internal data class InvitationAvailabilityReply(val version: String, val phone: InvitationPhone)
@Serializable internal data class InvitationPollItem(val invitation_id: String, val expires_at: Long)
@Serializable internal data class InvitationPollReply(val version: String, val revision: Long, val invitations: List<InvitationPollItem>)
@Serializable internal data class InvitationLookupReply(val version: String, val receiver_id: String, val foreground_id: String, val target: RemoteTarget, val expires_at: Long)
@Serializable internal data class InvitationClaimReply(val version: String, val status: String)
@Serializable internal data class InvitationDeletedReply(val version: String)
@Serializable internal data class InvitationError(val version: String, val code: String, val message: String)

internal object InvitationWire {
    inline fun <reified T> decode(bytes: ByteArray): T {
        val body = RemoteWire.objectBody(bytes, 64 * 1024)
        require(body["version"]?.jsonPrimitive?.content == INVITATION_VERSION)
        return RemoteWire.json.decodeFromJsonElement<T>(body)
    }
    fun proof(value: String): String {
        require(Regex("[A-Za-z0-9_-]{43}").matches(value))
        val bytes = Base64.getUrlDecoder().decode(value)
        require(bytes.size == 32 && Base64.getUrlEncoder().withoutPadding().encodeToString(bytes) == value)
        return value
    }
    fun invitation(value: String): String { proof(value); return value }
    fun installation(value: String): String {
        val bytes = Base64.getUrlDecoder().decode(invitation(value))
        val buffer = ByteBuffer.wrap(bytes)
        return UUID(buffer.long, buffer.long).toString()
    }
    fun counter(value: Long, zero: Boolean = false) { require(value in (if (zero) 0 else 1)..REMOTE_MAX_INTEGER) }
    fun label(value: String, limit: Int = 80) { require(value.toByteArray(Charsets.UTF_8).size in 1..limit && value.none { Character.isISOControl(it) }) }
    fun phone(value: InvitationPhone): InvitationPhone = value.also {
        RemoteWire.uuid(it.installation_id); label(it.name); require(it.platform in setOf("apple", "android"))
        counter(it.phone_generation); counter(it.created_at, true)
        require(!it.resident_active || it.platform == "android" && it.permission_granted)
    }
    fun consent(value: InvitationConsent): InvitationConsent = value.also {
        RemoteWire.uuid(it.receiver_id); it.grant_id?.let(RemoteWire::uuid)
        counter(it.consent_generation, true); counter(it.transport_generation, true)
        require(it.transport == null || it.transport in setOf("apns", "fcm", "android_resident"))
        require(!it.enabled || it.grant_id != null && it.transport != null && it.consent_generation > 0)
        require(it.readiness.eligible == (it.enabled && it.readiness.status == "ready"))
        if (it.consent_generation == 0L) require(!it.enabled && it.grant_id == null && it.transport == null && it.transport_generation == 0L && it.readiness.status == "disabled" && !it.readiness.eligible)
        require(!it.readiness.provider_delivery_verified)
        require(it.readiness.status in setOf("disabled", "ready", "global_disabled", "login_changed", "grant_revoked", "permission_unavailable", "provider_unconfigured", "transport_pending", "transport_unavailable", "retention_limit", "migration_remediation"))
    }
    fun brokerOrigin(value: String): String {
        val uri = java.net.URI(value)
        require(uri.scheme == "https" && uri.userInfo == null && uri.rawPath.isNullOrEmpty() && uri.query == null && uri.fragment == null)
        val canonical = requireNotNull(tv.plurx.app.data.Session.canonicalOrigin(value))
        require(canonical == value)
        return canonical
    }
    fun ticket(value: InvitationTicket): InvitationTicket = value.also {
        RemoteWire.uuid(it.ticket_id); proof(it.ticket_secret); counter(it.expires_at)
        brokerOrigin(it.broker_origin); RemoteWire.uuid(it.broker_generation)
    }
    fun phoneReply(bytes: ByteArray) = decode<InvitationPhoneReply>(bytes).also { phone(it.phone); it.phone_secret?.let(::proof) }
    fun phonePage(bytes: ByteArray) = decode<InvitationPhonePage>(bytes).also {
        require(it.phones.size <= 20); it.phones.forEach(::phone)
        require(it.phones.map { row -> row.installation_id }.distinct().size == it.phones.size)
        it.next_cursor?.let(RemoteWire::uuid)
    }
    fun availability(bytes: ByteArray) = decode<InvitationAvailabilityReply>(bytes).also { phone(it.phone) }
    fun consentReply(bytes: ByteArray) = decode<InvitationConsentReply>(bytes).also { consent(it.consent) }
    fun consentPage(bytes: ByteArray) = decode<InvitationConsentPage>(bytes).also {
        require(it.consents.size <= 20); it.consents.forEach(::consent)
        require(it.consents.map { row -> row.receiver_id }.distinct().size == it.consents.size)
        it.next_cursor?.let(RemoteWire::uuid)
    }
    fun start(bytes: ByteArray) = decode<InvitationStartReply>(bytes).also { consent(it.consent); it.ticket?.let(::ticket) }
    fun poll(bytes: ByteArray) = decode<InvitationPollReply>(bytes).also {
        counter(it.revision, true); require(it.invitations.size <= 16)
        it.invitations.forEach { row -> invitation(row.invitation_id); counter(row.expires_at) }
        require(it.invitations.map { row -> row.invitation_id }.distinct().size == it.invitations.size)
    }
    fun lookup(bytes: ByteArray) = decode<InvitationLookupReply>(bytes).also {
        RemoteWire.uuid(it.receiver_id); RemoteWire.uuid(it.foreground_id); RemoteWire.validateTarget(it.target); counter(it.expires_at)
    }
    fun claim(bytes: ByteArray) = decode<InvitationClaimReply>(bytes).also { require(it.status == "claimed") }
    fun error(bytes: ByteArray) = decode<InvitationError>(bytes).also { require(Regex("[a-z_]{1,80}").matches(it.code)); label(it.message, 512) }
}
