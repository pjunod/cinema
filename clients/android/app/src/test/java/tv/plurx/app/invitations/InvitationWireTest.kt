package tv.plurx.app.invitations

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.remote.RemoteProfileScope
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID

class InvitationWireTest {
    private val id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private fun phone() = buildJsonObject {
        put("installation_id", id); put("name", "Android phone"); put("platform", "android")
        put("phone_generation", 1); put("created_at", 1780000000); put("permission_granted", false); put("resident_active", false)
    }
    private fun reply() = buildJsonObject { put("version", INVITATION_VERSION); put("phone", phone()); put("phone_secret", JsonNull) }.toString()
    @Test fun strictWholeBodyRejectsNumericLexemesUnknownNestedFieldsAndBadGeneration() {
        assertEquals(id, InvitationWire.phoneReply(reply().toByteArray()).phone.installation_id)
        listOf("1.0", "1e0", "-0", "9007199254740992").forEach { number ->
            assertTrue(runCatching { InvitationWire.phoneReply(reply().replace("\"phone_generation\":1", "\"phone_generation\":" + number).toByteArray()) }.isFailure)
        }
        val unknown = reply().replace("\"name\":", "\"provider_token\":\"forbidden\",\"name\":")
        assertTrue(runCatching { InvitationWire.phoneReply(unknown.toByteArray()) }.isFailure)
        assertTrue(runCatching { InvitationWire.phoneReply(reply().replace("\"version\":\"cinema.invitation.v1\"", "\"version\":\"unknown\"").toByteArray()) }.isFailure)
        assertTrue(runCatching { InvitationWire.phoneReply(reply().replace("\"resident_active\":false", "\"resident_active\":true").toByteArray()) }.isFailure)
    }
    @Test fun consentReadinessAndAbsentOffAreConsistent() {
        val empty = InvitationConsent(id, null, false, null, 0, 0, InvitationReadiness(false, "disabled", false))
        assertEquals(empty, InvitationWire.consent(empty))
        listOf(empty.copy(readiness = empty.readiness.copy(eligible = true)),
            empty.copy(transport_generation = 1), empty.copy(transport = "fcm"),
            empty.copy(grant_id = id), empty.copy(readiness = empty.readiness.copy(status = "ready")),
            empty.copy(enabled = true, grant_id = id, transport = "fcm")).forEach {
            assertTrue(runCatching { InvitationWire.consent(it) }.isFailure)
        }
        val ready = empty.copy(enabled = true, grant_id = id, transport = "fcm", consent_generation = 1,
            transport_generation = 1, readiness = InvitationReadiness(true, "ready", false))
        assertEquals(ready, InvitationWire.consent(ready))
        assertTrue(runCatching { InvitationWire.consent(ready.copy(readiness = ready.readiness.copy(eligible = false))) }.isFailure)
        assertEquals(0L, InvitationWire.phoneReply(reply().replace("1780000000", "0").toByteArray()).phone.created_at)
    }
    @Test fun canonicalOpaqueIdSelectsOnlyInstallationPrefixAndRejectsPaddingAliases() {
        val uuid = UUID.fromString(id)
        val bytes = ByteBuffer.allocate(32).putLong(uuid.mostSignificantBits).putLong(uuid.leastSignificantBits).putLong(7).putLong(9).array()
        val encoded = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)
        assertEquals(43, encoded.length)
        assertEquals(id, InvitationWire.installation(encoded))
        assertTrue(runCatching { InvitationWire.invitation(encoded + "=") }.isFailure)
        val last = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
        val alias = encoded.dropLast(1) + last[last.indexOf(encoded.last()) + 1]
        assertTrue(runCatching { InvitationWire.invitation(alias) }.isFailure)
    }
    @Test fun canonicalScopeSeparatesOriginInstanceAccountAndOldConcatenationCollision() {
        val first = RemoteProfileScope.create("HTTPS://HOME.TEST:443/", "instance", 7)
        assertEquals(first, RemoteProfileScope.create("https://home.test", "instance", 7))
        assertNotEquals(first.digest, RemoteProfileScope.create("https://other.test", "instance", 7).digest)
        assertNotEquals(first.digest, RemoteProfileScope.create("https://home.test", "instance", 8).digest)
        assertNotEquals(RemoteProfileScope.create("https://home.test:123", "instance", 7).digest,
            RemoteProfileScope.create("https://home.test", "123:instance", 7).digest)
        assertTrue(runCatching { RemoteProfileScope.create("https://user@home.test", "instance", 7) }.isFailure)
    }
    @Test fun brokerTicketAcceptsOnlyExactCanonicalHttpsOrigin() {
        assertEquals("https://broker.test:8443", InvitationWire.brokerOrigin("https://broker.test:8443"))
        listOf("http://broker.test", "https://broker.test/", "https://user@broker.test", "https://broker.test/path", "https://broker.test?key=secret", "https://broker.test#x", "https://BROKER.test").forEach {
            assertTrue(it, runCatching { InvitationWire.brokerOrigin(it) }.isFailure)
        }
    }
}
