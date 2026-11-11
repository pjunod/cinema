package tv.plurx.app.player

import androidx.media3.common.Player
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.encodeToString
import org.junit.Assert.*
import org.junit.Test
import tv.plurx.app.data.*
import java.util.UUID

class CaptionContinuityTest {
    private fun continuity(): CaptionContinuity {
        var next = 0L
        return CaptionContinuity { UUID(0, ++next).toString() }
    }
    private fun receipt(generation: String, continuity: String?) = CaptionReceipt(1, "updated", "operation", "7", "11",
        "partial", demand_id = "demand", demand_generation = generation, server_now_ms = 1_000, continuity_id = continuity)
    private fun refuses(block: () -> Unit) {
        try { block(); fail("Expected continuity refusal") } catch (_: IllegalArgumentException) { }
    }
    @Test fun ordinaryClockAdvancePauseRateAndHeartbeatGenerationsPreserveTraversalIdentity() {
        val continuity = continuity()
        continuity.bind(41, 1)
        val first = continuity.id
        for ((generation, position, paused, speed) in listOf(
            Quad("1", 29_000L, false, 1.0), Quad("2", 31_000L, false, 1.0),
            Quad("3", 31_000L, true, 1.0), Quad("4", 32_000L, false, 2.0))) {
            val demand = CaptionDemand(1, UUID.randomUUID().toString(), 60_000, "7", generation,
                "playback", position, speed, paused, continuity.id)
            assertEquals(first, demand.continuity_id)
            captionReceiptContinuity(receipt(generation, first), demand.continuity_id)
        }
        assertEquals(0L, continuity.discontinuitySerial)
    }
    private data class Quad(val generation: String, val position: Long, val paused: Boolean, val speed: Double)
    @Test fun explicitSeeksRevokeContinuityEvenWhenTheyTargetTheSamePosition() {
        val continuity = continuity()
        val first = continuity.id
        continuity.discontinuity()
        val samePositionSeek = continuity.id
        continuity.discontinuity()
        assertNotEquals(first, samePositionSeek)
        assertNotEquals(samePositionSeek, continuity.id)
        assertEquals(2L, continuity.discontinuitySerial)
    }
    @Test fun mediaSessionAndRendererDiscontinuitiesAdvanceWithoutClockJumpHeuristics() {
        val continuity = continuity()
        val first = continuity.id
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_SEEK)
        assertNotEquals(first, continuity.id)
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_SEEK_ADJUSTMENT)
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_INTERNAL)
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_AUTO_TRANSITION)
        assertEquals(4L, continuity.discontinuitySerial)
    }
    @Test fun ownedQualityAndRecoverySeeksPreserveContinuityAndCannotSuppressLaterExternalSeeks() {
        val continuity = continuity()
        val first = continuity.id
        continuity.ownedMediaMutation {
            continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_SEEK)
            continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_INTERNAL)
        }
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_REMOVE)
        assertEquals(first, continuity.id)
        try {
            continuity.ownedMediaMutation<Unit> { error("failed continuous attachment") }
        } catch (_: IllegalStateException) { }
        continuity.rendererDiscontinuity(Player.DISCONTINUITY_REASON_SEEK)
        assertNotEquals(first, continuity.id)
        assertEquals(1L, continuity.discontinuitySerial)
    }
    @Test fun sourceAudioAndOffBeginNewTraversalsWhileQualityOwnerReplacementPreservesThem() {
        val continuity = continuity()
        continuity.bind(41, 1)
        val first = continuity.id
        continuity.bind(41, 1) // A replacement Controller restores this same intent.
        assertEquals(first, continuity.id)
        continuity.bind(41, 2)
        val audio = continuity.id
        assertNotEquals(first, audio)
        continuity.bind(42, 2)
        assertNotEquals(audio, continuity.id)
        val source = continuity.id
        continuity.endTraversal()
        continuity.bind(42, 2)
        assertNotEquals(source, continuity.id)
        assertEquals(3L, continuity.discontinuitySerial)
    }
    @Test fun seekDuringAmbiguousRenewalResolvesExactOldBodyThenSendsCurrentUuidOnNextGeneration() {
        val continuity = continuity()
        val pending = CaptionDemandAttempt()
        var ids = 0
        fun request(value: CaptionReceipt) = pending.request(value, "7") {
            CaptionDemand(1, "request-${++ids}", 60_000, "7", nextCaptionCounter(requireNotNull(value.demand_generation)),
                "playback", 31_000, 1.0, false, continuity.id)
        }
        val initial = receipt("1", continuity.id)
        val old = request(initial)
        pending.finish(old, ambiguous = true)
        continuity.discontinuity()
        val current = continuity.id
        assertSame(old, request(initial))
        assertNotEquals(current, old.continuity_id)
        val ack = receipt("2", old.continuity_id)
        captionReceiptContinuity(ack, old.continuity_id)
        pending.finish(old, ambiguous = false)
        assertEquals(current, continuity.id)
        val next = request(ack)
        assertEquals("3", next.demand_generation)
        assertEquals(current, next.continuity_id)
        assertNotEquals(old.request_id, next.request_id)
        assertEquals(2, ids)
    }
    @Test fun everyProducerMutationCarriesItsSnapshotAndEchoValidationRemainsDemandScoped() {
        val id = UUID(0, 1).toString()
        val bodies = listOf(
            Net.json.encodeToString(CaptionCreate(1, "request", 60_000, "100", "20", 1, "de", "de", "transcribe",
                "small", "a".repeat(64), "playback", 1_000, 1.0, false, id)),
            Net.json.encodeToString(CaptionDemand(1, "request", 60_000, "7", "2", "playback", 1_000, 1.0, false, id)),
            Net.json.encodeToString(CaptionRetry("operation", 1, "request", 60_000, "7", "11", "stopped", "playback", 1_000, 1.0, false, id)),
            Net.json.encodeToString(CaptionRangeRetry(1, "request", 60_000, "7", "partial", 0, "3", "failed", "playback", 1_000, 1.0, false, "range", id)),
        )
        bodies.forEach { assertTrue(it.contains("\"continuity_id\":\"$id\"")) }
        captionReceiptContinuity(receipt("2", id), id)
        captionReceiptContinuity(receipt("2", null), null)
        refuses { captionReceiptContinuity(receipt("2", null), id) }
        refuses { captionReceiptContinuity(receipt("2", UUID(0, 2).toString()), id) }
        refuses { captionReceiptContinuity(receipt("2", "00000000-0000-0000-0000-000000000000"), "00000000-0000-0000-0000-000000000000") }
    }
    @Test fun actualSeekWakesTheExistingRenewalChildWhileItsWindowReadIsStalled() = runBlocking {
        val readStarted = CompletableDeferred<Unit>()
        val firstRenewal = CompletableDeferred<Unit>()
        val releaseRead = CompletableDeferred<Unit>()
        val wake = Channel<Unit>(Channel.CONFLATED)
        var current = true
        var renewals = 0
        val owner = launch {
            captionPollingLifecycle({ current }, read = {
                readStarted.complete(Unit); releaseRead.await(); current = false
            }, renew = {
                renewals++
                if (renewals == 1) firstRenewal.complete(Unit) else releaseRead.complete(Unit)
            }, readFailed = { fail("Read must remain owned") }, renewalFailed = { fail("Renewal must remain owned") },
                renewalPause = { interval -> assertEquals(3_000L, interval); wake.receive() })
        }
        readStarted.await(); firstRenewal.await()
        wake.trySend(Unit)
        owner.join()
        assertEquals(2, renewals)
    }
    @Test fun heldViewerSeekRevokesBothTheOldTraversalAndAnyPreSeekRequestWhenItsDestinationAttaches() {
        val continuity = continuity()
        val old = continuity.id
        continuity.viewerSeekRequested()
        val waiting = continuity.id
        assertNotEquals(old, waiting)
        assertTrue(continuity.executionAttached())
        val attached = continuity.id
        assertNotEquals(waiting, attached)
        assertFalse(continuity.executionAttached()) // Ordinary quality/track execution has no viewer destination.
        assertEquals(attached, continuity.id)
        assertEquals(2L, continuity.discontinuitySerial)
    }

    @Test fun plannedQualityOwnerTransferKeepsTheLeaseLiveAndRejectsPredecessorCleanup() {
        val producer = CaptionProducerOwnership()
        val playback = producer.playbackId
        var oldReleases = 0
        var newReleases = 0
        val old = producer.claim()
        producer.retain(old) { oldReleases++ }
        producer.prepareTransfer()
        producer.release(old)
        assertEquals(0, oldReleases)
        val successor = producer.claim()
        assertEquals(playback, producer.playbackId)
        producer.retain(successor) { newReleases++ }
        producer.release(old)
        assertEquals(0, oldReleases)
        assertEquals(0, newReleases)
        producer.release(successor)
        assertEquals(1, newReleases)
    }
    @Test fun abandonedTransferAndExplicitNewTraversalRetireTheirBoundedLeaseCapsules() {
        val producer = CaptionProducerOwnership()
        val old = producer.claim()
        val playback = producer.playbackId
        var released = 0
        producer.retain(old) { released++ }
        producer.prepareTransfer()
        producer.release(old)
        producer.claim() // Incoming options/create has not succeeded yet.
        producer.retire()
        assertEquals(1, released)
        assertNotEquals(playback, producer.playbackId)
        val another = producer.claim()
        val nextPlayback = producer.playbackId
        producer.retain(another) { released++ }
        producer.newTraversal() // Off, source or audio replacement.
        assertEquals(2, released)
        assertNotEquals(nextPlayback, producer.playbackId)
        producer.release(another)
        assertEquals(2, released)
    }
    @Test fun incomingOwnerCanClaimBeforeOldDisposalWithoutBorrowingOrReleasingItsLease() {
        val producer = CaptionProducerOwnership()
        var oldReleases = 0
        var newReleases = 0
        val old = producer.claim()
        producer.retain(old) { oldReleases++ }
        producer.prepareTransfer()
        val successor = producer.claim()
        assertFalse(producer.owns(old))
        producer.retain(old) { fail("A stale owner cannot replace the retained capsule") }
        producer.release(old)
        assertEquals(0, oldReleases)
        producer.retain(successor) { newReleases++ }
        producer.retire()
        assertEquals(0, oldReleases)
        assertEquals(1, newReleases)
    }

}
