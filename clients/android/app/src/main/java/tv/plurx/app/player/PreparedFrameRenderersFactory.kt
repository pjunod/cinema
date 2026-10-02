@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)
@file:Suppress("DEPRECATION")

package tv.plurx.app.player

import android.content.Context
import android.media.MediaFormat
import android.os.Handler
import android.os.Looper
import java.nio.ByteBuffer
import androidx.media3.exoplayer.mediacodec.MediaCodecAdapter
import androidx.media3.exoplayer.audio.AudioSink
import androidx.media3.exoplayer.audio.ForwardingAudioSink
import androidx.media3.exoplayer.audio.DefaultAudioSink
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.Renderer
import androidx.media3.exoplayer.mediacodec.MediaCodecSelector
import androidx.media3.exoplayer.video.MediaCodecVideoRenderer
import androidx.media3.exoplayer.video.VideoFrameMetadataListener
import androidx.media3.exoplayer.video.VideoRendererEventListener

/** Keep the pinned default renderer policy, including TV tunneling. The
 * tunneled hardware callback also supplies frame PTS to prepared outputs. */
internal class PreparedFrameRenderersFactory(context: Context, private val continuousOutput: ContinuousOutputEvidence? = null) : DefaultRenderersFactory(context) {
    override fun getCodecAdapterFactory(): MediaCodecAdapter.Factory {
        val original = super.getCodecAdapterFactory()
        return continuousOutput?.let { ContinuousCodecAdapterFactory(original, it) } ?: original
    }
    override fun buildVideoRenderers(
        context: Context, extensionRendererMode: Int, mediaCodecSelector: MediaCodecSelector,
        enableDecoderFallback: Boolean, eventHandler: Handler,
        eventListener: VideoRendererEventListener, allowedVideoJoiningTimeMs: Long,
        out: ArrayList<Renderer>,
    ) {
        // This application uses the bundled codec renderer. Preserve extension
        // selection if another construction path explicitly asks for it.
        if (extensionRendererMode != EXTENSION_RENDERER_MODE_OFF) {
            super.buildVideoRenderers(context, extensionRendererMode, mediaCodecSelector,
                enableDecoderFallback, eventHandler, eventListener, allowedVideoJoiningTimeMs, out)
            return
        }
        val builder = MediaCodecVideoRenderer.Builder(context)
            .setCodecAdapterFactory(codecAdapterFactory)
            .setMediaCodecSelector(mediaCodecSelector)
            .setAllowedJoiningTimeMs(allowedVideoJoiningTimeMs)
            .setEnableDecoderFallback(enableDecoderFallback)
            .setEventHandler(eventHandler)
            .setEventListener(eventListener)
            .setMaxDroppedFramesToNotify(50)
            .experimentalSetParseAv1SampleDependencies(true)
            .experimentalSetLateThresholdToDropDecoderInputUs(
                MediaCodecVideoRenderer.DEFAULT_LATE_THRESHOLD_TO_DROP_DECODER_INPUT_US)
            .setEnableDurationToProgressUs(false)
        out.add(PreparedFrameVideoRenderer(builder, continuousOutput))
    }
    override fun buildAudioSink(context: Context, enableFloatOutput: Boolean, enableAudioTrackPlaybackParams: Boolean): AudioSink? {
        val evidence = continuousOutput ?: return super.buildAudioSink(context, enableFloatOutput, enableAudioTrackPlaybackParams)
        val outputProvider = ContinuousAudioTrackProvider(evidence)
        val sink = DefaultAudioSink.Builder(context)
            .setEnableFloatOutput(enableFloatOutput)
            .setEnableAudioOutputPlaybackParameters(enableAudioTrackPlaybackParams)
            .setAudioTrackProvider(outputProvider)
            .build()
        return object : ForwardingAudioSink(sink) {
            private var offsetUs = C.TIME_UNSET
            private var audioOwner: Any? = null
            override fun setListener(listener: AudioSink.Listener) {
                super.setListener(object : AudioSink.Listener by listener {
                    override fun onAudioTrackReleased(config: AudioSink.AudioTrackConfig) {
                        outputProvider.released()
                        listener.onAudioTrackReleased(config)
                    }
                })
            }
            override fun setOutputStreamOffsetUs(outputStreamOffsetUs: Long) {
                super.setOutputStreamOffsetUs(outputStreamOffsetUs)
                offsetUs = outputStreamOffsetUs
                audioOwner = evidence.owner()
            }
            override fun getCurrentPositionUs(sourceEnded: Boolean): Long {
                val position = super.getCurrentPositionUs(sourceEnded)
                if (position != AudioSink.CURRENT_POSITION_NOT_SET) preparedItemFramePositionUs(position, offsetUs)?.let {
                    evidence.emit(ContinuousOutputEvidence.Event.AudioHead(it), audioOwner)
                }
                return position
            }
            override fun flush() { super.flush(); evidence.emit(ContinuousOutputEvidence.Event.AudioSinkFlushed, audioOwner) }
            override fun reset() { super.reset(); evidence.emit(ContinuousOutputEvidence.Event.AudioSinkFlushed, audioOwner) }
            override fun release() { super.release(); evidence.emit(ContinuousOutputEvidence.Event.AudioSinkFlushed, audioOwner) }
        }
    }
}

/** Media3's normal path already reports item-local PTS. Its tunneled path
 * reports codec timestamps through a different callback, before first render. */
private class PreparedFrameVideoRenderer(builder: MediaCodecVideoRenderer.Builder, private val continuousOutput: ContinuousOutputEvidence?) : MediaCodecVideoRenderer(builder) {
    private var metadataListener: VideoFrameMetadataListener? = null
    private var decodedFormat: Format? = null
    private val actualFrames = ContinuousCodecFrames()
    private var observerEpoch: Long? = null

    private fun installActualFrameObserver() {
        val codec = codec ?: return
        if (configuration.tunneling || continuousOutput?.owner() == null) return
        val observedEpoch = actualFrames.epoch
        observerEpoch = observedEpoch
        val handler = Handler(checkNotNull(Looper.myLooper()))
        codec.setOnFrameRenderedListener({ _, timestampUs, _ ->
            // Post even on newer Android to avoid calling renderer or codec
            // code while the hardware callback may hold its codec lock.
            handler.post {
                actualFrames.rendered(timestampUs, observedEpoch)?.let { frame ->
                    continuousOutput.emit(ContinuousOutputEvidence.Event.Frame(frame.positionUs, frame.format, System.currentTimeMillis()), frame.owner)
                }
            }
        }, handler)
    }

    override fun onCodecInitialized(name: String, configuration: MediaCodecAdapter.Configuration, initializedTimestampMs: Long, initializationDurationMs: Long) {
        super.onCodecInitialized(name, configuration, initializedTimestampMs, initializationDurationMs)
        actualFrames.reset()
        installActualFrameObserver()
    }
    override fun processOutputBuffer(positionUs: Long, elapsedRealtimeUs: Long, codec: MediaCodecAdapter?, buffer: ByteBuffer?,
        bufferIndex: Int, bufferFlags: Int, sampleCount: Int, bufferPresentationTimeUs: Long, isDecodeOnlyBuffer: Boolean,
        isLastBuffer: Boolean, format: Format): Boolean {
        if (observerEpoch != actualFrames.epoch) installActualFrameObserver()
        continuousOutput?.owner()?.let { actualFrames.queued(bufferPresentationTimeUs, outputStreamOffsetUs, format, it, skippedFlushOffsetUs) }
        return super.processOutputBuffer(positionUs, elapsedRealtimeUs, codec, buffer, bufferIndex, bufferFlags,
            sampleCount, bufferPresentationTimeUs, isDecodeOnlyBuffer, isLastBuffer, format)
    }
    override fun resetCodecStateForFlush() {
        super.resetCodecStateForFlush()
        actualFrames.reset()
    }
    override fun onCodecReleased(name: String) {
        super.onCodecReleased(name)
        actualFrames.reset()
    }
    override fun onPositionReset(positionUs: Long, joining: Boolean, sampleStreamIsResetToKeyFrame: Boolean) {
        super.onPositionReset(positionUs, joining, sampleStreamIsResetToKeyFrame)
        actualFrames.reset()
        installActualFrameObserver()
    }

    override fun handleMessage(messageType: Int, message: Any?) {
        if (messageType == Renderer.MSG_SET_VIDEO_FRAME_METADATA_LISTENER) {
            metadataListener = message as? VideoFrameMetadataListener
        }
        super.handleMessage(messageType, message)
    }

    override fun onOutputFormatChanged(format: Format, mediaFormat: MediaFormat?) {
        super.onOutputFormatChanged(format, mediaFormat)
        decodedFormat = format
    }

    override fun onProcessedTunneledBuffer(presentationTimeUs: Long) {
        // Resolve the format belonging to this timestamp, rather than the
        // latest format already queued to the decoder. No reflection or fork.
        updateOutputFormatForTime(presentationTimeUs)
        val itemPositionUs = preparedItemFramePositionUs(presentationTimeUs, outputStreamOffsetUs)
        val format = decodedFormat
        if (itemPositionUs != null && format != null) {
            continuousOutput?.emit(ContinuousOutputEvidence.Event.Frame(itemPositionUs, format, System.currentTimeMillis()), continuousOutput.owner())
            metadataListener?.onVideoFrameAboutToBeRendered(
                itemPositionUs, System.nanoTime(), format, codecOutputMediaFormat)
        }
        // This posts the ordinary first-render event after its PTS is retained.
        super.onProcessedTunneledBuffer(presentationTimeUs)
    }

    override fun onDisabled() {
        decodedFormat = null
        super.onDisabled()
    }
}

internal fun preparedItemFramePositionUs(codecPositionUs: Long, streamOffsetUs: Long): Long? {
    if (codecPositionUs == C.TIME_UNSET || codecPositionUs == Long.MAX_VALUE || streamOffsetUs == C.TIME_UNSET) return null
    val position = try { Math.subtractExact(codecPositionUs, streamOffsetUs) } catch (_: ArithmeticException) { return null }
    return position.takeIf { it >= 0 }
}
