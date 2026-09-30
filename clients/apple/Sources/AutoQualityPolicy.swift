import Foundation

/// Pure port of PlaybackPolicy.decideRung. No player, timer or measurement
/// adapter calls this yet. Parameters are supplied by the shared fixture in M2;
/// a future adapter must retain the same data rather than retune private values.
enum AutoQualityPolicy {
    struct Defaults: Decodable {
        let sampleMs, decisionMs, safeEstimateFactor, severeEstimateRatio: Double
        let lowRungEmergencyCount, severePeakSafetyFactor, mildHeadroom, mildSamples: Double
        let cooldownMs, upgradeHeadroom, upgradeHoldMs, upgradeRunwaySeconds: Double
        let upgradeAfterCliffMs, upgradeSpeedFloor, stallWindowMs, dwellMs: Double
        let nearEmptyRunwaySeconds, restartCostSeconds, causeMaxAgeMs, recentSampleMaxAgeMs: Double
    }

    struct Rung: Decodable {
        let height, total_kbps: Double
        let peak_kbps: Double?
        var peak: Double { (peak_kbps ?? 0) > 0 ? peak_kbps! : total_kbps }
    }

    struct Cause: Decodable {
        let kind: String?
        let ageMs: Double?
        private enum CodingKeys: String, CodingKey { case kind, ageMs }
        init(from decoder: Decoder) throws {
            let fields = try decoder.container(keyedBy: CodingKeys.self)
            kind = try fields.decodeIfPresent(String.self, forKey: .kind)
            // Number(undefined) is NaN; Number(null) is 0 in the web policy.
            ageMs = fields.contains(.ageMs) ? try fields.decodeIfPresent(Double.self, forKey: .ageMs) ?? 0 : nil
        }
    }

    struct Sample: Decodable {
        let currentHeight: Double
        let estimateKbps, recentEstimateKbps, recentEstimateAtMs, recentMediaDeliveryKbps: Double?
        let runwaySeconds, previousRunwaySeconds, recentSpeed: Double?
        let activeSupplyStall: Bool
        let supplyStalls, decodeStalls: Double
        let decodeStepConsumed: Bool
        let lastStallAtMs, lastSwitchAtMs, lastCliffAtMs: Double?
        let nowMs, mildSamples: Double
        let upgradeSinceMs, playerHeight: Double?
        let blockedHeights: [Double]?
        let causeEvidence: Cause?
    }

    struct Evidence {
        let kind: String
        let ageMs, runwaySeconds, throughputKbps: Double?
    }

    struct Decision {
        var height: Double?
        var reason: String?
        var emergency = false
        var mildSamples: Double = 0
        var upgradeSinceMs: Double?
        var action: String?
        var evidence: Evidence?
        var blockedHeights: [Double]?
    }

    static func decideRung(ladder: [Rung], sample s: Sample, defaults d: Defaults) -> Decision {
        // Last duplicate height wins, as in the web Map. The presentation
        // ceiling vetoes only upgrades; it must never strand a downgrade.
        var byHeight: [Double: Rung] = [:]
        for rung in ladder where rung.height > 0 && rung.total_kbps > 0 {
            byHeight[rung.height] = rung
        }
        let available = byHeight.values.sorted { $0.height < $1.height }
        guard !available.isEmpty else { return Decision() }
        var index = 0
        for i in available.indices.dropFirst() where
            abs(available[i].height - s.currentHeight) < abs(available[index].height - s.currentHeight) {
            index = i
        }
        let current = available[index]
        let estimate = s.estimateKbps ?? .nan
        let recent = s.recentEstimateKbps ?? .nan
        let at = s.recentEstimateAtMs ?? .nan
        let fresh = recent > 0 && at.isFinite && at <= s.nowMs
            && s.nowMs - at <= d.recentSampleMaxAgeMs ? recent : .nan
        let runway = s.runwaySeconds ?? .nan
        let previous = s.previousRunwaySeconds ?? .nan
        let speed = s.recentSpeed ?? .nan
        let draining = runway.isFinite && previous.isFinite && runway < previous - 0.25
        let nearEmpty = runway.isFinite && runway <= d.nearEmptyRunwaySeconds
        let cliff = fresh > 0 && (fresh < current.total_kbps * d.severeEstimateRatio
            || (Double(index) < d.lowRungEmergencyCount && fresh < current.total_kbps))
        let starvation = s.activeSupplyStall || nearEmpty || s.supplyStalls >= 3
        let kind = s.causeEvidence?.kind ?? "unknown"
        let rawAge = s.causeEvidence?.ageMs
        let age = rawAge.flatMap { $0.isFinite ? max(0, $0) : nil }
        let causeFresh = age == nil || age! <= d.causeMaxAgeMs
        let suppressed = causeFresh && ["producer-failed", "authority-refused", "loader-suspended",
            "delivery-refused", "control-stall-verdict"].contains(kind) ? kind : nil
        if suppressed != nil || (starvation && !cliff && kind != "capacity-shortfall" && kind != "decode-failed") {
            return Decision(height: current.height, reason: suppressed ?? "insufficient-evidence",
                action: "suppressed", evidence: Evidence(kind: suppressed ?? (causeFresh ? kind : "stale"),
                    ageMs: age, runwaySeconds: runway.isFinite ? runway : nil,
                    throughputKbps: fresh > 0 ? fresh : nil))
        }
        if causeFresh && kind == "decode-failed" && s.decodeStalls > 0 {
            if s.decodeStepConsumed || index == 0 {
                return Decision(height: current.height, reason: "decode", action: "suppressed")
            }
            return Decision(height: available[index - 1].height, reason: "decode",
                action: "switch", blockedHeights: [current.height])
        }
        if cliff && index > 0 {
            let safe = available.lastIndex { $0.peak <= fresh * d.severePeakSafetyFactor } ?? 0
            return Decision(height: available[min(index - 1, safe)].height, reason: "bandwidth cliff",
                emergency: true, action: "switch", evidence: Evidence(kind: "bandwidth-limited",
                    ageMs: s.nowMs - at, runwaySeconds: runway.isFinite ? runway : nil,
                    throughputKbps: fresh))
        }
        if cliff { return Decision(height: current.height) }
        let estimatePressure = estimate > 0 && estimate < current.total_kbps * d.mildHeadroom
        let serverPressure = causeFresh && kind == "capacity-shortfall" && speed > 0 && speed < 1 && draining
        let mild = estimatePressure || serverPressure ? s.mildSamples + 1 : 0
        let sinceSwitch = s.lastSwitchAtMs.map { s.nowMs - $0 } ?? .infinity
        let voluntary = sinceSwitch >= d.cooldownMs && sinceSwitch >= d.dwellMs
        func gain(_ target: Rung) -> Double {
            if target.height > current.height {
                return (target.total_kbps - current.total_kbps) / current.total_kbps * (d.dwellMs / 1000)
            }
            if speed > 0 && speed < 1 { return (1 / speed - 1) * (d.dwellMs / 1000) }
            if !(estimate > 0) { return 0 }
            return max(0, (current.total_kbps * d.mildHeadroom / estimate - 1) * (d.dwellMs / 1000))
        }
        if index > 0 && mild >= d.mildSamples && voluntary && gain(available[index - 1]) > d.restartCostSeconds {
            return Decision(height: available[index - 1].height,
                reason: serverPressure ? "server supply" : "bandwidth pressure")
        }
        let ceiling = (s.playerHeight ?? .infinity) > 0 ? (s.playerHeight ?? .infinity) : .infinity
        let blocked = Set(s.blockedHeights ?? [])
        let next = index + 1 < available.count ? available[index + 1] : nil
        let candidate = next.flatMap { $0.height <= ceiling && !blocked.contains($0.height) ? $0 : nil }
        var ready = false
        if let target = candidate {
            let predicted = speed > 0 ? speed * ((current.height * current.height) / (target.height * target.height)) : nil
            let encode = predicted == nil || predicted! >= d.upgradeSpeedFloor
            let stallFree = s.lastStallAtMs.map { s.nowMs - $0 >= d.stallWindowMs } ?? true
            let settled = s.lastCliffAtMs.map { s.nowMs - $0 >= d.upgradeAfterCliffMs } ?? true
            let sustained = (s.recentMediaDeliveryKbps ?? 0) > target.peak * d.upgradeHeadroom
            ready = estimate > target.total_kbps * d.upgradeHeadroom && fresh > target.peak * d.upgradeHeadroom
                && runway.isFinite && runway >= d.upgradeRunwaySeconds && (settled || sustained)
                && encode && stallFree && !estimatePressure && !serverPressure
        }
        let upgradeSince = ready ? (s.upgradeSinceMs ?? s.nowMs) : nil
        if let target = candidate, let since = upgradeSince,
            s.nowMs - since >= d.upgradeHoldMs && voluntary && gain(target) > d.restartCostSeconds {
            return Decision(height: target.height, reason: "bandwidth recovered")
        }
        return Decision(height: current.height, mildSamples: mild, upgradeSinceMs: upgradeSince)
    }
}
