package tv.plurx.app.player

import android.app.Activity
import android.content.Context
import android.hardware.display.DisplayManager
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.util.Log
import android.view.Display
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withTimeoutOrNull
import java.util.Locale
import kotlin.coroutines.resume

internal data class DisplayModeMatchResult(
    val outcome: String,
    val sourceFps: Double? = null,
    val fromHz: Float? = null,
    val toHz: Float? = null,
    val waitMs: Long = 0,
    val late: Boolean = false,
) {
    fun detail(): String = String.format(
        Locale.US,
        "outcome=%s result=%s source_fps=%.3f from_hz=%.3f to_hz=%.3f wait_ms=%d",
        if (late && outcome !in setOf("disabled", "unsupported", "no_source_rate")) "late" else outcome,
        outcome,
        sourceFps ?: 0.0,
        fromHz ?: 0f,
        toHz ?: 0f,
        waitMs,
    )
}

internal fun logDisplayModeResult(result: DisplayModeMatchResult) {
    Log.i("PlurxTelemetry", "playback_display_mode ${result.detail()}")
}

/** Owns one activity window's exact display-mode request and bounded wait. */
internal class DisplayModeMatcher(private val activity: Activity) {
    private var generation = 0L

    fun claimOwner(): Long = ++generation

    suspend fun match(
        owner: Long,
        sourceFps: Double?,
        enabled: Boolean,
        late: Boolean = false,
    ): DisplayModeMatchResult {
        if (owner != generation) return DisplayModeMatchResult("unsupported", sourceFps, late = late)
        if (sourceFps == null) return DisplayModeMatchResult("no_source_rate", late = late)
        if (!enabled) return DisplayModeMatchResult("disabled", sourceFps, late = late)
        if (!isTelevision(activity)) {
            return DisplayModeMatchResult("unsupported", sourceFps, late = late)
        }

        @Suppress("DEPRECATION")
        val display = activity.windowManager.defaultDisplay ?: return DisplayModeMatchResult(
            "unsupported", sourceFps, late = late,
        )
        val current = display.mode.toCandidate()
        val supported = display.supportedModes.map { it.toCandidate() }
        val requestedId = chooseDisplayMode(supported, current, sourceFps)
            ?: return DisplayModeMatchResult(
                "unchanged", sourceFps, current.refreshHz, current.refreshHz, late = late,
            )
        val requested = supported.firstOrNull { it.id == requestedId }
            ?: return DisplayModeMatchResult("unchanged", sourceFps, current.refreshHz, current.refreshHz, late = late)
        val manager = activity.getSystemService(Context.DISPLAY_SERVICE) as? DisplayManager
            ?: return DisplayModeMatchResult("unsupported", sourceFps, current.refreshHz, requested.refreshHz, late = late)
        val startedAt = SystemClock.elapsedRealtime()
        val matched = withTimeoutOrNull(DISPLAY_MODE_WAIT_MS) {
            suspendCancellableCoroutine { continuation ->
                val listener = object : DisplayManager.DisplayListener {
                    override fun onDisplayAdded(displayId: Int) = Unit
                    override fun onDisplayRemoved(displayId: Int) = Unit
                    override fun onDisplayChanged(displayId: Int) {
                        if (displayId == display.displayId && display.mode.modeId == requestedId && continuation.isActive) {
                            manager.unregisterDisplayListener(this)
                            continuation.resume(true)
                        }
                    }
                }
                manager.registerDisplayListener(listener, Handler(Looper.getMainLooper()))
                continuation.invokeOnCancellation { manager.unregisterDisplayListener(listener) }
                val attributes = activity.window.attributes
                attributes.preferredDisplayModeId = requestedId
                activity.window.attributes = attributes
                if (display.mode.modeId == requestedId && continuation.isActive) {
                    manager.unregisterDisplayListener(listener)
                    continuation.resume(true)
                }
            }
        } == true
        if (owner != generation) {
            return DisplayModeMatchResult(
                "unsupported",
                sourceFps,
                current.refreshHz,
                requested.refreshHz,
                (SystemClock.elapsedRealtime() - startedAt).coerceAtLeast(0),
                late,
            )
        }
        return DisplayModeMatchResult(
            outcome = if (matched) "matched" else "timeout",
            sourceFps = sourceFps,
            fromHz = current.refreshHz,
            toHz = requested.refreshHz,
            waitMs = (SystemClock.elapsedRealtime() - startedAt).coerceAtLeast(0),
            late = late,
        )
    }

    fun isOwner(owner: Long): Boolean = owner == generation

    fun reset(owner: Long) {
        if (owner != generation) return
        generation += 1
        val attributes = activity.window.attributes
        attributes.preferredDisplayModeId = 0
        activity.window.attributes = attributes
    }

    private fun Display.Mode.toCandidate() = DisplayModeCandidate(
        id = modeId,
        width = physicalWidth,
        height = physicalHeight,
        refreshHz = refreshRate,
    )

    private companion object {
        const val DISPLAY_MODE_WAIT_MS = 2_000L
    }
}
