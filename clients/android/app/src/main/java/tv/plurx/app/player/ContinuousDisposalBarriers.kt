package tv.plurx.app.player

import java.io.IOException
import kotlinx.coroutines.CompletableDeferred

/** Reads wait before reservation or extraction. A failed disposition keeps
 * its barrier until the exact operation is replayed and provenance removed. */
internal class ContinuousDisposalBarriers {
    private val pending = LinkedHashMap<String, CompletableDeferred<Unit>>()
    @Synchronized fun begin(key: String): Boolean {
        if (key in pending) return false
        if (pending.size >= 128) throw IOException("Continuous disposal barrier bound")
        pending[key] = CompletableDeferred()
        return true
    }
    suspend fun await(key: String) {
        while (true) {
            val barrier = synchronized(this) { pending[key] } ?: return
            barrier.await()
        }
    }
    @Synchronized fun retired(key: String) { pending.remove(key)?.complete(Unit) }
}
