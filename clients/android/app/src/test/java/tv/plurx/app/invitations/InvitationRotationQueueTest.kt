package tv.plurx.app.invitations

import org.junit.Assert.*
import org.junit.Test

class InvitationRotationQueueTest {
    @Test fun explicitUnknownEnrollmentDoesNotDrainPendingTokenUntilFreshExplicitRetry() {
        val queue = InvitationRotationQueue(); queue.offer("callback-token")
        val bindings = mapOf("screen" to "same-ready-consent")
        queue.explicitRetry("screen"); queue.unknown("screen")
        assertTrue(queue.remaining(bindings).isEmpty())
        queue.offer("callback-token"); assertTrue(queue.remaining(bindings).isEmpty())
        queue.explicitRetry("screen"); assertEquals(listOf("screen"), queue.remaining(bindings).map { it.first })
        assertTrue(queue.remaining(emptyMap()).isEmpty())
    }
    @Test fun firstScreenSuccessDoesNotDropSecondUncertainRenewalOrEnableAnOffScreen() {
        val queue = InvitationRotationQueue(); queue.offer("sdk-token")
        var bindings = mapOf("one" to "one-start", "two" to "two-start")
        assertEquals(listOf("one", "two"), queue.remaining(bindings).map { it.first })
        bindings = mapOf("one" to "one-confirmed", "two" to "two-start")
        queue.complete("one", "sdk-token", "one-confirmed"); queue.unknown("two")
        queue.offer("sdk-token"); assertTrue(queue.remaining(bindings).isEmpty())
        queue.explicitRetry("two"); assertEquals(listOf("two"), queue.remaining(bindings).map { it.first })
        queue.unknown("two")
        assertTrue(queue.remaining(mapOf("one" to "one-confirmed")).isEmpty())
        queue.explicitRetry("two"); assertTrue(queue.remaining(mapOf("one" to "one-confirmed")).isEmpty())
        queue.offer("new-token"); assertEquals(listOf("one"), queue.remaining(mapOf("one" to "one-confirmed")).map { it.first })
        queue.clearAuthority(); assertTrue(queue.remaining(emptyMap()).isEmpty())
    }
}
