@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.os.Build
import android.os.Handler
import android.os.Looper
import android.view.Surface
import android.view.SurfaceControl
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.annotation.SuppressLint
import androidx.media3.common.ForwardingPlayer
import androidx.media3.common.Player
import androidx.media3.common.VideoSize
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.analytics.AnalyticsListener
import androidx.media3.exoplayer.video.VideoFrameMetadataListener
import java.util.IdentityHashMap
import java.util.concurrent.Executor

/**
 * Two decoder outputs parented below PlayerView's SurfaceView. Only our child
 * controls are mutated. The SurfaceView control is a read-only parent.
 *
 * API 35 supplies an actual presented-transaction receipt. Older systems keep
 * the ordinary surface handoff: a committed transaction alone cannot stand in
 * for presentation. This class never promotes an unrendered decoder output.
 */
// Construction is private and the sole factory verifies the platform API.
@SuppressLint("NewApi")
internal class PreparedVideoSurfaces private constructor() {
    private class Output(val player: ExoPlayer, val control: SurfaceControl) {
        val surface = Surface(control)
        val pendingFrame = PreparedFirstFrameSlot()
        lateinit var metadata: VideoFrameMetadataListener
        var width = 0
        var height = 0
        var pixelRatio = 1f
        lateinit var observer: AnalyticsListener
    }

    private val handler = Handler(Looper.getMainLooper())
    private val executor = Executor { handler.post(it) }
    private val outputs = IdentityHashMap<ExoPlayer, Output>()
    private val receipts = PreparedSurfaceReceipts<Output>()
    private val presenters = IdentityHashMap<ExoPlayer, Player>()
    private var view: SurfaceView? = null
    private var visible: ExoPlayer? = null
    private var staged: ExoPlayer? = null
    private var epoch = 0L
    private var closed = false
    var exposurePending = false
        private set

    private val callback = object : SurfaceHolder.Callback {
        override fun surfaceCreated(holder: SurfaceHolder) {
            visible?.let(::attach)
            staged?.let(::attach)
        }
        override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
            layout()
        }
        override fun surfaceDestroyed(holder: SurfaceHolder) {
            exposurePending = false
            epoch++
            outputs.keys.toList().forEach(::remove)
        }
    }

    fun presenter(player: ExoPlayer): Player = presenters.getOrPut(player) {
        object : ForwardingPlayer(player) {
            override fun setVideoSurfaceView(surfaceView: SurfaceView?) {
                if (surfaceView == null || closed) return
                if (view !== surfaceView) {
                    view?.holder?.removeCallback(callback)
                    exposurePending = false
                    epoch++
                    outputs.keys.toList().forEach(::remove)
                    view = surfaceView
                    surfaceView.holder.addCallback(callback)
                }
                if (visible == null) visible = player
                attach(player)
                staged?.let(::attach)
            }
            // PlayerView switches its delegate for cues and track metadata.
            // It must not detach the predecessor's retained decoder output.
            override fun clearVideoSurfaceView(surfaceView: SurfaceView?) = Unit
            override fun clearVideoSurface() = Unit
        }
    }

    fun stage(player: ExoPlayer) {
        check(!closed)
        check(staged == null || staged === player)
        staged = player
        attach(player)
    }

    fun invalidate(player: ExoPlayer) {
        outputs[player]?.let { receipts.invalidate(it);it.pendingFrame.clear() }
    }

    fun ready(player: ExoPlayer, positionMs: Long? = null): Boolean {
        val output = outputs[player] ?: return false
        val host = view ?: return false
        return !closed && host.isShown && host.holder.surface.isValid && output.surface.isValid &&
            receipts.ready(output, positionMs) && output.width > 0 && output.height > 0
    }

    fun alignmentWindowMs(player: ExoPlayer): Double? =
        outputs[player]?.let(receipts::alignmentWindowMs)

    /** Visibility changes only; neither decoder receives a new Surface. */
    fun expose(player: ExoPlayer, presented: () -> Unit): Boolean {
        if (!ready(player)) return false
        val next = outputs[player] ?: return false
        val old = visible?.let(outputs::get)
        val receipt = receipts.expose(next) ?: return false
        val expectedEpoch = ++epoch
        visible = player
        exposurePending = true
        if (staged === player) staged = null
        SurfaceControl.Transaction().use { change ->
            old?.let { change.setVisibility(it.control, false) }
            change.setVisibility(next.control, true).setLayer(next.control, 1)
            change.addTransactionCompletedListener(executor) { stats ->
                val fence = stats.presentFence
                val deadline = android.os.SystemClock.elapsedRealtime() + PREPARED_OVERLAP_BOUND_MS
                val observe = object : Runnable {
                    override fun run() {
                        if (closed || epoch != expectedEpoch || visible !== player ||
                            outputs[player] !== next || !ready(player) || !receipts.presented(receipt)) {
                            fence.close()
                            return
                        }
                        // A valid present fence must signal. Polling uses a
                        // zero timeout and never blocks the application looper.
                        // On devices without fences the completed transaction
                        // callback itself is the platform's presented receipt.
                        if (!fence.isValid || fence.await(java.time.Duration.ZERO)) {
                            fence.close()
                            exposurePending = false
                            presented()
                        } else if (android.os.SystemClock.elapsedRealtime() < deadline) {
                            handler.postDelayed(this, 8)
                        } else {
                            fence.close()
                        }
                    }
                }
                observe.run()
            }
            change.apply()
        }
        return true
    }

    fun remove(player: ExoPlayer) {
        presenters.remove(player)
        if (staged === player) staged = null
        val output = outputs.remove(player) ?: return
        receipts.remove(output)
        player.removeAnalyticsListener(output.observer)
        player.clearVideoFrameMetadataListener(output.metadata)
        player.clearVideoSurface(output.surface)
        SurfaceControl.Transaction().use { change -> change.reparent(output.control, null).apply() }
        output.surface.release()
        output.control.release()
    }

    fun release() {
        if (closed) return
        closed = true
        exposurePending = false
        epoch++
        view?.holder?.removeCallback(callback)
        outputs.keys.toList().forEach(::remove)
        presenters.clear()
        view = null
        visible = null
        staged = null
    }

    private fun attach(player: ExoPlayer) {
        if (closed || outputs.containsKey(player)) return
        val host = view ?: return
        if (!host.holder.surface.isValid || !host.surfaceControl.isValid) return
        check(outputs.size < 2) { "Prepared output overlap exceeds two decoders" }
        val control = SurfaceControl.Builder()
            .setName("plurx-video-output")
            .setParent(host.surfaceControl)
            .setBufferSize(host.width.coerceAtLeast(1), host.height.coerceAtLeast(1))
            .build()
        val output = Output(player, control)
        output.width = player.videoSize.width
        output.height = player.videoSize.height
        output.pixelRatio = player.videoSize.pixelWidthHeightRatio
        receipts.attach(output)
        output.metadata = VideoFrameMetadataListener { positionUs, _, format, _ ->
            val durationUs = format.frameRate.takeIf { it.isFinite() && it > 0 }?.let { 1_000_000.0 / it }
            output.pendingFrame.offer(PreparedSurfaceReceipts.Frame(positionUs, format.width, format.height, durationUs))
        }
        output.observer = object : AnalyticsListener {
            override fun onRenderedFirstFrame(eventTime: AnalyticsListener.EventTime, target: Any, renderTimeMs: Long) {
                if (target === output.surface && outputs[player] === output) {
                    output.pendingFrame.snapshot()?.let { receipts.rendered(output, it) }
                }
            }
            override fun onVideoSizeChanged(eventTime: AnalyticsListener.EventTime, videoSize: VideoSize) {
                if (outputs[player] !== output) return
                output.width = videoSize.width
                output.height = videoSize.height
                output.pixelRatio = videoSize.pixelWidthHeightRatio
                layout()
            }
            override fun onPositionDiscontinuity(eventTime: AnalyticsListener.EventTime,
                oldPosition: Player.PositionInfo, newPosition: Player.PositionInfo, reason: Int) {
                if (reason == Player.DISCONTINUITY_REASON_SEEK && outputs[player] === output) invalidate(player)
            }
        }
        outputs[player] = output
        player.addAnalyticsListener(output.observer)
        player.setVideoFrameMetadataListener(output.metadata)
        SurfaceControl.Transaction().use { change ->
            change.setLayer(control, 1)
            if (visible === player) change.setVisibility(control, true) else change.setVisibility(control, false)
            change.apply()
        }
        player.setVideoSurface(output.surface)
        layout()
    }

    private fun layout() {
        val host = view ?: return
        if (!host.holder.surface.isValid) return
        SurfaceControl.Transaction().use { change ->
            for (output in outputs.values) {
                if (output.width <= 0 || output.height <= 0) continue
                val displayWidth = output.width * output.pixelRatio
                val scale = minOf(host.width.toFloat() / displayWidth, host.height.toFloat() / output.height)
                if (!scale.isFinite() || scale <= 0) continue
                change.setScale(output.control, scale * output.pixelRatio, scale)
                    .setPosition(output.control, (host.width - displayWidth * scale) / 2,
                        (host.height - output.height * scale) / 2)
            }
            change.apply()
        }
    }

    companion object {
        fun create(video: Boolean): PreparedVideoSurfaces? =
            if (video && Build.VERSION.SDK_INT >= 35) PreparedVideoSurfaces() else null
    }
}
