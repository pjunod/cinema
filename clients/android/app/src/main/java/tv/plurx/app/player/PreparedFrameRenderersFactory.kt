@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.content.Context
import android.media.MediaFormat
import android.os.Handler
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
internal class PreparedFrameRenderersFactory(context: Context) : DefaultRenderersFactory(context) {
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
        out.add(PreparedFrameVideoRenderer(builder))
    }
}

/** Media3's normal path already reports item-local PTS. Its tunneled path
 * reports codec timestamps through a different callback, before first render. */
private class PreparedFrameVideoRenderer(builder: MediaCodecVideoRenderer.Builder) : MediaCodecVideoRenderer(builder) {
    private var metadataListener: VideoFrameMetadataListener? = null
    private var decodedFormat: Format? = null

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
