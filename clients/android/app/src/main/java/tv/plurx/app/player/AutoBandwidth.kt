@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.player

import android.content.Context
import android.os.Handler
import android.os.Looper
import androidx.media3.exoplayer.upstream.BandwidthMeter
import androidx.media3.exoplayer.upstream.DefaultBandwidthMeter

/** One meter per pipeline; downloads, live and a priming successor never seed it. */
internal class AutoBandwidth(context: Context) {
    val meter = DefaultBandwidthMeter.Builder(context).build()
    var recentKbps: Double? = null
        private set
    var recentAtMs: Long? = null
        private set
    init {
        meter.addEventListener(Handler(Looper.getMainLooper()), BandwidthMeter.EventListener { elapsedMs, bytes, _ ->
            if (elapsedMs > 0 && bytes > 0) {
                recentKbps = bytes.toDouble() * 8 / elapsedMs
                recentAtMs = monotonicNowMs()
            }
        })
    }
    val estimateKbps: Double? get() = meter.bitrateEstimate.takeIf { it > 0 }?.div(1000.0)
}
