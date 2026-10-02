package tv.plurx.app.player

import java.util.IdentityHashMap
import java.util.concurrent.atomic.AtomicReference

/** Ownership of actual output frames and asynchronous compositor receipts. */
internal class PreparedSurfaceReceipts<T : Any> {
    data class Frame(val positionUs: Long, val width: Int, val height: Int, val frameDurationUs: Double? = null)
    data class Exposure<T>(val output: T, val frame: Frame, val revision: Long)
    private val frames = IdentityHashMap<T, Frame?>()
    private var revision = 0L
    private var exposure: Exposure<T>? = null

    fun attach(output: T) {
        check(frames.containsKey(output) || frames.size < 2)
        frames[output] = null
    }

    fun rendered(output: T, frame: Frame) {
        if (frames.containsKey(output) && frame.positionUs >= 0 && frame.width > 0 && frame.height > 0) {
            frames[output] = frame
        }
    }

    fun ready(output: T, positionMs: Long? = null): Boolean {
        val frame = frames[output] ?: return false
        if (positionMs == null) return true
        val duration = frame.frameDurationUs ?: return false
        return positionMs >= 0 && duration.isFinite() && duration > 0 && duration <= 1_000_000 &&
            kotlin.math.abs(frame.positionUs.toDouble() - positionMs.toDouble() * 1000) <= duration
    }

    fun expose(output: T): Exposure<T>? {
        val frame = frames[output] ?: return null
        return Exposure(output, frame, ++revision).also { exposure = it }
    }

    fun presented(receipt: Exposure<T>): Boolean = exposure === receipt &&
        frames[receipt.output] == receipt.frame && receipt.revision == revision

    fun invalidate(output: T) {
        if (frames.containsKey(output)) frames[output] = null
        if (exposure?.output === output) { exposure = null; revision++ }
    }

    fun remove(output: T) {
        invalidate(output)
        frames.remove(output)
    }
}


/** The renderer thread can queue more metadata before its first-frame event
 * reaches the application looper. Preserve the first candidate for that event. */
internal class PreparedFirstFrameSlot {
    private val first = AtomicReference<PreparedSurfaceReceipts.Frame?>(null)
    fun offer(frame: PreparedSurfaceReceipts.Frame) {
        if (frame.positionUs >= 0 && frame.width > 0 && frame.height > 0) {
            first.compareAndSet(null, frame)
        }
    }
    fun snapshot(): PreparedSurfaceReceipts.Frame? = first.get()
    fun clear() { first.set(null) }
}
