package tv.plurx.app.player

import okhttp3.MediaType.Companion.toMediaType
import okhttp3.ResponseBody.Companion.toResponseBody
import retrofit2.HttpException
import retrofit2.Response
import org.junit.Assert.*
import org.junit.Test

/** The production mutation gate, exercised independently of Android player and network clocks. */
class CaptionActionFenceTest {
    private fun gate(): CaptionActionFence { var id = 0; return CaptionActionFence { "request-${++id}" } }
    @Test fun duplicateRetryHasOneInFlightOwner() {
        val gate = gate(); val owner = gate.begin("retry:job")!!
        assertNull(gate.begin("retry:job")); assertTrue(gate.owns(owner))
        gate.finish(owner); assertNotNull(gate.begin("retry:job"))
    }
    @Test fun stopDuringRetryCannotBeClearedByItsLateAcceptance() {
        val gate = gate(); val owner = gate.begin("retry:job")!!
        gate.stop(); assertTrue(gate.stoppedSince(owner)); assertTrue(gate.owns(owner))
        gate.finish(owner)
        val explicitNextRetry = gate.begin("retry:job")!!
        assertFalse(gate.stoppedSince(explicitNextRetry))
        gate.stop(); assertTrue(gate.stoppedSince(explicitNextRetry))
    }
    @Test fun ambiguousCreateReplayKeepsExactRequestIdentityAndTheNewerStop() {
        val gate = gate(); val first = gate.begin("create:file:source:language")!!
        gate.stop(); gate.finish(first, ambiguous = true)
        val replay = gate.begin("create:file:source:language")!!
        assertEquals(first.requestId, replay.requestId); assertTrue(replay.replay); assertTrue(gate.stoppedSince(replay))
        assertFalse(gate.owns(first)); assertTrue(gate.owns(replay))
    }
    @Test fun ambiguousRetryReorderedRefusalCannotFinishOrOverwriteTheCurrentOwner() {
        val gate = gate(); val old = gate.begin("retry:job:epoch")!!
        gate.finish(old, ambiguous = true)
        val current = gate.begin("retry:job:epoch")!!
        assertEquals(old.requestId, current.requestId)
        assertFalse(gate.owns(old)); assertFalse(gate.finish(old)); assertTrue(gate.owns(current)); assertTrue(gate.inFlight)
    }
    @Test fun changedSourceOrReleasedOwnerCannotAdoptAnOldReceipt() {
        val gate = gate(); val old = gate.begin("create:old-source")!!
        gate.invalidate()
        val current = gate.begin("create:new-source")!!
        assertNotEquals(old.requestId, current.requestId); assertFalse(gate.owns(old)); assertTrue(gate.owns(current))
        gate.invalidate(); assertFalse(gate.owns(current)); assertFalse(gate.finish(current))
    }
    @Test fun staleOperationViewCannotDisplaceAnAcceptedRetryReceipt() {
        val view = CaptionViewFence(); view.accept("operation", "2", "9007199254740994")
        assertFalse(view.acceptCurrent("operation", "1", "9007199254740993"))
        assertFalse(view.acceptCurrent("operation", "2", "9007199254740993"))
        assertTrue(view.acceptCurrent("operation", "2", "9007199254740994"))
    }
    @Test fun untypedHttpAuthorityRefusalsAreDefinitiveWhileServiceFailuresRemainAmbiguous() {
        fun failure(status: Int) = HttpException(Response.error<okhttp3.ResponseBody>(status,
            "{}".toResponseBody("application/json".toMediaType())))
        assertFalse(captionActionAmbiguous(failure(401)))
        assertFalse(captionActionAmbiguous(failure(403)))
        assertFalse(captionActionAmbiguous(failure(409)))
        assertTrue(captionActionAmbiguous(failure(408)))
        assertTrue(captionActionAmbiguous(failure(503)))
    }

}
