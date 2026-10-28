package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.awaitCancellation
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.RefusalException

class CaptionPollingLifecycleTest {
    private fun refusal(status: Int, code: String? = null) = RefusalException(status, code, "fixture", null)
    @Test fun oneServiceFailureOrTimeoutKeepsTheOwnedReadLoopAliveUntilRecovery() = runBlocking {
        for (failure in listOf(refusal(503), IOException("timeout"))) {
            var current = true
            var reads = 0
            var failures = 0
            val cadence = mutableListOf<Long>()
            captionPollingLifecycle({ current }, read = {
                reads++
                if (reads == 1) throw failure
                current = false
            }, renew = {}, readFailed = { failures++ }, renewalFailed = { fail("Renewal must not fail") },
                pause = { cadence += it; yield() })
            assertEquals(2, reads)
            assertEquals(1, failures)
            assertTrue(cadence.contains(4_000L))
            assertTrue(cadence.all { it in 2_000..8_000 })
        }
    }
    @Test fun demandRenewalContinuesWhileOneWindowReadIsHeldBeyondItsTtl() = runBlocking {
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        var current = true
        var renewals = 0
        var elapsed = 0L
        captionPollingLifecycle({ current }, read = {
            entered.complete(Unit)
            release.await()
            current = false
        }, renew = {
            entered.await()
            renewals++
            if (elapsed >= 18_000) release.complete(Unit)
        }, readFailed = { fail("Held read must remain current") }, renewalFailed = { fail("Renewal must succeed") },
            pause = { interval -> assertEquals(3_000L, interval); elapsed += interval; yield() })
        assertTrue(elapsed >= 18_000)
        assertTrue(renewals >= 7)
    }
    @Test fun aRenewalFailureRetriesBelowTheDemandTtlWithoutCancellingTheWindowRead() = runBlocking {
        val entered = CompletableDeferred<Unit>()
        val release = CompletableDeferred<Unit>()
        var current = true
        var renewals = 0
        var failures = 0
        captionPollingLifecycle({ current }, read = {
            entered.complete(Unit); release.await(); current = false
        }, renew = {
            entered.await(); renewals++
            if (renewals == 1) throw IOException("lost renewal")
            release.complete(Unit)
        }, readFailed = { fail("Window read must not be cancelled by a transient renewal") }, renewalFailed = { failures++ },
            pause = { assertEquals(3_000L, it); yield() })
        assertEquals(2, renewals)
        assertEquals(1, failures)
    }
    @Test fun repeatedTransientReadsBackOffWithinTheServerCadenceBound() = runBlocking {
        var current = true
        var failures = 0
        val cadence = mutableListOf<Long>()
        captionPollingLifecycle({ current }, read = {
            if (failures < 4) throw refusal(503)
            current = false
        }, renew = {}, readFailed = { failures++ }, renewalFailed = {}, pause = {
            cadence += it; yield()
        })
        assertEquals(4, failures)
        assertTrue(cadence.contains(4_000L))
        assertTrue(cadence.contains(8_000L))
        assertTrue(cadence.all { it <= 8_000 })
    }
    @Test fun authSourceAndInvalidProtocolErrorsRetireInsteadOfRetryingForever() = runBlocking {
        for (failure in listOf(refusal(401), refusal(403), refusal(409, "source_changed"),
                IllegalArgumentException("hash mismatch"))) {
            var reads = 0
            try {
                captionPollingLifecycle({ true }, read = { reads++; throw failure }, renew = {},
                    readFailed = { fail("Terminal refusal must retire") }, renewalFailed = {},
                    pause = { fail("Terminal refusal cannot schedule a retry") })
                fail("Expected retirement")
            } catch (e: Exception) {
                // Coroutine stack-trace recovery can copy standard exceptions.
                assertEquals(failure.javaClass, e.javaClass)
                assertEquals(failure.message, e.message)
            }
            assertEquals(1, reads)
        }
        assertTrue(captionReadRetryable(refusal(425, "source_verifying")))
        assertTrue(captionReadRetryable(refusal(410, "translation_snapshot_expired")))
        assertFalse(captionReadRetryable(refusal(503, "source_missing")))
    }
    @Test fun releasingAnOwnerCancelsBothItsStalledReadAndItsRenewalChild() = runBlocking {
        val entered = CompletableDeferred<Unit>()
        val renewing = CompletableDeferred<Unit>()
        var readsCancelled = false
        var renewalCancelled = false
        val owner = launch {
            captionPollingLifecycle({ true }, read = {
                entered.complete(Unit)
                try { awaitCancellation() } finally { readsCancelled = true }
            }, renew = {
                renewing.complete(Unit)
                try { awaitCancellation() } finally { renewalCancelled = true }
            }, readFailed = { fail("Owner cancellation is not a transient failure") }, renewalFailed = {
                fail("Owner cancellation must never renew again")
            })
        }
        entered.await(); renewing.await(); owner.cancelAndJoin()
        assertTrue(readsCancelled)
        assertTrue(renewalCancelled)
    }
    @Test fun aFatalRenewalRetiresItsSiblingReadWithoutSchedulingAnotherRequest() = runBlocking {
        val entered = CompletableDeferred<Unit>()
        val failure = refusal(403)
        var cancelled = false
        var renewals = 0
        try {
            captionPollingLifecycle({ true }, read = {
                entered.complete(Unit)
                try { awaitCancellation() } finally { cancelled = true }
            }, renew = { entered.await(); renewals++; throw failure }, readFailed = {
                fail("Authority retirement is not a transient read")
            }, renewalFailed = { fail("A forbidden renewal cannot be retried") },
                pause = { fail("Authority retirement cannot schedule another request") })
            fail("Expected authority retirement")
        } catch (e: Exception) { assertSame(failure, e) }
        assertEquals(1, renewals)
        assertTrue(cancelled)
    }

}
