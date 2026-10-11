package tv.plurx.app.invitations

import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.remote.RemoteSecretStorage
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID

class InvitationTapAdmissionTest {
    private val id = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private val receiver = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    private val fingerprint = "a".repeat(64)
    private fun state(): InvitationLocalState = InvitationLocalState("exact-origin-instance-user", id, "A".repeat(43),
        InvitationPhone(id, "Phone", "android", 1, 0, true, false),
        listOf(InvitationChoice(receiver, true, "fcm", 1, false)), emptyList(), false, false, 0, emptyMap(), fingerprint,
        true, false, false, false, 0, null)
    private fun invitation(): String {
        val uuid = UUID.fromString(id)
        return Base64.getUrlEncoder().withoutPadding().encodeToString(ByteBuffer.allocate(32)
            .putLong(uuid.mostSignificantBits).putLong(uuid.leastSignificantBits).putLong(0).putLong(1).array())
    }
    @Test fun lookupCompletionCannotSelectAfterOfflineOffOrIdentityPhoneIntentReplacement() {
        val state = state(); val permit = InvitationTapPermit.capture(state, invitation(), fingerprint, true, false)
        assertTrue(permit.permits(state, receiver, fingerprint, true, false))
        listOf(state.copy(pendingDeletion = true), state.copy(lostProof = true), state.copy(scope = "another-origin"),
            state.copy(choices = state.choices.map { it.copy(enabled = false, pendingSync = true) }),
            state.copy(choices = state.choices.map { it.copy(intentRevision = 2) }),
            state.copy(phone = state.phone!!.copy(phone_generation = 2))).forEach {
            assertFalse(permit.permits(it, receiver, fingerprint, true, false))
        }
        assertFalse(permit.permits(state, receiver, fingerprint, false, false))
        assertFalse(permit.permits(state, receiver, fingerprint, true, true))
        assertFalse(permit.permits(state, receiver, "b".repeat(64), true, false))
        assertTrue(runCatching { InvitationTapPermit.capture(state, invitation(), fingerprint, false, false) }.isFailure)
    }
    @Test fun missingGrantAndUnavailableReadinessKeepSavedOffRowsInActualInventory() {
        val state = state()
        val other = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
        val consent = InvitationConsent(other, null, false, null, 0, 0, InvitationReadiness(false, "disabled", false))
        assertEquals(listOf(receiver, other), InvitationScreenInventory.ids(state.choices, listOf(consent), emptyList()))
        val grant = RemoteSecretStorage.Grant(other, id, "A".repeat(43))
        assertEquals(listOf(receiver, other), InvitationScreenInventory.ids(state.choices, listOf(consent), listOf(grant)))
    }
}
