package tv.plurx.app.player

import java.io.IOException
import java.util.IdentityHashMap

/** An observed zero write index after accepted samples proves queue reset.
 * Reading or front-discarding samples keeps absolute indices and cannot do so. */
internal class ContinuousQueueEpochs {
    private data class State(var epoch: Long = 0, var write: Int = 0)
    private val queues = IdentityHashMap<Any, State>()
    @Synchronized fun accepted(queue: Any, before: Int, after: Int): Long {
        if (before < 0 || after.toLong() - before.toLong() != 1L) throw IOException("Continuous queue index bound")
        val state = queues[queue] ?: State().also {
            if (queues.size >= 32) throw IOException("Continuous queue epoch bound")
            queues[queue] = it
        }
        if (before == 0 && state.write > 0) state.epoch++
        state.write = after
        return state.epoch
    }
    @Synchronized fun empty(queue: Any, first: Int, read: Int, write: Int) {
        val state = queues[queue] ?: return
        if (state.write > 0 && first == 0 && read == 0 && write == 0) {
            state.epoch++
            state.write = 0
        }
    }
    @Synchronized fun current(queue: Any): Long = queues[queue]?.epoch ?: 0
}
