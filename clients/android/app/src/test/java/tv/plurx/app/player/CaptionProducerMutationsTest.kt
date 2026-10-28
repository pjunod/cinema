package tv.plurx.app.player

import kotlinx.coroutines.*
import kotlinx.serialization.encodeToString
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.*

/** Held server transports exercise the production owner and shared mutation capsule together. */
class CaptionProducerMutationsTest {
    private val profile = CaptionProfile("a".repeat(64), "base", "transcribe", null, listOf("ja"))
    private fun binding() = captionBinding(CaptionOptions(1, "41", "9007199254740993", "123", 120_000, 125,
        "source-clock-v1", listOf(CaptionAudio(1, 1, 9, CaptionCapability(true))), listOf(profile),
        CaptionCapability(true), CaptionPermissions(true, true, true, true), emptyList(), server_now_ms = 1000),
        1, ProgressiveCaptionSelection("transcribe", "ja", profile.profile_id))
    private fun create(intent: ProgressiveCaptionIntent) = CaptionProducerRequest(41, binding(), create =
        CaptionCreate(1, "create-request", 60_000, "9007199254740993", "123", 1, "ja", "ja", "transcribe",
            "base", profile.profile_id, intent.producer.playbackId, 29_000, 1.0, false, intent.continuity.id))
    private fun retry(intent: ProgressiveCaptionIntent, range: Boolean) = CaptionProducerRequest(41, binding(), retry =
        captionRetryAttempt(CaptionRetryTarget("operation", "7", "11", if (range) "partial" else "stopped",
            if (range) CaptionFailedRange(3, "4", "failed", true, 1) else null), "retry-request", 60_000,
            intent.producer.playbackId, 91_000, 1.0, false, intent.continuity.id))
    private fun receipt(request: CaptionProducerRequest, generation: String = "2") = CaptionReceipt(1, "accepted", "operation",
        "7", "12", "partial", demand_id = "demand", demand_generation = generation, server_now_ms = 1_000,
        continuity_id = request.create?.continuity_id ?: request.retry?.range?.continuity_id ?: request.retry?.operation?.continuity_id)
    private fun wire(request: CaptionProducerRequest): String {
        request.create?.let { return Net.json.encodeToString(it) }
        val retry = requireNotNull(request.retry)
        return retry.range?.let { Net.json.encodeToString(it) } ?: Net.json.encodeToString(requireNotNull(retry.operation))
    }

    @Test fun heldCreateReplacementSettlesExactBodyBeforeSuccessorMutationAndCannotReleaseSuccessor() = runBlocking {
        val intent = ProgressiveCaptionIntent()
        val oldOwner = intent.producer.claim()
        val request = create(intent)
        val held = CompletableDeferred<Unit>()
        val seen = mutableListOf<String>()
        val durable = mutableMapOf<String, CaptionReceipt>()
        var effects = 0; var releases = 0
        intent.mutations.install(oldOwner, CaptionProducerMutations.Pending(request, false, intent.stopRevision, { true }) {
            seen += wire(request)
            if (seen.size == 1) withContext(NonCancellable) { held.await() }
            durable.getOrPut(request.create!!.request_id) { effects++; receipt(request) }
        })
        val old = async(start = CoroutineStart.UNDISPATCHED) { intent.mutations.settle(oldOwner) { true } }
        intent.producer.prepareTransfer(); intent.producer.release(oldOwner)
        val successor = intent.producer.claim()
        assertNotNull(intent.mutations.pending) // Fresh Create must wait for this exact request.
        intent.continuity.discontinuity()
        val currentContinuity = intent.continuity.id
        val settled = requireNotNull(intent.mutations.settle(successor) { true })
        assertEquals(request, settled.pending.request)
        captionReceiptContinuity(settled.receipt, request.create!!.continuity_id)
        assertEquals(currentContinuity, intent.continuity.id)
        intent.producer.retain(successor) { releases++ }
        held.complete(Unit)
        assertNull(old.await())
        intent.producer.release(oldOwner)
        assertEquals(0, releases)
        assertEquals(1, effects)
        assertEquals(listOf(wire(request), wire(request)), seen)
        assertNull(intent.mutations.pending)
        intent.producer.release(successor)
        assertEquals(1, releases)
    }

    @Test fun heldRangeRetryReplacementReplaysOriginalTargetOnceAndOldCompletionCannotClearLaterMutation() = runBlocking {
        val intent = ProgressiveCaptionIntent()
        val oldOwner = intent.producer.claim()
        val request = retry(intent, true)
        val held = CompletableDeferred<Unit>()
        val durable = mutableMapOf<String, CaptionReceipt>()
        val seen = mutableListOf<String>()
        var effects = 0; var releases = 0
        intent.mutations.install(oldOwner, CaptionProducerMutations.Pending(request, true, intent.stopRevision, { true }) {
            seen += wire(request)
            if (seen.size == 1) withContext(NonCancellable) { held.await() }
            durable.getOrPut(request.retry!!.range!!.request_id) { effects++; receipt(request) }
        })
        val old = async(start = CoroutineStart.UNDISPATCHED) { intent.mutations.settle(oldOwner) { true } }
        intent.producer.prepareTransfer(); intent.producer.release(oldOwner)
        val successor = intent.producer.claim()
        val settled = requireNotNull(intent.mutations.settle(successor) { true })
        assertEquals(3L, settled.pending.request.retry!!.range!!.core_index)
        assertEquals("4", settled.pending.request.retry!!.range!!.expected_range_revision)
        intent.producer.retain(successor) { releases++ }
        val later = CaptionProducerMutations.Pending(create(intent), false, intent.stopRevision, { true }) { receipt(request, "3") }
        intent.mutations.install(successor, later)
        held.complete(Unit)
        assertNull(old.await())
        intent.producer.release(oldOwner)
        assertSame(later, intent.mutations.pending)
        assertEquals(0, releases)
        assertEquals(1, effects)
        assertEquals(listOf(wire(request), wire(request)), seen)
    }

    @Test fun heldResumeReplacementPreservesExplicitResumeButNewerStopWins() = runBlocking {
        for (stopAfterSubmission in listOf(false, true)) {
            val intent = ProgressiveCaptionIntent().also { it.stopped = true }
            val oldOwner = intent.producer.claim()
            val request = retry(intent, false)
            val held = CompletableDeferred<Unit>()
            var calls = 0; var effects = 0
            val durable = mutableMapOf<String, CaptionReceipt>()
            intent.mutations.install(oldOwner, CaptionProducerMutations.Pending(request, true, intent.stopRevision, { true }) {
                calls++
                if (calls == 1) withContext(NonCancellable) { held.await() }
                durable.getOrPut(request.retry!!.operation!!.request_id) { effects++; receipt(request) }
            })
            val old = async(start = CoroutineStart.UNDISPATCHED) { intent.mutations.settle(oldOwner) { true } }
            intent.producer.prepareTransfer(); intent.producer.release(oldOwner)
            val successor = intent.producer.claim()
            if (stopAfterSubmission) intent.requestStop()
            val settled = requireNotNull(intent.mutations.settle(successor) { true })
            if (!settled.pending.requiresStop(intent.stopRevision, intent.stopped))
                intent.acceptDemand(settled.receipt.state, settled.pending.explicitResume)
            assertEquals(stopAfterSubmission, intent.stopped)
            held.complete(Unit)
            assertNull(old.await())
            assertEquals(1, effects)
            assertEquals(2, calls)
        }
    }

    @Test fun cancelledTransportKeepsTheExactBodyForReplacement() = runBlocking {
        val intent = ProgressiveCaptionIntent()
        val first = intent.producer.claim()
        val request = create(intent)
        val entered = CompletableDeferred<Unit>()
        var calls = 0
        val pending = intent.mutations.install(first, CaptionProducerMutations.Pending(request, false, 0, { true }) {
            if (++calls == 1) { entered.complete(Unit); awaitCancellation() }
            receipt(request)
        })
        val old = launch { intent.mutations.settle(first) { true } }
        entered.await(); old.cancelAndJoin()
        assertSame(pending, intent.mutations.pending)
        val successor = intent.producer.claim()
        assertSame(request, requireNotNull(intent.mutations.settle(successor) { true }).pending.request)
        assertEquals(2, calls)
    }

    @Test fun pendingMutationNeverBorrowsReplacementAuthority() = runBlocking {
        val intent = ProgressiveCaptionIntent()
        val first = intent.producer.claim()
        val request = create(intent)
        var originalAuthority = true; var sends = 0
        intent.mutations.install(first, CaptionProducerMutations.Pending(request, false, 0, { originalAuthority }) {
            sends++; receipt(request)
        })
        val successor = intent.producer.claim()
        originalAuthority = false
        try { intent.mutations.settle(successor) { true }; fail("Must retain original authority") }
        catch (_: IllegalStateException) { }
        assertEquals(0, sends)
        intent.retire()
        assertNull(intent.mutations.pending)
    }

    @Test fun duplicateReplayOnTheSameOwnerDoesNotSubmitTwice() = runBlocking {
        val intent = ProgressiveCaptionIntent()
        val owner = intent.producer.claim()
        val request = create(intent)
        val held = CompletableDeferred<Unit>()
        var sends = 0
        intent.mutations.install(owner, CaptionProducerMutations.Pending(request, false, 0, { true }) {
            sends++; held.await(); receipt(request)
        })
        val first = async(start = CoroutineStart.UNDISPATCHED) { intent.mutations.settle(owner) { true } }
        assertNull(intent.mutations.settle(owner) { true })
        held.complete(Unit)
        assertNotNull(first.await())
        assertEquals(1, sends)
    }
}
