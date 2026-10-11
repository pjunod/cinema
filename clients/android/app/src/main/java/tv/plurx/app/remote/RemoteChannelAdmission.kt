package tv.plurx.app.remote

import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.delay

/** Only the network coalescing slot belongs here; physical scrub jobs do not. */
internal class RemoteChannelAdmission {
    private var serial = 0L
    private var operation: Job? = null
    fun retire() { serial++; operation?.cancel(); operation = null }
    suspend fun run(waitMs: Long, permitted: () -> Boolean, invoke: () -> Boolean): RemoteOutcome {
        val token = ++serial
        val task = currentCoroutineContext()[Job]
        operation?.takeIf { it !== task }?.cancel()
        operation = task
        try {
            delay(waitMs)
            if (serial != token || !permitted()) return RemoteOutcome.Unavailable
            return if (invoke()) RemoteOutcome.Applied else RemoteOutcome.Unavailable
        } finally { if (operation === task) operation = null }
    }
}
