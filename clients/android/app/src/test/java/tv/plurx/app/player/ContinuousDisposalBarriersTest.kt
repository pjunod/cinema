package tv.plurx.app.player

import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test

class ContinuousDisposalBarriersTest {
    @Test fun reentryWaitsForActualRetirementAndCancellationDoesNotRemoveTheBarrier() = runBlocking {
        val barriers = ContinuousDisposalBarriers()
        assertTrue(barriers.begin("video:a:0"))
        assertFalse(barriers.begin("video:a:0"))
        val cancelled = launch(start = CoroutineStart.UNDISPATCHED) { barriers.await("video:a:0") }
        cancelled.cancelAndJoin()
        val reentry = async(start = CoroutineStart.UNDISPATCHED) { barriers.await("video:a:0") }
        assertFalse(reentry.isCompleted)
        barriers.await("audio:a:0")
        barriers.retired("video:a:0")
        reentry.await()
        assertTrue(barriers.begin("video:a:0"))
        barriers.retired("video:a:0")
        assertTrue(barriers.begin("artifact:audio:a:hash"))
        val alias = async(start = CoroutineStart.UNDISPATCHED) { barriers.await("artifact:audio:a:hash") }
        // A different URI can finish fetching, but the shared digest still
        // blocks authorization until old physical provenance is retired.
        barriers.await("audio:a:another-uri")
        assertFalse(alias.isCompleted)
        barriers.retired("artifact:audio:a:hash")
        alias.await()
    }
}
