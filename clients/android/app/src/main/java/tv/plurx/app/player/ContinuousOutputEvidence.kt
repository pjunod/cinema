@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.Format
import java.util.concurrent.atomic.AtomicReference

/** Actual hardware frame and sink-head observations. Subscription belongs to
 * one attachment; neither a scheduled release nor the player clock is proof. */
internal class ContinuousOutputEvidence {
    sealed interface Event {
        data class Frame(val positionUs: Long, val format: Format, val observedAtMs: Long) : Event
        data class AudioHead(val positionUs: Long) : Event
        data object VideoFreed : Event
        data object AudioSinkFlushed : Event
        data object AudioDecoderOwned : Event
        data object AudioDecoderFreed : Event
        data object AudioOutputOwned : Event
        data object AudioOutputFreed : Event
        data object VideoOwned : Event
    }
    private data class Subscription(val owner: Any, val observe: (Event) -> Unit)
    val audioOutputs = ContinuousAudioOutputs()
    private val subscription = AtomicReference<Subscription?>(null)
    fun subscribe(owner: Any, observe: (Event) -> Unit) { check(subscription.compareAndSet(null, Subscription(owner, observe))) }
    fun unsubscribe(owner: Any) {
        while (true) {
            val old = subscription.get() ?: return
            if (old.owner !== owner || subscription.compareAndSet(old, null)) return
        }
    }
    fun owner(): Any? = subscription.get()?.owner
    fun emit(event: Event, owner: Any?) {
        val current = subscription.get()
        if (current != null && current.owner === owner) current.observe(event)
    }
}

/** Playback-thread timestamp provenance survives format changes but not a
 * codec/reset epoch. A callback consumes exactly its queued frame's format. */
internal class ContinuousCodecFrames {
    data class Frame(val positionUs: Long, val format: Format, val owner: Any)
    var epoch = 0L
        private set
    private val frames = LinkedHashMap<Long, Frame>()
    fun reset(): Long { frames.clear(); epoch++; return epoch }
    fun queued(codecTimeUs: Long, offsetUs: Long, format: Format, owner: Any, skippedFlushOffsetUs: Long = 0) {
        val position = preparedItemFramePositionUs(codecTimeUs, offsetUs) ?: return
        val raw = try { Math.addExact(codecTimeUs, skippedFlushOffsetUs) } catch (_: ArithmeticException) { return }
        if (raw < 0) return
        frames[raw] = Frame(position, format, owner)
        while (frames.size > 512) frames.remove(frames.keys.first())
    }
    fun rendered(codecTimeUs: Long, observedEpoch: Long): Frame? = if (observedEpoch == epoch) frames.remove(codecTimeUs) else null
}
