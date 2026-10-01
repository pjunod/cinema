package tv.plurx.app.player

import kotlinx.serialization.json.Json
import kotlinx.serialization.json.double
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import tv.plurx.app.data.PlaybackQuality
import tv.plurx.app.data.ReopenReason

/** Defaults come from the same source artifact the three policy runners use. */
internal fun autoQualityDefaults(json: String): AutoQualityPolicy.Defaults {
    val defaults = Json.parseToJsonElement(json).jsonObject.getValue("defaults").jsonObject
    fun value(key: String) = defaults.getValue(key).jsonPrimitive.double
    return AutoQualityPolicy.Defaults(
        value("sampleMs"), value("decisionMs"), value("safeEstimateFactor"),
        value("severeEstimateRatio"), value("lowRungEmergencyCount"), value("severePeakSafetyFactor"),
        value("mildHeadroom"), value("mildSamples"), value("cooldownMs"), value("upgradeHeadroom"),
        value("upgradeHoldMs"), value("upgradeRunwaySeconds"), value("upgradeAfterCliffMs"),
        value("upgradeSpeedFloor"), value("stallWindowMs"), value("dwellMs"),
        value("nearEmptyRunwaySeconds"), value("restartCostSeconds"),
        value("causeMaxAgeMs"), value("recentSampleMaxAgeMs"),
    )
}

/** Playback-scoped state survives controller replacement, never another film. */
internal class AutoQualityState {
    var requestedHeight: Int? = null
    private var claimOwner: Any? = null
    val inFlight: Boolean get() = claimOwner != null
    fun claim(owner: Any): Boolean {
        if (claimOwner != null) return false
        claimOwner = owner
        return true
    }
    fun release(owner: Any) { if (claimOwner === owner) claimOwner = null }
    var decodeStepConsumed = false
    val blockedHeights = mutableSetOf<Double>()
    var lastSwitchAtMs: Double? = null
    var lastCliffAtMs: Double? = null
    var lastStallAtMs: Double? = null
    var mildSamples = 0.0
    var upgradeSinceMs: Double? = null
    var previousRunwaySeconds: Double? = null
    private var eligibleAtMs: Long? = null
    var measurementFloorMs: Long = 0
        private set
    val supplyStalls = ArrayDeque<Long>()
    var decodeAtMs: Long? = null
    var holdUntilMs: Long? = null
    var refusalKind: String? = null
    var refusalAtMs: Long? = null

    fun eligible(setting: Boolean, quality: PlaybackQuality, presenting: Boolean,
                 attached: Boolean, replacing: Boolean, nowMs: Long, sampleMs: Long): Boolean {
        if (!setting || quality != PlaybackQuality.Auto || !presenting || !attached || replacing || inFlight) {
            eligibleAtMs = null
            upgradeSinceMs = null
            previousRunwaySeconds = null
            return false
        }
        val since = eligibleAtMs ?: nowMs.also { eligibleAtMs = it; measurementFloorMs = it }
        return nowMs - since >= sampleMs
    }

    fun viewerChangedQuality() {
        requestedHeight = null
        claimOwner = null
        // Manual intent is unrestricted, but returning to Auto in this same
        // playback must not forget the decoder failure or replenish its step.
        eligibleAtMs = null
        upgradeSinceMs = null
        previousRunwaySeconds = null
    }

    fun suspendMeasurements(nowMs: Long) {
        eligibleAtMs = null
        measurementFloorMs = nowMs
        mildSamples = 0.0
        upgradeSinceMs = null
        previousRunwaySeconds = null
    }
}

internal fun nativeAutoCause(
    state: AutoQualityState, nowMs: Long, maxAgeMs: Long,
    statusAgeMs: Long?, producerState: String?, recentSpeed: Double?,
): AutoQualityPolicy.Cause {
    if (state.holdUntilMs?.let { nowMs < it } == true) {
        return AutoQualityPolicy.Cause("control-stall-verdict", 0.0)
    }
    val refusalAge = state.refusalAtMs?.let { nowMs - it }
    if (refusalAge != null && refusalAge in 0..maxAgeMs) {
        return AutoQualityPolicy.Cause(state.refusalKind, refusalAge.toDouble())
    }
    if (statusAgeMs != null && statusAgeMs in 0..maxAgeMs) {
        if (producerState == "failed") return AutoQualityPolicy.Cause("producer-failed", statusAgeMs.toDouble())
        if (producerState in setOf("running", "held") && recentSpeed != null && recentSpeed > 0 && recentSpeed < 1) {
            return AutoQualityPolicy.Cause("capacity-shortfall", statusAgeMs.toDouble())
        }
    }
    val decodeAge = state.decodeAtMs?.let { nowMs - it }
    if (decodeAge != null && decodeAge in 0..maxAgeMs) return AutoQualityPolicy.Cause("decode-failed", decodeAge.toDouble())
    return AutoQualityPolicy.Cause("unknown")
}

internal fun adaptiveReopenReason(decision: AutoQualityPolicy.Decision): ReopenReason? = when {
    decision.reason == "decode" -> ReopenReason.Decode
    decision.reason == "server supply" -> ReopenReason.Encode
    decision.reason in setOf("bandwidth cliff", "bandwidth pressure", "bandwidth recovered") -> ReopenReason.Link
    else -> null
}

/** Without per-rung grade proof, an HDR change is refused, not guessed SDR-safe. */
internal fun adaptiveGradeAllowsChange(deliveredRange: String?): Boolean = deliveredRange == "sdr"
