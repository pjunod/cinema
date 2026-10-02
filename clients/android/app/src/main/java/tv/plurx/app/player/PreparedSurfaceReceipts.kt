package tv.plurx.app.player

import java.util.IdentityHashMap

/** Ownership of actual output frames and asynchronous compositor receipts. */
internal class PreparedSurfaceReceipts<T : Any> {
    data class Frame(val positionUs: Long, val width: Int, val height: Int)
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
        return positionMs == null || (positionMs >= 0 &&
            kotlin.math.abs(frame.positionUs / 1000 - positionMs) <= PREPARED_ALIGNMENT_SLACK_MS)
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
