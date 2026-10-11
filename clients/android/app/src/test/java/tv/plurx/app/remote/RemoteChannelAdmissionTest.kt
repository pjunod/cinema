package tv.plurx.app.remote

import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.async
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test

class RemoteChannelAdmissionTest {
    @Test fun expiredOrReplacedChannelNeverInvokesIncumbentAndPhysicalJobIsUntouched(): Unit = runBlocking {
        val admission = RemoteChannelAdmission()
        var current = true
        var invoked = 0
        val physical = CompletableDeferred<Unit>()
        val pending = async(start = CoroutineStart.UNDISPATCHED) { admission.run(1, { current }) { invoked++; true } }
        current = false
        assertEquals(RemoteOutcome.Unavailable, pending.await())
        assertEquals(0, invoked)
        assertFalse(physical.isCancelled)
        current = true
        assertEquals(RemoteOutcome.Applied, admission.run(0, { current }) { invoked++; true })
        assertEquals(1, invoked)
        physical.complete(Unit)
    }
    @Test fun networkRetirementCancelsOnlyItsOwnUninvokedCoalescingSlot(): Unit = runBlocking {
        val admission = RemoteChannelAdmission()
        var invoked = 0
        val pending = async(start = CoroutineStart.UNDISPATCHED) { admission.run(10000, { true }) { invoked++; true } }
        admission.retire()
        assertTrue(runCatching { pending.await() }.isFailure)
        assertEquals(0, invoked)
        assertEquals(RemoteOutcome.Applied, admission.run(0, { true }) { invoked++; true })
    }
}
