package tv.plurx.app.player

import kotlin.math.abs
import kotlin.math.max
import kotlin.math.min

/** Pure decideRung port. Runners and the measurement adapter supply parameters
 * from the single shared JSON; the policy owns no timer, meter or setting. */
object AutoQualityPolicy {
    data class Defaults(
        val sampleMs: Double, val decisionMs: Double, val safeEstimateFactor: Double,
        val severeEstimateRatio: Double, val lowRungEmergencyCount: Double,
        val severePeakSafetyFactor: Double, val mildHeadroom: Double, val mildSamples: Double,
        val cooldownMs: Double, val upgradeHeadroom: Double, val upgradeHoldMs: Double,
        val upgradeRunwaySeconds: Double, val upgradeAfterCliffMs: Double,
        val upgradeSpeedFloor: Double, val stallWindowMs: Double, val dwellMs: Double,
        val nearEmptyRunwaySeconds: Double, val restartCostSeconds: Double,
        val causeMaxAgeMs: Double, val recentSampleMaxAgeMs: Double,
    )

    data class Rung(val height: Double, val totalKbps: Double, val peakKbps: Double? = null) {
        val peak: Double get() = peakKbps?.takeIf { it > 0 } ?: totalKbps
    }
    data class Cause(val kind: String? = null, val ageMs: Double? = null)
    data class Sample(
        val currentHeight: Double,
        val estimateKbps: Double? = null, val recentEstimateKbps: Double? = null,
        val recentEstimateAtMs: Double? = null, val recentMediaDeliveryKbps: Double? = null,
        val runwaySeconds: Double? = null, val previousRunwaySeconds: Double? = null,
        val recentSpeed: Double? = null, val activeSupplyStall: Boolean = false,
        val supplyStalls: Double = 0.0, val decodeStalls: Double = 0.0,
        val decodeStepConsumed: Boolean = false, val lastStallAtMs: Double? = null,
        val lastSwitchAtMs: Double? = null, val lastCliffAtMs: Double? = null,
        val nowMs: Double = 0.0, val mildSamples: Double = 0.0,
        val upgradeSinceMs: Double? = null, val playerHeight: Double? = null,
        val blockedHeights: Set<Double> = emptySet(), val causeEvidence: Cause? = null,
    )
    data class Evidence(val kind: String, val ageMs: Double?, val runwaySeconds: Double?, val throughputKbps: Double?)
    data class Decision(
        val height: Double? = null, val reason: String? = null,
        val emergency: Boolean = false, val mildSamples: Double = 0.0,
        val upgradeSinceMs: Double? = null, val action: String? = null,
        val evidence: Evidence? = null, val blockedHeights: List<Double>? = null,
    )

    fun decideRung(ladder: List<Rung>, s: Sample, d: Defaults): Decision {
        // Last duplicate wins; the presentation ceiling is upgrade-only.
        val available = ladder.filter { it.height > 0 && it.totalKbps > 0 }
            .associateBy { it.height }.values.sortedBy { it.height }
        if (available.isEmpty()) return Decision()
        var index = 0
        for (i in 1 until available.size) {
            if (abs(available[i].height - s.currentHeight) < abs(available[index].height - s.currentHeight)) index = i
        }
        val current = available[index]
        val estimate = s.estimateKbps ?: Double.NaN
        val recent = s.recentEstimateKbps ?: Double.NaN
        val at = s.recentEstimateAtMs ?: Double.NaN
        val fresh = if (recent > 0 && at.isFinite() && at <= s.nowMs && s.nowMs - at <= d.recentSampleMaxAgeMs)
            recent else Double.NaN
        val runway = s.runwaySeconds ?: Double.NaN
        val previous = s.previousRunwaySeconds ?: Double.NaN
        val speed = s.recentSpeed ?: Double.NaN
        val draining = runway.isFinite() && previous.isFinite() && runway < previous - 0.25
        val nearEmpty = runway.isFinite() && runway <= d.nearEmptyRunwaySeconds
        val cliff = fresh > 0 && (fresh < current.totalKbps * d.severeEstimateRatio ||
            (index < d.lowRungEmergencyCount && fresh < current.totalKbps))
        val starvation = s.activeSupplyStall || nearEmpty || s.supplyStalls >= 3
        val kind = s.causeEvidence?.kind ?: "unknown"
        val age = s.causeEvidence?.ageMs?.takeIf { it.isFinite() }?.let { max(0.0, it) }
        val causeFresh = age == null || age <= d.causeMaxAgeMs
        val suppressed = kind.takeIf { causeFresh && it in setOf("producer-failed", "authority-refused",
            "loader-suspended", "delivery-refused", "control-stall-verdict") }
        if (suppressed != null || (starvation && !cliff && kind != "capacity-shortfall" && kind != "decode-failed")) {
            return Decision(height = current.height, reason = suppressed ?: "insufficient-evidence",
                action = "suppressed", evidence = Evidence(suppressed ?: if (causeFresh) kind else "stale",
                    age, runway.takeIf { it.isFinite() }, fresh.takeIf { it > 0 }))
        }
        if (causeFresh && kind == "decode-failed" && s.decodeStalls > 0) {
            if (s.decodeStepConsumed || index == 0) {
                return Decision(height = current.height, reason = "decode", action = "suppressed")
            }
            return Decision(height = available[index - 1].height, reason = "decode", action = "switch",
                blockedHeights = listOf(current.height))
        }
        if (cliff && index > 0) {
            val safe = available.indexOfLast { it.peak <= fresh * d.severePeakSafetyFactor }.coerceAtLeast(0)
            return Decision(height = available[min(index - 1, safe)].height, reason = "bandwidth cliff",
                emergency = true, action = "switch", evidence = Evidence("bandwidth-limited", s.nowMs - at,
                    runway.takeIf { it.isFinite() }, fresh))
        }
        if (cliff) return Decision(height = current.height)
        val estimatePressure = estimate > 0 && estimate < current.totalKbps * d.mildHeadroom
        val serverPressure = causeFresh && kind == "capacity-shortfall" && speed > 0 && speed < 1 && draining
        val mild = if (estimatePressure || serverPressure) s.mildSamples + 1 else 0.0
        val sinceSwitch = s.lastSwitchAtMs?.let { s.nowMs - it } ?: Double.POSITIVE_INFINITY
        val voluntary = sinceSwitch >= d.cooldownMs && sinceSwitch >= d.dwellMs
        fun gain(target: Rung): Double {
            if (target.height > current.height) {
                return (target.totalKbps - current.totalKbps) / current.totalKbps * (d.dwellMs / 1000)
            }
            if (speed > 0 && speed < 1) return (1 / speed - 1) * (d.dwellMs / 1000)
            if (!(estimate > 0)) return 0.0
            return max(0.0, (current.totalKbps * d.mildHeadroom / estimate - 1) * (d.dwellMs / 1000))
        }
        if (index > 0 && mild >= d.mildSamples && voluntary && gain(available[index - 1]) > d.restartCostSeconds) {
            return Decision(height = available[index - 1].height,
                reason = if (serverPressure) "server supply" else "bandwidth pressure")
        }
        val ceiling = s.playerHeight?.takeIf { it > 0 } ?: Double.POSITIVE_INFINITY
        val target = available.getOrNull(index + 1)?.takeIf { it.height <= ceiling && it.height !in s.blockedHeights }
        var ready = false
        if (target != null) {
            val predicted = if (speed > 0) speed * ((current.height * current.height) / (target.height * target.height)) else null
            val encode = predicted == null || predicted >= d.upgradeSpeedFloor
            val stallFree = s.lastStallAtMs?.let { s.nowMs - it >= d.stallWindowMs } ?: true
            val settled = s.lastCliffAtMs?.let { s.nowMs - it >= d.upgradeAfterCliffMs } ?: true
            val sustained = (s.recentMediaDeliveryKbps ?: 0.0) > target.peak * d.upgradeHeadroom
            ready = estimate > target.totalKbps * d.upgradeHeadroom && fresh > target.peak * d.upgradeHeadroom &&
                runway.isFinite() && runway >= d.upgradeRunwaySeconds && (settled || sustained) &&
                encode && stallFree && !estimatePressure && !serverPressure
        }
        val since = if (ready) s.upgradeSinceMs ?: s.nowMs else null
        if (target != null && since != null && s.nowMs - since >= d.upgradeHoldMs && voluntary && gain(target) > d.restartCostSeconds) {
            return Decision(height = target.height, reason = "bandwidth recovered")
        }
        return Decision(height = current.height, mildSamples = mild, upgradeSinceMs = since)
    }
}
