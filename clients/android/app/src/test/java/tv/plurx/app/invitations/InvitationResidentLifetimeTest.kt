package tv.plurx.app.invitations

import org.junit.Assert.*
import org.junit.Test

class InvitationResidentLifetimeTest {
    private fun run(id: String = "one", profile: String = "profile", installation: String = "installation") = InvitationResidentLifetime.Run(profile, installation, id)
    @Test fun teardownWaitsForSocketsAndUsesLatestRejectedStartWithoutAdmittingReplacement() {
        val service = InvitationServiceTeardown()
        val old = run(); val fresh = run("two")
        assertTrue(service.start(old, 1)); assertTrue(service.retire(old))
        assertFalse(service.start(fresh, 2)); assertNull(service.rejectedStopId())
        assertNull(service.completed(fresh)); assertFalse(service.retire(fresh))
        assertEquals(2, service.completed(old))
        assertFalse(service.retire(old)); assertFalse(service.start(fresh, 3))
        assertEquals(3, service.rejectedStopId())
        // New Android Service instance, not the retired instance, owns fresh sockets.
        val replacementService = InvitationServiceTeardown()
        assertTrue(replacementService.start(fresh, 1)); assertFalse(replacementService.retire(old))
        assertNull(replacementService.completed(old)); assertTrue(replacementService.retire(fresh))
        assertEquals(1, replacementService.completed(fresh))
    }
    @Test fun permissionRejectedAfterAdmissionRetiresExactRunBeforeAndroidDestroy() {
        val teardown = InvitationServiceTeardown(); val owner = run()
        assertTrue(teardown.start(owner, 1))
        // Permission rejection uses the same owner teardown before any network/context attachment.
        assertTrue(teardown.retire(owner)); assertEquals(1, teardown.completed(owner))
        assertFalse(teardown.start(run("later"), 2)); assertEquals(2, teardown.rejectedStopId())
    }
    @Test fun callbackLossStopAndReplacementCannotRearmOrRetireAnotherRun() {
        val owner = InvitationResidentLifetime()
        val first = run(); val replacement = run("two")
        assertTrue(owner.begin(first)); assertFalse(owner.begin(replacement))
        assertTrue(owner.available(first, "wifi-one")); assertFalse(owner.available(first, "wifi-two"))
        assertFalse(owner.lost(first, "wifi-two")); assertTrue(owner.lost(first, "wifi-one"))
        assertFalse(owner.end(run(profile = "another-origin"))); assertFalse(owner.end(run(installation = "another-phone")))
        assertTrue(owner.end(first)); assertFalse(owner.available(first, "late-wifi"))
        assertTrue(owner.begin(replacement)); assertFalse(owner.end(first)); assertTrue(owner.owns(replacement))
        assertFalse(owner.available(first, "late-wifi")); assertTrue(owner.available(replacement, "wifi-two"))
        assertFalse(owner.lost(first, "wifi-one")); assertTrue(owner.end(replacement)); assertFalse(owner.owns(replacement))
    }
}
