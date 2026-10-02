@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import androidx.media3.decoder.CryptoInfo
import androidx.media3.exoplayer.mediacodec.MediaCodecAdapter

/** Observe successful codec primitives, not the renderer's finally-based
 * state reset: a failed flush cannot establish physical decoder disposal. */
internal class ContinuousCodecAdapterFactory(
    private val delegate: MediaCodecAdapter.Factory,
    private val evidence: ContinuousOutputEvidence,
) : MediaCodecAdapter.Factory {
    override fun createAdapter(configuration: MediaCodecAdapter.Configuration): MediaCodecAdapter {
        val adapter = delegate.createAdapter(configuration)
        val video = configuration.format.sampleMimeType?.startsWith("video/") == true
        val audio = configuration.format.sampleMimeType?.startsWith("audio/") == true
        return observe(adapter, video, audio, evidence)
    }
    companion object {
        fun observe(adapter: MediaCodecAdapter, video: Boolean, audio: Boolean, evidence: ContinuousOutputEvidence): MediaCodecAdapter = object : MediaCodecAdapter by adapter {
            private var owner: Any? = null
            private fun owned() {
                owner = evidence.owner()
                if (video) evidence.emit(ContinuousOutputEvidence.Event.VideoOwned, owner)
                if (audio) evidence.emit(ContinuousOutputEvidence.Event.AudioDecoderOwned, owner)
            }
            private fun freed() {
                if (video) evidence.emit(ContinuousOutputEvidence.Event.VideoFreed, owner)
                if (audio) evidence.emit(ContinuousOutputEvidence.Event.AudioDecoderFreed, owner)
            }
            override fun queueInputBuffer(index: Int, offset: Int, size: Int, presentationTimeUs: Long, flags: Int) {
                owned(); adapter.queueInputBuffer(index, offset, size, presentationTimeUs, flags)
            }
            override fun queueSecureInputBuffer(index: Int, offset: Int, info: CryptoInfo, presentationTimeUs: Long, flags: Int) {
                owned(); adapter.queueSecureInputBuffer(index, offset, info, presentationTimeUs, flags)
            }
            override fun flush() { adapter.flush(); freed() }
            override fun release() { adapter.release(); freed() }
        }
    }
}
