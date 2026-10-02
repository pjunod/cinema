@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.common.C
import androidx.media3.common.Format
import org.junit.Assert.*
import org.junit.Test

class ContinuousOutputEvidenceTest {
    @Test fun hardwareCallbacksUseTheirQueuedFormatAndRejectEarlierEpochs() {
        val owner = Any()
        val low = Format.Builder().setWidth(1280).setHeight(720).build()
        val high = Format.Builder().setWidth(1920).setHeight(1080).build()
        val frames = ContinuousCodecFrames()
        val epoch = frames.epoch
        frames.queued(1_000_000_000, 999_000_000, low, owner)
        frames.queued(1_000_041_667, 999_000_000, high, owner)
        assertEquals(low, frames.rendered(1_000_000_000, epoch)?.format)
        assertEquals(1_041_667L, frames.rendered(1_000_041_667, epoch)?.positionUs)
        assertNull(frames.rendered(1_000_041_667, epoch))
        frames.reset()
        frames.queued(1_000_000_000, 999_000_000, high, owner)
        assertNull(frames.rendered(1_000_000_000, epoch))
        assertEquals(high, frames.rendered(1_000_000_000, frames.epoch)?.format)
        frames.queued(2_000_000, 1_000_000, low, owner, 20_000_000)
        assertEquals(1_000_000L, frames.rendered(22_000_000, frames.epoch)?.positionUs)
        frames.queued(C.TIME_UNSET, 0, low, owner)
        assertNull(frames.rendered(C.TIME_UNSET, frames.epoch))
        repeat(513) { frames.queued(it.toLong(), 0, low, owner) }
        assertNull(frames.rendered(0, frames.epoch))
        assertNotNull(frames.rendered(512, frames.epoch))
    }
    @Test fun aFailedCodecFlushCannotCreditDecoderRelease() {
        val evidence = ContinuousOutputEvidence()
        val owner = Any()
        val events = mutableListOf<ContinuousOutputEvidence.Event>()
        evidence.subscribe(owner, events::add)
        var fail = true
        val delegate = java.lang.reflect.Proxy.newProxyInstance(
            androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java.classLoader,
            arrayOf(androidx.media3.exoplayer.mediacodec.MediaCodecAdapter::class.java),
        ) { _, method, _ ->
            if (method.name == "flush" && fail) throw IllegalStateException("fixture codec flush failure")
            null
        } as androidx.media3.exoplayer.mediacodec.MediaCodecAdapter
        val adapter = ContinuousCodecAdapterFactory.observe(delegate, video = true, audio = false, evidence)
        adapter.queueInputBuffer(0, 0, 1, 0, 0)
        assertThrows(IllegalStateException::class.java) { adapter.flush() }
        assertEquals(listOf(ContinuousOutputEvidence.Event.VideoOwned), events)
        fail = false
        adapter.flush()
        assertEquals(ContinuousOutputEvidence.Event.VideoFreed, events.last())
    }
    @Test fun callbacksCannotBorrowANewAttachmentSubscription() {
        val evidence = ContinuousOutputEvidence()
        val old = Any(); val next = Any()
        val events = mutableListOf<ContinuousOutputEvidence.Event>()
        evidence.subscribe(old, events::add)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(100), old)
        evidence.unsubscribe(old)
        evidence.subscribe(next, events::add)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(200), old)
        evidence.unsubscribe(old)
        evidence.emit(ContinuousOutputEvidence.Event.AudioHead(300), next)
        assertEquals(listOf(ContinuousOutputEvidence.Event.AudioHead(100), ContinuousOutputEvidence.Event.AudioHead(300)), events)
    }
}
