package tv.plurx.app.invitations

import org.junit.Assert.*
import org.junit.Test
import java.nio.ByteBuffer
import java.util.Base64
import java.util.UUID

class InvitationAdmissionTest {
    private val installation = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private fun id(number: Long, owner: String = installation): String {
        val uuid = UUID.fromString(owner)
        return Base64.getUrlEncoder().withoutPadding().encodeToString(ByteBuffer.allocate(32)
            .putLong(uuid.mostSignificantBits).putLong(uuid.leastSignificantBits).putLong(0).putLong(number).array())
    }
    @Test fun offlineOffLogoutPermissionAndInstallationGateBeforeSharedDedupe() {
        var now = 100L
        val gate = InvitationAdmission { now }
        val state = InvitationAdmission.State(installation, true, true, true, true, true)
        assertFalse(gate.admit(state.copy(fcmEnabled = false), id(1), "fcm"))
        assertFalse(gate.admit(state.copy(loginCurrent = false), id(1), "fcm"))
        assertFalse(gate.admit(state.copy(permission = false), id(1), "fcm"))
        assertFalse(gate.admit(state.copy(channelEnabled = false), id(1), "fcm"))
        assertFalse(gate.admit(state, id(1, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"), "fcm"))
        assertTrue(gate.admit(state, id(1), "fcm"))
        assertFalse(gate.admit(state, id(1), "android_resident", 200))
        assertFalse(gate.admit(state.copy(residentActive = false), id(2), "android_resident", 200))
        assertFalse(gate.admit(state, id(2), "android_resident", 100))
        assertTrue(gate.admit(state, id(2), "android_resident", 200))
        val restored = InvitationAdmission { now }; restored.restore(gate.snapshot())
        assertFalse(restored.admit(state, id(2), "fcm"))
        now = 201; assertTrue(restored.admit(state, id(2), "fcm"))
    }
    @Test fun invalidClockAndRollbackCannotReleaseDedupeOrAdmitFreshWork() {
        var now = 100L
        val gate = InvitationAdmission { now }
        val state = InvitationAdmission.State(installation, true, true, true, true, false)
        assertTrue(gate.admit(state, id(1), "fcm"))
        now = 99; assertFalse(gate.validClock()); assertFalse(gate.admit(state, id(2), "fcm"))
        now = 0; assertFalse(gate.admit(state, id(2), "fcm"))
        now = Long.MAX_VALUE; assertFalse(gate.admit(state, id(2), "fcm"))
        now = 100; assertTrue(gate.validClock()); assertFalse(gate.admit(state, id(1), "fcm")); assertTrue(gate.admit(state, id(2), "fcm"))
    }
    @Test fun saturationNeverEvictsAnUnexpiredNotificationIdentity() {
        val gate = InvitationAdmission { 100L }
        val state = InvitationAdmission.State(installation, true, true, true, true, false)
        repeat(256) { assertTrue(gate.admit(state, id(it.toLong()), "fcm")) }
        assertFalse(gate.admit(state, id(257), "fcm"))
        assertFalse(gate.admit(state, id(0), "fcm"))
        assertEquals(256, gate.snapshot().size)
    }
}
