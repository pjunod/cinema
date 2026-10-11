package tv.plurx.app.player

import java.io.IOException
import org.junit.Assert.*
import org.junit.Test

class ContinuousQueueEpochsTest {
    @Test fun queueResetNeedsActualZeroIndicesAndNewSamplesCannotBorrowTheEarlierEpoch() {
        val epochs = ContinuousQueueEpochs()
        val queue = Any()
        val other = Any()
        assertEquals(0L, epochs.accepted(queue, 0, 1))
        assertEquals(0L, epochs.accepted(queue, 1, 2))
        epochs.empty(queue, 2, 2, 2)
        assertEquals(0L, epochs.current(queue))
        epochs.empty(queue, 0, 0, 1)
        assertEquals(0L, epochs.current(queue))
        epochs.empty(queue, 0, 0, 0)
        assertEquals(1L, epochs.current(queue))
        epochs.empty(queue, 0, 0, 0)
        assertEquals(1L, epochs.current(queue))
        assertEquals(1L, epochs.accepted(queue, 0, 1))
        assertEquals(2L, epochs.accepted(queue, 0, 1))
        assertEquals(0L, epochs.accepted(other, 0, 1))
        assertTrue(runCatching { epochs.accepted(queue, -1, 0) }.exceptionOrNull() is IOException)
        assertTrue(runCatching { epochs.accepted(queue, 1, 3) }.exceptionOrNull() is IOException)
    }
}
