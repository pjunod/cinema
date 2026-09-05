package tv.plurx.app.livetv

import android.content.Context
import android.os.SystemClock
import android.util.AtomicFile
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.async
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.File

internal interface LiveTvBarrierStore {
    fun pending(): Boolean
    fun setPending(pending: Boolean)
}

/** One byte of uncertainty, not a token, profile, channel or capability. */
internal class LiveTvFileBarrierStore(context: Context) : LiveTvBarrierStore {
    private val file = AtomicFile(File(context.noBackupFilesDir, "live-tv-start.pending"))
    override fun pending(): Boolean = file.baseFile.exists() || File(file.baseFile.path + ".bak").exists()
    override fun setPending(pending: Boolean) {
        if (pending) {
            val output = file.startWrite()
            try { output.write(1); file.finishWrite(output) } catch (error: Exception) {
                file.failWrite(output)
                throw error
            }
        } else {
            file.delete()
            check(!pending()) { "pending marker could not be removed" }
        }
    }
}

/** Main-thread owned and shared across every profile/lease in this process. */
internal class LiveTvStartBarrier(
    private val store: LiveTvBarrierStore,
    private val now: () -> Long = SystemClock::elapsedRealtime,
) {
    private var until: Long? = null
    private var storageFailed = false
    init {
        try { if (store.pending()) until = now() + 90_000 } catch (_: Exception) { storageFailed = true }
    }
    val pending: Boolean get() = until?.let { now() < it } == true
    fun begin() {
        if (storageFailed) throw LiveTvFailure("storage_unavailable")
        if (pending) throw LiveTvFailure("start_outcome_unknown")
        arm()
    }
    fun arm() {
        try { store.setPending(true) } catch (_: Exception) { throw LiveTvFailure("storage_unavailable") }
        until = now() + 90_000
    }
    fun acquired() { until = null } // The disk marker survives an active-session crash.
    fun confirm() {
        try { store.setPending(false); until = null } catch (_: Exception) {
            // A confirmed server release is safe, but storage failure cannot
            // justify forgetting uncertainty in this or a future process.
            until = now() + 90_000
        }
    }
}

/** Scope belongs to the application, so a caller leaving a screen cannot
 * cancel ownership bookkeeping. Every next start first confirms old DELETE. */
internal class LiveTvLease(
    private val requests: LiveTvRequests,
    private val barrier: LiveTvStartBarrier,
    private val scope: CoroutineScope,
) {
    private val mutex = Mutex()
    private var generation = 0L
    var current: LiveTvStarted? = null
        private set

    fun start(channel: String): Deferred<LiveTvStarted?> {
        val mine = ++generation
        return scope.async {
            mutex.withLock {
                releaseCurrent()
                if (mine != generation) return@withLock null
                barrier.begin()
                val info = try { requests.start(channel) } catch (error: Exception) {
                    if (error !is LiveTvFailure || error.code in setOf("start_outcome_unknown", "owner_unavailable", "startup_timeout", "stream_failed")) {
                        runCatching { barrier.arm() }
                        throw LiveTvFailure("start_outcome_unknown")
                    }
                    barrier.confirm()
                    throw error
                }
                if (info.session_id.isEmpty() || info.session_id.length > 1024) {
                    runCatching { barrier.arm() }
                    throw LiveTvFailure("start_outcome_unknown")
                }
                current = info
                barrier.acquired()
                if (mine != generation || !info.live) { releaseCurrent(); null } else info
            }
        }
    }
    fun stop(): Deferred<Unit> {
        ++generation
        return scope.async { mutex.withLock { releaseCurrent() } }
    }
    fun heartbeatMarker() { if (current != null) barrier.arm() }
    private suspend fun releaseCurrent() {
        val previous = current ?: return
        runCatching { barrier.arm() } // Storage failure must not prevent DELETE.
        requests.release(previous.session_id)
        current = null
        barrier.confirm()
    }
}

internal class LiveTvWatchdog(private val now: () -> Long = SystemClock::elapsedRealtime) {
    private var lastProgress = now()
    private var lastFrames = 0
    fun observe(renderedFrames: Int, advancing: Boolean): Boolean {
        // ExoPlayer position is relative to the sliding window and can move
        // backwards while healthy. Only actual rendered video renews a lease.
        // A decoder replacement may reset the counter; one positive new count
        // is progress, but a frozen/repeated count never extends the budget.
        if (advancing && renderedFrames > 0 && renderedFrames != lastFrames) {
            lastFrames = renderedFrames
            lastProgress = now()
            return true
        }
        return false
    }
    val expired: Boolean get() = now() - lastProgress >= 30_000
}
