import AVKit
import Combine
import Foundation
import MediaPlayer

/// Immutable AVFoundation access-log facts; no event or item crosses back to
/// the main actor. These are advisory measurements, never delivery receipts.
struct PlayerAccessLogValues: Sendable {
    let observedBitrate: Double
    let indicatedBitrate: Double
    let numberOfStalls: Int
    let numberOfDroppedVideoFrames: Int
    let numberOfMediaRequests: Int
    let segmentsDownloadedDuration: Double
    let numberOfBytesTransferred: Int64
    let transferDuration: Double
}

struct PlayerAccessLogOwner: Equatable {
    let item: ObjectIdentifier
    let session: String?
    let lifecycle: Int
}

struct PlayerAccessLogSample {
    let values: PlayerAccessLogValues
    let sequence: Int
    let sampledAtMs: Int
}

/// Retains one exact item for a read-only AVFoundation operation on a worker.
/// The worker never mutates the item or accesses controller/UI state.
final class PlayerAccessLogItem: @unchecked Sendable {
    let item: AVPlayerItem
    init(_ item: AVPlayerItem) { self.item = item }
    func read() -> PlayerAccessLogValues? {
        guard let event = item.accessLog()?.events.last else { return nil }
        return PlayerAccessLogValues(observedBitrate: event.observedBitrate,
            indicatedBitrate: event.indicatedBitrate, numberOfStalls: event.numberOfStalls,
            numberOfDroppedVideoFrames: event.numberOfDroppedVideoFrames,
            numberOfMediaRequests: event.numberOfMediaRequests,
            segmentsDownloadedDuration: event.segmentsDownloadedDuration,
            numberOfBytesTransferred: event.numberOfBytesTransferred,
            transferDuration: event.transferDuration)
    }
}

@MainActor
final class PlayerAccessLogSampler {
    // Match the existing two-second status poll. Keep at most two poll periods
    // of real facts, and reject a native read that outlives one poll period.
    static let pollMs = 2_000
    static let maximumAgeMs = 4_000
    private let now: @Sendable () -> Int
    private var owner: PlayerAccessLogOwner?
    private var generation = 0
    private var sequence = 0
    private var nextReadMs = 0
    private var sample: PlayerAccessLogSample?
    private var reading: Task<Void, Never>?

    init(now: @escaping @Sendable () -> Int = { Int(ProcessInfo.processInfo.systemUptime * 1_000) }) {
        self.now = now
    }

    func invalidate() {
        generation &+= 1
        owner = nil
        sample = nil
        nextReadMs = 0
        // A synchronous native read cannot be interrupted. Retain its slot
        // until it returns, preventing cancellation from multiplying workers.
    }

    func value(for requested: PlayerAccessLogOwner,
               read: @escaping @Sendable () -> PlayerAccessLogValues?) -> PlayerAccessLogSample? {
        if owner != requested { invalidate(); owner = requested }
        let currentMs = now()
        if reading == nil, currentMs >= nextReadMs {
            nextReadMs = currentMs + Self.pollMs
            sequence &+= 1
            let serial = sequence, capturedGeneration = generation, clock = now
            let worker = Task.detached(priority: .utility) { (read(), clock()) }
            reading = Task { @MainActor [weak self] in
                let (values, sampledAtMs) = await worker.value
                guard let self else { return }
                self.reading = nil
                guard self.owner == requested, self.generation == capturedGeneration,
                      sampledAtMs >= currentMs, sampledAtMs - currentMs <= Self.pollMs,
                      self.now() >= sampledAtMs, self.now() - sampledAtMs <= Self.maximumAgeMs else { return }
                self.sample = values.map { PlayerAccessLogSample(values: $0, sequence: serial, sampledAtMs: sampledAtMs) }
            }
        }
        guard let sample, currentMs >= sample.sampledAtMs,
              currentMs - sample.sampledAtMs <= Self.maximumAgeMs else { return nil }
        return sample
    }
}

/// Render loss needs two real progressing, well-supplied intervals. Waiting,
/// seek, pause, counter reset and another installed item are not decode proof.
struct AutoDecodePressureEvidence {
    let elapsedMs: Int
    let progressMs: Int
    let droppedFrames: Int
}
struct AutoDecodePressureWindow {
    private var attachment: ObjectIdentifier?
    private var candidate: String?
    private var previous: (at: Int, position: Int, dropped: Int)?
    private var intervals = 0
    private var first: (at: Int, position: Int, dropped: Int)?
    private(set) var evidence: AutoDecodePressureEvidence?

    mutating func observe(attachment: ObjectIdentifier?, candidate: String?,
                          nowMs: Int, positionMs: Int, cumulativeDropped: Int?, eligible: Bool) -> Bool {
        if self.attachment != attachment || self.candidate != candidate {
            self = AutoDecodePressureWindow()
            self.attachment = attachment
            self.candidate = candidate
        }
        guard attachment != nil, candidate != nil, eligible, nowMs >= 0, positionMs >= 0,
              let dropped = cumulativeDropped, dropped >= 0 else {
            previous = nil; first = nil; evidence = nil; intervals = 0; return false
        }
        defer { previous = (nowMs, positionMs, dropped) }
        guard let old = previous, nowMs >= old.at, positionMs >= old.position, dropped >= old.dropped else {
            first = nil; evidence = nil; intervals = 0; return false
        }
        let elapsed = nowMs - old.at
        guard elapsed >= 1_000, elapsed <= 5_000,
              positionMs - old.position >= elapsed / 2, dropped - old.dropped >= 3 else {
            first = nil; evidence = nil; intervals = 0; return false
        }
        intervals = min(2, intervals + 1)
        if first == nil { first = old }
        guard intervals >= 2, let first, nowMs - first.at >= 4_000 else { return false }
        evidence = AutoDecodePressureEvidence(elapsedMs: nowMs - first.at,
            progressMs: positionMs - first.position, droppedFrames: dropped - first.dropped)
        return true
    }
}

enum AutoRecoveryCause: String { case link, encode, decode, hold, authority }
struct AutoRecoveryCauseTicket {
    let session: String
    let attachment: ObjectIdentifier
    let incumbentCandidate: String
    let proposedCandidate: String
    let cause: AutoRecoveryCause
    let observedAtMs: Int
    var linkReceipt: String? = nil
    func receiptForRequest(previous: String?, candidate: String?, cause: String?) -> String? {
        guard self.cause == .link, cause == AutoRecoveryCause.link.rawValue,
              previous == session, candidate == proposedCandidate else { return nil }
        return linkReceipt
    }
    func isCurrent(session: String?, attachment: ObjectIdentifier?, candidate: String?, proposed: String?, nowMs: Int) -> Bool {
        self.session == session && self.attachment == attachment && incumbentCandidate == candidate &&
            proposedCandidate == proposed && nowMs >= observedAtMs && nowMs - observedAtMs <= 15_000
    }
}

func autoNegativeLinkAcknowledgement(receipt: String, values: [String], status: Int,
                                     sameEndpoint: Bool, remainingMs: Int) -> Bool {
    remainingMs > 0 && status == 204 && sameEndpoint && values == [receipt] &&
        UUID(uuidString: receipt)?.uuidString.lowercased() == receipt
}

/// A decoder failure the device observed is its own evidence: the failed
/// candidate joins the local rejected set at once, with no wait on the server.
/// The server's acceptance of the sample only authorizes server-side
/// consequences, which the server verifies itself. False when this candidate
/// was already rejected, so one failure is reported once.
func rejectFailedDecoderCandidate(_ rejected: inout Set<String>, candidateId: String) -> Bool {
    rejected.insert(candidateId).inserted
}

/// What a viewer boundary owes Auto, and what it must never do.
///
/// The viewer's Play reaches AVPlayer on the press, however long the pause
/// was, and a viewer seek is executed without waiting for anything optional.
/// A long pause used to hold Play until an optional original-quality stage
/// committed at the paused frame or ran out of its borrowed budget — seconds
/// of a pressed Play doing nothing — and every seek waited behind the same
/// attempt. The original-first re-plan is still owed; it is served afterwards
/// by the ordinary Auto evaluation, as a prepared replacement the playing
/// incumbent hands off to.
enum AutoViewerBoundary {
    /// An explicit pause at least this long makes its resume a boundary.
    static let longPauseMs = 60_000
    /// The incumbent runway a prepared handoff needs before it is asked for.
    static let handoffRunwaySeconds: Double = 10

    /// Whether this resume arms the re-plan. It never decides whether Play is
    /// applied: that is unconditional.
    static func resumeArmsReplan(viewerOrigin: Bool, pauseDurationMs: Int, samePauseOwner: Bool) -> Bool {
        viewerOrigin && samePauseOwner && pauseDurationMs >= longPauseMs
    }
}

/// The one Auto re-plan a viewer boundary is owed, held until the ordinary
/// Auto evaluation can serve it. It is keyed by the captured attempt that
/// armed it, so a newer viewer action makes it stale without any timer, and it
/// is consumed by the first evaluation whose incumbent can carry a handoff.
/// Nothing waits on it.
struct AutoBoundaryReplan {
    private(set) var owner: Attempt?

    var isArmed: Bool { owner != nil }

    mutating func arm(_ owner: Attempt) { self.owner = owner }

    mutating func clear() { owner = nil }

    /// The candidate `produce` names, exactly once per armed boundary, when
    /// `runwaySeconds` can carry it. A boundary is spent only on a candidate:
    /// with none (no fresh link proof yet, every original blocked) it stays
    /// owed until it is served, superseded or cleared.
    mutating func take<Candidate>(runwaySeconds: Double?, isCurrent: (Attempt) -> Bool,
                                  produce: () -> Candidate?) -> Candidate? {
        guard let owner else { return nil }
        guard isCurrent(owner) else {
            self.owner = nil
            return nil
        }
        guard (runwaySeconds ?? 0) >= AutoViewerBoundary.handoffRunwaySeconds else { return nil }
        guard let chosen = produce() else { return nil }
        self.owner = nil
        return chosen
    }
}

/// Observational upgrade windows for one installed item and playback attempt.
/// Re-reading a completed cliff sample never renews its original timestamp.
struct AutoUpgradeEvidenceWindow {
    private var attachment: ObjectIdentifier?
    private var attempt: String?
    private var observationStartMs: Int?
    private(set) var lastStallMs: Int?
    private(set) var lastCliffMs: Int?

    mutating func bind(attachment: ObjectIdentifier?, attempt: String, nowMs: Int) {
        guard self.attachment != attachment || self.attempt != attempt else { return }
        self = AutoUpgradeEvidenceWindow()
        self.attachment = attachment
        self.attempt = attempt
        observationStartMs = attachment != nil && nowMs >= 0 ? nowMs : nil
    }

    mutating func stalled(at nowMs: Int) {
        guard attachment != nil, nowMs >= 0 else { return }
        lastStallMs = max(lastStallMs ?? nowMs, nowMs)
    }

    mutating func cliff(completedAtMs: Int, nowMs: Int) {
        guard attachment != nil, completedAtMs >= 0, nowMs >= completedAtMs,
              nowMs - completedAtMs <= 15_000 else { return }
        lastCliffMs = max(lastCliffMs ?? completedAtMs, completedAtMs)
    }

    func allowsUpgrade(nowMs: Int) -> Bool {
        guard attachment != nil, let observationStartMs else { return false }
        let quietStart = max(observationStartMs, lastStallMs ?? observationStartMs)
        return (nowMs >= quietStart && nowMs - quietStart >= 60_000)
            && (lastCliffMs.map { nowMs >= $0 && nowMs - $0 >= 90_000 } ?? true)
    }

    func allowsProposal(nowMs: Int, headroomStartedAtMs: Int?) -> Bool {
        guard let headroomStartedAtMs, nowMs >= headroomStartedAtMs,
              nowMs - headroomStartedAtMs >= 45_000 else { return false }
        return allowsUpgrade(nowMs: nowMs)
    }
}

func autoActiveProductionPressure(status: PlaybackSessionStatus?, observedAtMs: Int?, nowMs: Int,
                                  sessionId: String?, candidateId: String, runwaySeconds: Double?) -> Bool {
    guard let status, let observedAtMs, nowMs >= observedAtMs,
          let age = status.activeEncodeAgeMs, age >= 0,
          nowMs - observedAtMs <= 15_000, age <= 15_000 - (nowMs - observedAtMs) else { return false }
    return sessionId != nil && status.id == sessionId && status.activeEncodeCandidateId == candidateId &&
        status.activeEncodeSegments.map { $0 >= 2 } == true &&
        status.activeEncodeActiveMs.map { $0 >= 2_000 } == true &&
        status.activeEncodeMilliRealtime.map { $0 > 0 && $0 < 1_000 } == true &&
        runwaySeconds.map { $0 >= 0 && $0 < 10 } == true
}

func autoCompletedTransferBps(_ sample: PlayerController.AutoCompletedTransfer, nowMs: Int, maximumAgeMs: Int = 10_000) -> Double? {
    guard sample.networkLoad, !sample.fromLocalCache, sample.producerPaced == false,
          sample.ageMs(nowMs: nowMs) <= maximumAgeMs, sample.bodyBytes > 0,
          let duration = sample.bodyDurationSeconds, duration.isFinite, duration > 0 else { return nil }
    let bps = Double(sample.bodyBytes) * 8 / duration
    return bps.isFinite && bps > 0 ? bps : nil
}

/// Completed link observations belong to one installed viewer lifetime.
struct AutoTransferPressureOwner: Equatable {
    let item: ObjectIdentifier
    let attempt: String
    let viewerEpoch: Int
    let session: String
    let candidate: String
    let origin: String
}

struct AutoTransferPressureObservation {
    let severe: Bool
    let mildSamples: Int
    var urgent: Bool { severe || mildSamples >= 2 }
}

/// Count actual receipts when they arrive, rather than samples left over at a
/// timer tick. Replayed, stale and out-of-order bodies cannot reset or renew it.
struct AutoTransferPressureEvidence {
    private var owner: AutoTransferPressureOwner?
    private var receipts: [String: Int] = [:]
    private var latestCompletedAtMs: Int?
    private var lastClassifiedAtMs: Int?
    private var mildCount = 0
    private var notBeforeMs = 0

    mutating func clear(notBeforeMs: Int = 0) {
        self = AutoTransferPressureEvidence()
        self.notBeforeMs = max(0, notBeforeMs)
    }

    mutating func bind(_ requested: AutoTransferPressureOwner) {
        if owner != requested {
            let floor = notBeforeMs
            clear(notBeforeMs: floor)
            owner = requested
        }
    }

    func owns(_ requested: AutoTransferPressureOwner) -> Bool { owner == requested }

    func mildSamples(nowMs: Int) -> Int {
        guard let lastClassifiedAtMs, nowMs >= lastClassifiedAtMs,
              nowMs - lastClassifiedAtMs <= 10_000 else { return 0 }
        return mildCount
    }

    mutating func observe(_ sample: PlayerController.AutoCompletedTransfer,
                          owner requested: AutoTransferPressureOwner, canonicalOrigin: String?,
                          downsideCost: Double?, knownPeak: Bool, nowMs: Int) -> AutoTransferPressureObservation? {
        guard owner == requested, sample.installedSessionId == requested.session,
              sample.installedCandidateId == requested.candidate, canonicalOrigin == requested.origin,
              !requested.session.isEmpty, !requested.candidate.isEmpty, !requested.origin.isEmpty,
              sample.statusCode == 200, sample.completedAtMs >= notBeforeMs,
              let duration = sample.bodyDurationSeconds, duration.isFinite, duration > 0, duration <= 120,
              let link = autoCompletedTransferBps(sample, nowMs: nowMs),
              let receipt = sample.receipt, UUID(uuidString: receipt)?.uuidString.lowercased() == receipt,
              let etag = sample.etag, !etag.isEmpty,
              let url = URL(string: sample.segmentId), url.pathComponents.contains(requested.session),
              url.lastPathComponent.range(of: "^seg[0-9]+\\.(m4s|ts)$", options: .regularExpression) != nil,
              latestCompletedAtMs.map({ sample.completedAtMs >= $0 }) ?? true,
              receipts[receipt] == nil else { return nil }
        receipts = receipts.filter { nowMs >= $0.value && nowMs - $0.value <= 10_000 }
        guard receipts.count < 32 else { return nil }
        receipts[receipt] = sample.completedAtMs
        latestCompletedAtMs = sample.completedAtMs
        guard let cost = downsideCost, cost.isFinite, cost > 0 else {
            return AutoTransferPressureObservation(severe: false, mildSamples: 0)
        }
        if mildSamples(nowMs: nowMs) == 0 { mildCount = 0 }
        let severe = link < cost * 0.7
        let mild = link < cost * (knownPeak ? 1.3 : 1.0)
        mildCount = mild ? min(2, mildCount + 1) : 0
        lastClassifiedAtMs = sample.completedAtMs
        return AutoTransferPressureObservation(severe: severe, mildSamples: mildCount)
    }
}

/// Catalog exposure permits a bounded observation, not a positive cost verdict.
func autoUnknownOriginalTrial(_ candidate: QualityCandidate, outputs: [MeasuredCandidateOutput]?) -> Bool {
    candidate.hasValidIdentity && candidate.decoderCompatible && candidate.route == "remux" &&
        candidate.peakBps == nil && (outputs?.count ?? 0) <= 64 &&
        !(outputs ?? []).contains { $0.candidateId == candidate.id &&
            $0.recipeDigest == candidate.recipeDigest && $0.route == candidate.route }
}

/// Advertised immutable-plan intervals, not packet timing or a full-title peak.
struct AutoVODAdvertisedInterval {
    let startSeconds: Double
    let durationSeconds: Double
}

final class AutoVODPlaylistRedirectRefusal: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask,
                    willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}

func autoVODEmpiricalMargin(_ samples: [PlayerController.AutoCompletedTransfer],
                           intervals: [String: AutoVODAdvertisedInterval], scope: UUID,
                           sessionId: String, candidateId: String, nowMs: Int, deadlineMs: Int) -> Bool {
    guard nowMs >= 0, nowMs < deadlineMs else { return false }
    var seenNames = Set<String>()
    var seenReceipts = Set<String>()
    var seenETags = Set<String>()
    var used: [AutoVODAdvertisedInterval] = []
    var duration = 0.0
    var minimumLink = Double.infinity
    var maximumObservedCost = 0.0
    for sample in samples {
        guard sample.stageScope == scope, sample.installedSessionId == sessionId,
              sample.installedCandidateId == candidateId, sample.statusCode == 200,
              let url = URL(string: sample.segmentId), url.pathComponents.contains(sessionId),
              let interval = intervals[url.lastPathComponent],
              let advertisedMs = sample.observedMediaDurationMs,
              Double(advertisedMs) >= interval.durationSeconds * 1000 - 0.001,
              Double(advertisedMs) <= interval.durationSeconds * 1000 + 1.001,
              let receipt = sample.receipt, UUID(uuidString: receipt)?.uuidString.lowercased() == receipt,
              let etag = sample.etag, !etag.isEmpty,
              let link = autoCompletedTransferBps(sample, nowMs: nowMs, maximumAgeMs: 15_000),
              !seenNames.contains(url.lastPathComponent), !seenReceipts.contains(receipt), !seenETags.contains(etag),
              used.allSatisfy({ $0.startSeconds + $0.durationSeconds <= interval.startSeconds ||
                  interval.startSeconds + interval.durationSeconds <= $0.startSeconds }) else { continue }
        seenNames.insert(url.lastPathComponent)
        seenReceipts.insert(receipt)
        seenETags.insert(etag)
        used.append(interval)
        duration += interval.durationSeconds
        minimumLink = min(minimumLink, link)
        maximumObservedCost = max(maximumObservedCost, Double(sample.bodyBytes) * 8 / interval.durationSeconds)
    }
    return used.count >= 2 && duration >= 2 && maximumObservedCost > 0 &&
        maximumObservedCost.isFinite && minimumLink >= maximumObservedCost * 1.8
}

func autoVODAdvertisedIntervals(_ data: Data) -> [String: AutoVODAdvertisedInterval]? {
    guard !data.isEmpty, data.count <= 1_048_576,
          let text = String(data: data, encoding: .utf8) else { return nil }
    let lines = text.components(separatedBy: .newlines).filter { !$0.isEmpty }
    guard lines.first == "#EXTM3U", lines.last == "#EXT-X-ENDLIST",
          lines.contains("#EXT-X-PLAYLIST-TYPE:VOD") else { return nil }
    var intervals: [String: AutoVODAdvertisedInterval] = [:]
    var pending: Double?
    var elapsed = 0.0
    for line in lines.dropFirst() {
        if line.hasPrefix("#EXTINF:") {
            guard pending == nil, line.hasSuffix(","),
                  let duration = Double(line.dropFirst(8).dropLast()),
                  duration.isFinite, duration > 0, duration <= 120 else { return nil }
            pending = duration
        } else if line.hasPrefix("#") {
            guard pending == nil,
                  line == "#EXT-X-ENDLIST" || line == "#EXT-X-PLAYLIST-TYPE:VOD" ||
                  line == "#EXT-X-MEDIA-SEQUENCE:0" || line == "#EXT-X-MAP:URI=\"init.mp4\"" ||
                  line.hasPrefix("#EXT-X-VERSION:") || line.hasPrefix("#EXT-X-TARGETDURATION:")
            else { return nil }
        } else {
            guard let duration = pending, intervals.count < 8192,
                  line.hasPrefix("seg"), line.hasSuffix(".m4s"),
                  let index = UInt32(line.dropFirst(3).dropLast(4)),
                  line == String(format: "seg%05u.m4s", index), intervals[line] == nil,
                  elapsed + duration > elapsed, (elapsed + duration).isFinite else { return nil }
            intervals[line] = AutoVODAdvertisedInterval(startSeconds: elapsed, durationSeconds: duration)
            elapsed += duration
            pending = nil
        }
    }
    return pending == nil && !intervals.isEmpty ? intervals : nil
}

func autoOriginalTransferMarginProven(_ samples: [PlayerController.AutoCompletedTransfer], sessionId: String, nowMs: Int) -> Bool {
    var distinct: [String: PlayerController.AutoCompletedTransfer] = [:]
    for sample in samples {
        guard sample.segmentId.contains("/\(sessionId)/"),
              autoCompletedTransferBps(sample, nowMs: nowMs, maximumAgeMs: 15_000) != nil,
              let mediaDuration = sample.mediaDurationSeconds, mediaDuration.isFinite, mediaDuration > 0 else { continue }
        distinct[sample.segmentId] = sample
    }
    let usable = Array(distinct.values)
    guard usable.count >= 2, usable.reduce(0, { $0 + ($1.mediaDurationSeconds ?? 0) }) >= 2 else { return false }
    let link = usable.compactMap { autoCompletedTransferBps($0, nowMs: nowMs, maximumAgeMs: 15_000) }.min() ?? 0
    let wire = usable.map { Double($0.bodyBytes) * 8 / ($0.mediaDurationSeconds ?? .infinity) }.max() ?? .infinity
    return wire.isFinite && link >= wire * 1.8
}

func autoStagedEncodeProof(_ status: PlaybackSessionStatus?, observedAtMs: Int?, nowMs: Int,
                          sessionId: String, candidateId: String) -> Bool {
    guard let status, let observedAtMs, nowMs >= observedAtMs,
          let age = status.activeEncodeAgeMs, age >= 0 else { return false }
    return nowMs - observedAtMs <= 15_000 && age <= 15_000 - (nowMs - observedAtMs) &&
        status.id == sessionId && status.activeEncodeCandidateId == candidateId &&
        status.activeEncodeSegments.map({ $0 >= 2 }) == true &&
        status.activeEncodeActiveMs.map({ $0 >= 2_000 }) == true &&
        status.activeEncodeMilliRealtime.map({ $0 >= 1_150 }) == true
}

func autoTrialMayCommit(requestedId: String?, offeredId: String?, requestedTargetRevision: UInt64?,
                       targetRevision: UInt64?, requestedViewerEpoch: Int?, viewerEpoch: Int,
                       automatic: Bool, presenting: Bool, seeking: Bool) -> Bool {
    requestedId != nil && requestedId == offeredId && requestedTargetRevision != nil &&
        requestedTargetRevision == targetRevision && requestedViewerEpoch == viewerEpoch &&
        automatic && presenting && !seeking
}

/// Prefer one minimum-cost rescue below two full overlaps plus the reserve.
/// A metadata-ready attempt may spend its entire overlap before failing.
/// This selection budget does not bound control or queue latency; native
/// admission still measures actual loaded coverage and the original deadlines.
func autoPressureNeedsMinimumAcquisitionCost(severe: Bool, runwaySeconds: Double?,
                                            playbackRate: Double) -> Bool {
    guard severe, let runwaySeconds, runwaySeconds.isFinite, runwaySeconds >= 0,
          playbackRate.isFinite, playbackRate > 0 else { return false }
    let wallSeconds = Double(2 * PreparedReplacementBounds.overlapMs
        + PreparedCommitRendezvous.postSwapReserveMs) / 1_000
    let requiredRunway = wallSeconds * max(1, playbackRate)
    return requiredRunway.isFinite && runwaySeconds < requiredRunway
}

/// The lower rung a recovery moves to. Under link pressure the link has
/// already proved it cannot carry `current`: the largest lower rung whose cost
/// fits `linkCeiling` is the answer. When none fits, the cheapest costed lower
/// rung is the smallest deficit, provided it costs less than `current`
/// (`currentCost`); otherwise holding is no worse. An uncosted rung is never
/// chosen for link pressure. A severe cliff with insufficient loaded recovery
/// runway instead minimizes measured output cost, retaining larger geometry
/// when costs tie; native admission still measures actual acquisition readiness.
func autoRecoveryCandidate(_ candidates: [QualityCandidate], current: QualityCandidate,
                           rejected: Set<String>, linkCeiling: Double? = nil, decoderRecovery: Bool = false,
                           currentCost: Double? = nil,
                           minimumAcquisitionCost: Bool = false) -> QualityCandidate? {
    let area = { (candidate: QualityCandidate) in Int64(candidate.width) * Int64(candidate.height) }
    let lower = candidates.filter { candidate in
        candidate.hasValidIdentity && candidate.decoderCompatible && !rejected.contains(candidate.id) &&
            (decoderRecovery || candidate.route != "encode" || candidate.grade == current.grade) &&
            area(candidate) < area(current)
    }
    guard let ceiling = linkCeiling else { return lower.max { area($0) < area($1) } }
    let costed = lower.filter { ($0.peakBps ?? 0) > 0 }
    if minimumAcquisitionCost, !decoderRecovery, ceiling.isFinite, ceiling > 0,
       let currentCost, currentCost.isFinite, currentCost > 0 {
        return costed.filter { Double($0.peakBps!) < currentCost }.min {
            if $0.peakBps! != $1.peakBps! { return $0.peakBps! < $1.peakBps! }
            return area($0) > area($1)
        }
    }
    if let fitting = costed.filter({ Double($0.peakBps!) <= ceiling }).max(by: { area($0) < area($1) }) {
        return fitting
    }
    guard let cheapest = costed.min(by: { $0.peakBps! < $1.peakBps! }) else { return nil }
    return currentCost.map { Double(cheapest.peakBps!) < $0 } ?? true ? cheapest : nil
}

/// The smallest affordable rung that covers the display; when none covers it,
/// the largest affordable rung is the closest. Returning nothing there (unlike
/// the web selector) left a ladder whose rungs are all below the display unable
/// to climb back after any downgrade.
func autoPreferredDisplayCandidate(_ candidates: [QualityCandidate], neededWidth: Double, neededHeight: Double) -> QualityCandidate? {
    let area = { (candidate: QualityCandidate) in Int64(candidate.width) * Int64(candidate.height) }
    let processed = candidates.filter { $0.processedHdr10 == true && $0.hasValidIdentity &&
        $0.decoderCompatible && ($0.sustainable || $0.completeCache) }
    let preferred = processed.isEmpty ? candidates : processed
    return preferred.first { $0.route != "encode" } ?? preferred.filter {
        autoDisplayFits(width: $0.width, height: $0.height, neededWidth: neededWidth, neededHeight: neededHeight)
    }.min { area($0) < area($1) } ?? preferred.max { area($0) < area($1) }
}

func autoDisplayFits(width: Int, height: Int, neededWidth: Double, neededHeight: Double) -> Bool {
    width > 0 && height > 0 && Double(width) * 11 >= neededWidth * 10 && Double(height) * 11 >= neededHeight * 10
}

func autoDownsideCostBps(_ candidate: QualityCandidate, sample: PlayerController.AutoCompletedTransfer?,
                         sessionId: String?, nowMs: Int) -> Double? {
    if let peak = candidate.peakBps, peak > 0 { return Double(peak) }
    guard candidate.route != "encode", let sample, let sessionId,
          sample.segmentId.contains("/\(sessionId)/"), autoCompletedTransferBps(sample, nowMs: nowMs) != nil else { return nil }
    let measured = sample.mediaDurationSeconds.flatMap { duration -> Double? in
        duration.isFinite && duration > 0 ? Double(sample.bodyBytes) * 8 / duration : nil
    }
    return [candidate.averageBps.flatMap { $0 > 0 ? Double($0) : nil }, measured].compactMap { $0 }.max()
}

/// A fragment cadence proof is valid only inside the inspected interval.
/// Cap the loaded planner there before freezing a target; never move a frozen
/// anchor or borrow timing from a different media fragment.
func preparedRendezvousWithCadence(
    stagedFilmPositionMs: Int, incumbentFilmPositionMs: Int, mediaOriginMs: Int,
    playbackRate: Double, overlapRemainingMs: Int, loadedItemRanges: [ClosedRange<Double>],
    cadence: PreparedDecodedCadence?
) -> PreparedCommitRendezvous? {
    var ranges = loadedItemRanges
    if let interval = cadence?.itemInterval {
        guard interval.lowerBound.isFinite, interval.upperBound.isFinite,
              interval.lowerBound >= 0, interval.upperBound > interval.lowerBound else { return nil }
        // The cadence interval is half-open; a boundary sample belongs to the
        // next fragment. One millisecond retains the planner's integer clock.
        let ceiling = interval.upperBound - 0.001
            + Double(PreparedReplacementBounds.switchRunwayMs) / 1_000
        ranges = ranges.compactMap { range in
            let end = min(range.upperBound, ceiling)
            return end >= range.lowerBound ? range.lowerBound...end : nil
        }
    }
    guard let plan = PreparedCommitRendezvous.planLoaded(
        stagedFilmPositionMs: stagedFilmPositionMs, incumbentFilmPositionMs: incumbentFilmPositionMs,
        mediaOriginMs: mediaOriginMs, playbackRate: playbackRate,
        overlapRemainingMs: overlapRemainingMs, loadedItemRanges: ranges),
        cadence?.covers(itemPositionMs: plan.itemPositionMs) ?? true else { return nil }
    return plan
}

/// Host-clock startup requires loaded media from its earliest item clock
/// through the frozen target's reserve. Telemetry's gap tolerance is not proof.
func preparedClockStartupHasRunway(
    earliestItemMs: Double, rendezvousItemMs: Int, loadedItemRanges: [ClosedRange<Double>]
) -> Bool {
    let targetMs = Double(rendezvousItemMs)
    let requiredEndMs = targetMs + Double(PreparedReplacementBounds.switchRunwayMs)
    guard earliestItemMs.isFinite, earliestItemMs >= 0, rendezvousItemMs >= 0,
          targetMs.isFinite, requiredEndMs.isFinite, earliestItemMs <= targetMs else { return false }
    let earliestSeconds = earliestItemMs / 1_000
    let runway = PlayerController.bufferedRunwaySeconds(
        playheadSeconds: earliestSeconds, ranges: loadedItemRanges, joinTolerance: 0, gapTolerance: 0)
    return earliestSeconds + runway >= requiredEndMs / 1_000
}

/// A provisional inspection position is evidence input, never an anchor.
/// Keep it inside both real loaded media and the same physical future envelope
/// the final planner can reach after preserving its post-swap reserve.
func preparedCadenceProbeItemMs(
    stagedFilmPositionMs: Int, incumbentFilmPositionMs: Int, mediaOriginMs: Int,
    playbackRate: Double, overlapRemainingMs: Int, loadedItemRanges: [ClosedRange<Double>]
) -> Int? {
    if let plan = PreparedCommitRendezvous.planLoaded(
        stagedFilmPositionMs: stagedFilmPositionMs, incumbentFilmPositionMs: incumbentFilmPositionMs,
        mediaOriginMs: mediaOriginMs, playbackRate: playbackRate,
        overlapRemainingMs: overlapRemainingMs, loadedItemRanges: loadedItemRanges) {
        return plan.itemPositionMs
    }
    guard stagedFilmPositionMs >= 0, incumbentFilmPositionMs >= 0, mediaOriginMs >= 0,
          playbackRate.isFinite, playbackRate > 0,
          let lead = PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: overlapRemainingMs)
    else { return nil }
    let originalSeconds = Double(max(0, stagedFilmPositionMs - mediaOriginMs)) / 1_000
    guard let interval = PreparedSeekPolicy.contiguousRanges(loadedItemRanges)
        .first(where: { $0.contains(originalSeconds) }) else { return nil }
    let physicalCeiling = Double(incumbentFilmPositionMs) + min(16, playbackRate) * Double(lead)
    let loadedCeiling = Double(mediaOriginMs) + interval.upperBound * 1_000 - 1
    let filmMs = min(physicalCeiling, loadedCeiling).rounded(.down)
    guard filmMs.isFinite, filmMs >= Double(max(stagedFilmPositionMs, mediaOriginMs)),
          filmMs >= Double(incumbentFilmPositionMs), filmMs < Double(Int.max),
          let film = Int(exactly: filmMs) else { return nil }
    let item = film - mediaOriginMs
    return interval.contains(Double(item) / 1_000) ? item : nil
}

/// A temporary fetch preference is safe only while already loaded incumbent
/// media covers the unchanged physical overlap and its post-swap reserve.
/// Zero means AVFoundation's automatic policy. It may take the same bounded
/// lease as an explicit preference and is restored to automatic afterward.
struct PreparedIncumbentBufferPreference {
    let original: Double
    let hint: Double

    init?(original: Double, actualRunwaySeconds: Double, playbackRate: Double) {
        let wallSeconds = Double(PreparedReplacementBounds.overlapMs
            + PreparedCommitRendezvous.postSwapReserveMs) / 1_000
        let protectedSeconds = wallSeconds * max(1, playbackRate)
        guard original.isFinite, original >= 0, actualRunwaySeconds.isFinite,
              playbackRate.isFinite, playbackRate > 0, protectedSeconds.isFinite,
              actualRunwaySeconds >= protectedSeconds,
              original == 0 || original > protectedSeconds else { return nil }
        self.original = original
        hint = protectedSeconds
    }

    /// A later owner may have chosen another preference. Restore only the
    /// captured item's unchanged lease hint, never overwrite that new choice.
    func restorationPreference(currentHint: Double) -> Double? {
        currentHint == hint ? original : nil
    }
}

struct NowPlayingState: Equatable {
    let title: String
    let durationMs: Int
    let elapsedMs: Int
    let rate: Float
    let isPlaying: Bool
}

enum PlaybackPreparationError: LocalizedError {
    case timedOut
    case failed

    var errorDescription: String? {
        switch self {
        case .timedOut:
            return "The video took too long to prepare. Check that the Cinema server is reachable."
        case .failed:
            return "The video stream could not be prepared. Check the server and try again."
        }
    }
}

/// One AVPlayer failure forwarded to the server's bounded client-log endpoint.
/// Keeping this payload small and token-free makes failures visible in the
/// ordinary server log without retaining a media capability URL.
struct ApplePlaybackFailureLog: Encodable {
    let level = "error"
    let event = "avplayer_item_failed"
    let message: String
    let method: String
    let code: Int?
    let title: String
    let fileId: Int
    let vcodec: String?
    let detail: String
    let ua = "Apple AVPlayer"

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, code, title, detail, ua
        case fileId = "file_id"
        case vcodec
    }
}

struct AppleCandidateLinkLog: Encodable {
    let event = "candidate_link_sample"
    let message = "Completed candidate body"
    let ua = "Apple AVPlayer"
    let session_id: String
    let link_sample: Sample
    struct Sample: Encodable {
        let receipt: String
        let object_name: String
        let etag: String
        let body_bytes: Int64
        let body_duration_ms: Int
        let age_ms: Int
        let network_load = true
        let from_cache = false
        let producer_paced = false
        let cause = "link"
        let negative: Bool
        let media_duration_ms: Int?
        let presenting: Bool
        let stalled: Bool
        let runway_ms: Int
    }
}

struct AppleCandidateRecoveryLog: Encodable {
    let event = "candidate_recovery"
    let message = "Candidate recovery evidence"
    let ua = "Apple AVPlayer"
    let session_id: String
    let candidate_recovery: Sample
    struct Sample: Encodable {
        var cause = "decode"
        let event_id = UUID().uuidString.lowercased()
        let candidate_id: String
        let recipe_digest: [UInt8]
        let age_ms = 0
        let decoder_failed: Bool
        let rendered_elapsed_ms: Int
        let position_progress_ms: Int
        let dropped_frames: Int
        let runway_ms: Int
    }
}

/// One `surface_raised` / `surface_cleared` / `surface_disagreement` /
/// `surface_log_only` event, with the identities the server-side session log
/// joins on (contract §3.6). `surface_disagreement` is the one to watch: each
/// occurrence is a place where something started the player after its owner
/// stopped it.
struct ApplePlaybackSurfaceLog: Encodable {
    let level: String
    let event: String
    let surfaceClass: String?
    let source: String?
    /// The raiser's own sentence. `log_only` is one source id standing for
    /// three unrelated facts (contract §3.3 row 18), and a row that cannot say
    /// which of them happened is not a trace of anything.
    let detail: String?
    let attached: Int?
    let intent: Int?
    let by: String?
    let action: String?
    let error: String?
    let positionMs: Int?
    let actions: [String]
    let playerStopped: Bool
    let rate: Double
    let presenting: Bool
    let raisedAtMs: Int
    let method: String
    let title: String
    let fileId: Int
    let sessionId: String?
    let attempt: String?
    let ua = "Apple AVPlayer"

    enum CodingKeys: String, CodingKey {
        case level, event, source, detail, attached, intent, by, action, error, actions, method, title, ua
        case surfaceClass = "class"
        case positionMs = "position_ms"
        case playerStopped = "player_stopped"
        case rate, presenting
        case raisedAtMs = "raised_at_ms"
        case fileId = "file_id"
        case sessionId = "session_id"
        case attempt
    }
}

struct AppleMarkerPlaybackLog: Encodable {
    let level = "info"
    let event: String
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let detail: String
    let ua = "Apple AVPlayer"

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, detail, ua
        case fileId = "file_id"
    }
}

/// `attempt_stale`: a migrated continuation fence refused the work it was
/// about to do because an epoch it depends on moved while it was suspended.
/// `detail` is `PlayerController.attemptStaleDetail(fence:captured:now:)`.
struct AppleAttemptStaleLog: Encodable {
    let level = "info"
    let event = "attempt_stale"
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let detail: String
    let ua = "Apple AVPlayer"

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, detail, ua
        case fileId = "file_id"
    }
}

struct MarkerOfferLedger {
    private var offered: Set<String> = []

    mutating func shouldReport(generation: String, marker: Marker) -> Bool {
        offered.insert("\(generation):\(marker.kind):\(marker.startMs)").inserted
    }
}

/// One automatic seek per marker per playback — the web client's `m._auto`
/// flag and Android's `lastAutoSkipped`, as a value the controller resets in
/// `start`. Scoped to the playback rather than to the attached item on
/// purpose: the skip's own seek can reopen the stream and land a tolerance
/// short of the marker's end, and a per-item ledger would fire again there.
/// A viewer who seeks back into a skipped region has undone the skip and is
/// not skipped a second time.
///
/// Only the preference and `Marker.isAutoSkipEligible` decide; a preview or
/// an estimate keeps its button and is never entered here.
struct MarkerAutoSkipLedger {
    private var skipped: Set<String> = []

    /// Whether an automatic skip is *possible* for this marker — the button
    /// is withheld exactly when this is true, so it never flashes for the
    /// half-second before the seek.
    static func isAutomatic(_ marker: Marker, autoSkip: Bool) -> Bool {
        autoSkip && marker.isAutoSkipEligible
    }

    mutating func shouldSkip(_ marker: Marker, autoSkip: Bool) -> Bool {
        guard Self.isAutomatic(marker, autoSkip: autoSkip) else { return false }
        return skipped.insert("\(marker.kind):\(marker.startMs)").inserted
    }
}

/// A repeated end notification at one early media boundary is a terminal
/// playback failure even though AVPlayer reports no NSError. Give it its own
/// event so server logs do not mislabel a playlist/timestamp failure as a
/// decoder failure.
struct ApplePlaybackEarlyEndLog: Encodable, Equatable {
    let level = "error"
    let event = "avplayer_early_end"
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let vcodec: String?
    let detail: String
    let ua = "Apple AVPlayer"

    init(
        positionMs: Int,
        expectedDurationMs: Int?,
        isGrowingPlaylist: Bool,
        message: String,
        method: String,
        title: String,
        fileId: Int,
        vcodec: String?
    ) {
        self.message = message
        self.method = method
        self.title = title
        self.fileId = fileId
        self.vcodec = vcodec
        let expected = expectedDurationMs.map(String.init) ?? "unknown"
        detail = "position_ms=\(max(0, positionMs)) · expected_duration_ms=\(expected) · growing=\(isGrowingPlaylist) · outcome=terminal"
    }

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, detail, ua
        case fileId = "file_id"
        case vcodec
    }
}

/// One start-or-resume time-to-first-frame measurement. The live HLS session
/// id is carried as a typed field so the server can join this client-observed
/// wait to the producer and pacing state that existed when the first frame
/// arrived.
struct ApplePlaybackTTFFLog: Encodable {
    let level = "info"
    let event = "ttff"
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let vcodec: String?
    let ms: Int
    let height: Int?
    let encoder: String?
    let sessionId: String?
    let attempt: String
    let reason: String
    let ua = "Apple AVPlayer"

    init(
        ms: Int,
        method: String,
        title: String,
        fileId: Int,
        vcodec: String?,
        height: Int?,
        encoder: String?,
        sessionId: String?,
        attempt: String,
        reason: String,
        evidence: String = "video-frame-ready"
    ) {
        self.message = "\(evidence) after \(max(0, ms)) ms"
        self.method = method
        self.title = title
        self.fileId = fileId
        self.vcodec = vcodec
        self.ms = max(0, ms)
        self.height = height
        self.encoder = encoder
        self.sessionId = sessionId
        self.attempt = attempt
        self.reason = reason
    }

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, vcodec, ms, height, encoder
        case attempt, reason, ua
        case fileId = "file_id"
        case sessionId = "session_id"
    }
}

enum PlaybackFirstOutputEvidence {
    case unknown, audioProgress, videoFrameReady

    static func observed(hasVideoSource: Bool?, localTargetExpected: Bool, evidence: LocalVideoEvidence) -> Self {
        switch hasVideoSource {
        case false: return .audioProgress
        case true: return localTargetExpected && evidence.frameReady ? .videoFrameReady : .unknown
        case nil: return .unknown
        }
    }

    var label: String {
        switch self {
        case .unknown: return "unobserved"
        case .audioProgress: return "audio-progress"
        case .videoFrameReady: return "video-frame-ready"
        }
    }
}

/// Monotonic, one-shot observation of a local ready frame or confirmed audio progress.
struct ApplePlaybackTTFFState: Equatable {
    private var openedAt: TimeInterval?
    private var openedPositionMs = 0

    mutating func opened(
        at positionMs: Int,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) {
        openedPositionMs = max(0, positionMs)
        openedAt = observedAt
    }

    mutating func observe(
        positionMs: Int,
        playing: Bool,
        evidence: PlaybackFirstOutputEvidence,
        seekPending: Bool = false,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> Int? {
        guard let openedAt, !seekPending else { return nil }
        switch evidence {
        case .unknown: return nil
        case .audioProgress:
            guard playing, positionMs >= openedPositionMs + 250 else { return nil }
        case .videoFrameReady: break
        }
        self.openedAt = nil
        return max(0, Int(((observedAt - openedAt) * 1_000).rounded()))
    }

    /// Move only the film-position gate when the attached item starts at a
    /// keyframe before the request, or a viewer seeks before first progress.
    /// The monotonic start remains the original wait anchor.
    mutating func rebasePosition(at positionMs: Int) {
        guard openedAt != nil else { return }
        openedPositionMs = max(0, positionMs)
    }

    mutating func reset() {
        openedAt = nil
        openedPositionMs = 0
    }
}

/// A dispatched seek owns one terminal beacon, independent of startup TTFF.
/// Coalesced slider ticks never enter this state. A command awaiting its first
/// decision retains its clock and binds delivery only when an item attaches.
struct ApplePlaybackSeekLog: Encodable, Equatable {
    let level = "info"
    let event: String
    let method: String
    let fileId: Int
    let attempt: String
    let ms: Int?

    enum CodingKeys: String, CodingKey {
        case level, event, method, attempt, ms
        case fileId = "file_id"
    }
}

struct ApplePlaybackSeekMeasurement {
    private struct Pending {
        let generation: Int
        let startedAt: TimeInterval
        var method: String?
        let fileId: Int
        let attempt: String
    }
    private var pending: Pending?

    mutating func dispatched(
        generation: Int, method: String?, fileId: Int, attempt: String,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> ApplePlaybackSeekLog? {
        // Resume/recovery can dispatch the same destination again. Its clock
        // remains the original command's clock, and it still owes one result.
        guard pending?.generation != generation else { return nil }
        let previous = abandoned()
        pending = Pending(generation: generation, startedAt: observedAt,
                          method: method, fileId: fileId, attempt: attempt)
        return previous
    }

    mutating func bindDelivery(generation: Int, method: String) {
        guard pending?.generation == generation, pending?.method == nil else { return }
        pending?.method = method
    }

    mutating func presented(
        generation: Int,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> ApplePlaybackSeekLog? {
        guard let pending, pending.generation == generation,
              let method = pending.method else { return nil }
        self.pending = nil
        return ApplePlaybackSeekLog(
            event: "seek_resumed", method: method,
            fileId: pending.fileId, attempt: pending.attempt,
            ms: max(0, Int(((observedAt - pending.startedAt) * 1_000).rounded()))
        )
    }

    mutating func abandoned(generation: Int? = nil) -> ApplePlaybackSeekLog? {
        guard let pending, generation == nil || pending.generation == generation else { return nil }
        self.pending = nil
        // Before the first attachment this command has not reached a player.
        guard let method = pending.method else { return nil }
        return ApplePlaybackSeekLog(event: "seek_abandoned", method: method,
                                    fileId: pending.fileId, attempt: pending.attempt, ms: nil)
    }
}

/// Last server status the Apple client observed before a stall. Recovery can
/// supersede the session before the best-effort beacon reaches `/client-log`,
/// so the snapshot travels with the client evidence and the server replaces
/// it with a fresher live join only when that is still possible.
struct ApplePlaybackServerSnapshot: Encodable, Equatable {
    var observedAgeMs: Int?
    var recentSpeed: Double?
    var serverReadyState: String?
    var serverReadyAnchorMs: Int?
    var serverReadyEndMs: Int?
    var serverReadySeconds: Double?
    var serverNextReadyStartMs: Int?
    var serverNextReadyEndMs: Int?
    var productionAheadSeconds: Int?
    var productionTargetSeconds: Int?
    var producerState: String?
    var aheadSeconds: Int?
    var aheadBytes: Int?
    var suspended: Bool?
    var holdReason: String?
    var deliveredBps: Int?
    var deliveredIdleMs: Int?
    var httpWaitCount: Int?
    var httpWaitOldestMs: Int?
    var httpWaitSegment: Int?
    var readrate: Double?
    var suspendCount: Int?
    var progressIdleMs: Int?
    var publishedEndMs: Int?
    var fetchedEndMs: Int?
    var fetchedSegment: Int?
    var firstRetainedSegment: Int?
    var playlistShape: String?
    var lastRequest: String?
    var lastRequestIdleMs: Int?

    init(_ status: PlaybackSessionStatus, observedAgeMs: Int? = nil) {
        self.observedAgeMs = observedAgeMs
        recentSpeed = status.recentSpeed
        serverReadyState = status.serverReadyState
        serverReadyAnchorMs = status.serverReadyAnchorMs
        serverReadyEndMs = status.serverReadyEndMs
        serverReadySeconds = status.serverReadySeconds
        serverNextReadyStartMs = status.serverNextReadyStartMs
        serverNextReadyEndMs = status.serverNextReadyEndMs
        productionAheadSeconds = status.productionAheadSeconds
        productionTargetSeconds = status.productionTargetSeconds
        producerState = status.producerState
        aheadSeconds = status.aheadSeconds
        aheadBytes = status.aheadBytes
        suspended = status.suspended
        holdReason = status.holdReason
        deliveredBps = status.deliveredBps
        deliveredIdleMs = status.deliveredIdleMs
        httpWaitCount = status.httpWaitCount
        httpWaitOldestMs = status.httpWaitOldestMs
        httpWaitSegment = status.httpWaitSegment
        readrate = status.readrate
        suspendCount = status.suspendCount
        progressIdleMs = status.progressIdleMs
        publishedEndMs = status.publishedEndMs
        fetchedEndMs = status.fetchedEndMs
        fetchedSegment = status.fetchedSegment
        firstRetainedSegment = status.firstRetainedSegment
        playlistShape = status.playlistShape
        lastRequest = status.lastRequest
        lastRequestIdleMs = status.idleSeconds.map {
            let seconds = max(0, $0)
            return seconds > Int.max / 1_000 ? Int.max : seconds * 1_000
        }
    }

    enum CodingKeys: String, CodingKey {
        case observedAgeMs = "observed_age_ms"
        case recentSpeed = "recent_speed"
        case serverReadyState = "server_ready_state"
        case serverReadyAnchorMs = "server_ready_anchor_ms"
        case serverReadyEndMs = "server_ready_end_ms"
        case serverReadySeconds = "server_ready_seconds"
        case serverNextReadyStartMs = "server_next_ready_start_ms"
        case serverNextReadyEndMs = "server_next_ready_end_ms"
        case productionAheadSeconds = "production_ahead_seconds"
        case productionTargetSeconds = "production_target_seconds"
        case producerState = "producer_state"
        case aheadSeconds = "ahead_seconds"
        case aheadBytes = "ahead_bytes"
        case suspended
        case holdReason = "hold_reason"
        case deliveredBps = "delivered_bps"
        case deliveredIdleMs = "delivered_idle_ms"
        case httpWaitCount = "http_wait_count"
        case httpWaitOldestMs = "http_wait_oldest_ms"
        case httpWaitSegment = "http_wait_segment"
        case readrate
        case suspendCount = "suspend_count"
        case progressIdleMs = "progress_idle_ms"
        case publishedEndMs = "published_end_ms"
        case fetchedEndMs = "fetched_end_ms"
        case fetchedSegment = "fetched_segment"
        case firstRetainedSegment = "first_retained_segment"
        case playlistShape = "playlist_shape"
        case lastRequest = "last_request"
        case lastRequestIdleMs = "last_request_idle_ms"
    }
}

/// AVPlayer and access-log state sampled at the same point that declared the
/// clock stagnant. Runway separates an empty buffer from a decode/clock stop;
/// request and transfer counters separate a client that stopped fetching from
/// a server that had no segment to hand it.
struct ApplePlaybackDiagnosticSnapshot: Encodable, Equatable {
    var positionMs: Int?
    var runway: Double?
    var timeControlStatus: String?
    var waitingReason: String?
    var playbackBufferEmpty: Bool?
    var playbackLikelyToKeepUp: Bool?
    var playbackBufferFull: Bool?
    var mediaRequests: Int?
    var downloadedDuration: Double?
    var bytesTransferred: Int?
    var transferDuration: Double?
    var observedBitrateBps: Double?
    var indicatedBitrateBps: Double?
    var accessStalls: Int?
    var server: ApplePlaybackServerSnapshot?

    enum CodingKeys: String, CodingKey {
        case positionMs = "position_ms"
        case runway
        case timeControlStatus = "time_control_status"
        case waitingReason = "waiting_reason"
        case playbackBufferEmpty = "playback_buffer_empty"
        case playbackLikelyToKeepUp = "playback_likely_to_keep_up"
        case playbackBufferFull = "playback_buffer_full"
        case mediaRequests = "media_requests"
        case downloadedDuration = "downloaded_duration"
        case bytesTransferred = "bytes_transferred"
        case transferDuration = "transfer_duration"
        case observedBitrateBps = "observed_bitrate_bps"
        case indicatedBitrateBps = "indicated_bitrate_bps"
        case accessStalls = "access_stalls"
        case server
    }
}

/// Periodic debug-acceptance sample. Unlike a stall report this is emitted
/// while playback is healthy too, giving the rig the actual buffer runway that
/// existed before each bandwidth transition instead of an operator's guess.
struct ApplePlaybackProbeLog: Encodable {
    let level = "info"
    let event = "playback_probe"
    let message = "periodic physical-device playback sample"
    let method: String
    let title: String
    let fileId: Int
    let vcodec: String?
    let height: Int?
    let encoder: String?
    let sessionId: String?
    let attempt: String
    let snapshot: ApplePlaybackDiagnosticSnapshot
    var detail: String? = nil
    let ua = "Apple AVPlayer"

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, vcodec, height, encoder, attempt, snapshot, detail, ua
        case fileId = "file_id"
        case sessionId = "session_id"
    }
}

/// One sustained AVPlayer clock stall forwarded before the controller either
/// reconnects the same delivery or stops after an immediate repeat. The
/// server's existing `stall` vocabulary renders `ms` as `stall_ms`; position
/// and outcome stay in the bounded detail field because the endpoint has no
/// dedicated film-position column.
struct ApplePlaybackStallLog: Encodable {
    let level = "warn"
    let event = "stall"
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let vcodec: String?
    let detail: String
    let ms: Int
    let encoder: String?
    let sessionId: String?
    let attempt: String
    let reason: String
    let snapshot: ApplePlaybackDiagnosticSnapshot
    let ua = "Apple AVPlayer"

    init(
        kind: PlaybackStallKind,
        outcome: SameDeliveryStallRecoveryOutcome,
        positionMs: Int,
        durationMs: Int,
        method: String,
        title: String,
        fileId: Int,
        vcodec: String?,
        encoder: String?,
        sessionId: String?,
        attempt: String,
        snapshot: ApplePlaybackDiagnosticSnapshot
    ) {
        message = kind == .buffering
            ? "AVPlayer buffering wait triggered same-delivery recovery"
            : "AVPlayer stopped advancing and triggered same-delivery recovery"
        self.method = method
        self.title = title
        self.fileId = fileId
        self.vcodec = vcodec
        detail = "kind=\(kind.rawValue) · position_ms=\(max(0, positionMs)) · outcome=\(outcome.rawValue)"
        ms = max(0, durationMs)
        self.encoder = encoder
        self.sessionId = sessionId
        self.attempt = attempt
        reason = "stall-\(kind.rawValue)"
        self.snapshot = snapshot
    }

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, detail, ms, encoder, attempt, reason, snapshot, ua
        case fileId = "file_id"
        case sessionId = "session_id"
        case vcodec
    }
}

/// AVPlayer's own access-log counter catches shorter waits that recover before
/// the six-sample recovery ladder acts. It rides the ordinary progress cadence
/// and keeps the same bounded, token-free client-log shape as ladder beacons.
struct ApplePlaybackObservedStallLog: Encodable {
    let level = "warn"
    let event = "stall"
    let message = "AVPlayer reported a self-recovered stall"
    let method: String
    let title: String
    let fileId: Int
    let vcodec: String?
    let detail: String
    let ms: Int
    let encoder: String?
    let sessionId: String?
    let attempt: String
    let reason = "self-recovered"
    let snapshot: ApplePlaybackDiagnosticSnapshot
    let ua = "Apple AVPlayer"

    init(
        delta: Int,
        positionMs: Int,
        stagnantDurationMs: Int,
        method: String,
        title: String,
        fileId: Int,
        vcodec: String?,
        encoder: String?,
        sessionId: String?,
        attempt: String,
        snapshot: ApplePlaybackDiagnosticSnapshot
    ) {
        self.method = method
        self.title = title
        self.fileId = fileId
        self.vcodec = vcodec
        detail = "kind=access_log · position_ms=\(max(0, positionMs)) · stall_delta=\(max(1, delta)) · outcome=self_recovered"
        ms = max(0, stagnantDurationMs)
        self.encoder = encoder
        self.sessionId = sessionId
        self.attempt = attempt
        self.snapshot = snapshot
    }

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, detail, ms, encoder, attempt, reason, snapshot, ua
        case fileId = "file_id"
        case sessionId = "session_id"
        case vcodec
    }
}

/// Requests that arrive while a stream replacement is in flight are remembered
/// rather than dropped. Two replacements must never overlap — they share a
/// `playback_id`, so the newer create intentionally removes the older session
/// and can otherwise strand AVPlayer on a URL the server has just deleted — but
/// a seek or track change issued during one is real viewer intent, and dropping
/// it snapped the tvOS progress bar back to where the change had started. Last
/// writer wins, and exactly one trailing reopen runs when the change lands.
///
/// A plain value with no player and no network in it, so the policy is
/// unit-testable without either.
struct PlayerReopenQueue: Equatable {
    private(set) var pending: PlayerReopenRequest?

    var pendingMs: Int? { pending?.positionMs }

    /// The request to open right now, or `nil` when a replacement is already
    /// running and this one was queued behind it instead.
    mutating func request(
        _ positionMs: Int,
        intent: PlayerOpenIntent = .normal,
        changeInFlight: Bool
    ) -> PlayerReopenRequest? {
        let next = PlayerReopenRequest(positionMs: positionMs, intent: intent)
        guard changeInFlight else { return next }
        // Last writer wins, and the *cause* travels with the position rather
        // than surviving it. A viewer seek landing behind a stall recovery
        // deliberately drops that recovery's binding: the seek builds its own
        // session, so by the time the queued request could be issued the
        // predecessor its ticket names has already been superseded and the
        // server would refuse the create outright.
        pending = next
        return nil
    }

    /// The single trailing reopen that accumulated during the change which just
    /// finished, consumed. Deliberately *not* deduplicated against the position
    /// just opened: a queued audio, quality, or subtitle change can name the
    /// very same position and still needs the session rebuilt around it.
    mutating func takePending() -> PlayerReopenRequest? {
        defer { pending = nil }
        return pending
    }

    /// The stream is gone — the change failed, or the player stopped. Nothing
    /// queued behind it is worth reopening.
    mutating func clear() { pending = nil }
}

/// One queued or immediate stream replacement: where to resume, and why it is
/// happening. The cause is carried rather than recomputed at open time because
/// only the request that observed the stall knows which session stalled.
struct PlayerReopenRequest: Equatable {
    var positionMs: Int
    var intent: PlayerOpenIntent = .normal
}

/// A cost response describes one recipe, display and attached presentation.
/// A plan is never allowed to rename that presentation while refreshing costs.
struct AutoCostCatalogOwner: Equatable {
    var lifecycle: Int
    var viewerAction: Int
    var recipe: Int
    var file: Int
    var source: String
    var session: String
    var candidate: String
    var target: PresentationTarget

    func owns(_ current: AutoCostCatalogOwner?, sameItem: Bool) -> Bool {
        sameItem && current == self
    }
}

func autoNeedsEncodedCostRefresh(_ candidates: [QualityCandidate], outputs: [MeasuredCandidateOutput]?, current: String) -> Bool {
    guard let incumbent = candidates.first(where: { $0.hasValidIdentity && $0.id == current }) else { return false }
    return candidates.contains {
        $0.hasValidIdentity && $0.decoderCompatible && $0.route == "encode" && $0.grade == incumbent.grade
            && measuredCandidatePeak($0, outputs: outputs) == nil
    }
}

func validatedAutoCostCatalog(_ candidates: [QualityCandidate]?, outputs: [MeasuredCandidateOutput]?, current: String)
    -> (candidates: [QualityCandidate], outputs: [MeasuredCandidateOutput])? {
    guard let candidates, !candidates.isEmpty, candidates.count <= 64,
          candidates.allSatisfy({ $0.hasValidIdentity }),
          Set(candidates.map(\.id)).count == candidates.count,
          candidates.contains(where: { $0.id == current }), (outputs?.count ?? 0) <= 64 else { return nil }
    // Preserve duplicate descriptors: the existing unique-proof check refuses
    // them. Filtering never turns two conflicting proofs into one good proof.
    return (candidates, (outputs ?? []).filter { output in candidates.contains { output.matches($0) } })
}

/// Desired recipe and attached media have different lifetimes. A later seek
/// must carry an unpublished quality/audio change, not seek the old recipe.
struct PlayerRecipeRevision: Equatable {
    private(set) var desired = 0
    private(set) var attached: Int?
    var needsReopen: Bool { attached != desired }
    mutating func change() { desired &+= 1 }
    mutating func didAttach(_ revision: Int) { attached = revision }
    mutating func clear() { desired &+= 1; attached = nil }
}

/// Why a create body is being posted. Everything a viewer does — a seek, a
/// quality/audio/subtitle change, a fresh title — is `.normal`. An observed
/// presentation stall is `.sameDeliveryRepair`: it preserves the recovery
/// budgets without carrying the server ticket that lowers Auto quality.
///
/// `.stallReopen` is the bound half of the wire and **nothing in this target
/// constructs it**. A-04 deleted the `stallReopenIntent` minter that used to
/// (the repository owner decided on 2026-09-23 to revive the wire and delete
/// the Apple helper), because one untyped "stall" cause cannot carry the five
/// evidence classes the adaptive-quality design distinguishes. The case, the
/// ticket, `applyOpenIntent`'s binding, `unboundStallRetry` and the floor
/// budget are all retained and still exercised by tests, so the client that
/// A-04's build plan writes mints onto this, rather than rebuilding it. See
/// docs/clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md §7.1.
enum PlayerOpenIntent: Equatable {
    case normal
    case sameDeliveryRepair
    /// A bounded explicit-resume repair that may preserve the recipe or use
    /// the already-classified transport/HDR/compatibility route. Unlike a
    /// viewer action it inherits the resume attempt's absolute deadline.
    case resumeRepair
    case stallReopen(StallReopenTicket)
}

/// The bound half of a same-session stall reopen: the exact predecessor the
/// server reads the resolved rung from, plus the request identity that makes a
/// transport replay of this one recovery idempotent. A ticket is meant to be
/// minted once per stall, so a replayed create returns the predecessor's
/// already-persisted answer instead of stepping the ladder down a second time.
/// No production code in this target mints one today — see `PlayerOpenIntent`.
struct StallReopenTicket: Equatable {
    let previousSessionId: String
    let requestId: String
}

/// The client-side bound the server deliberately does not implement. A session
/// at or below the ladder floor is answered with the rung it is already on,
/// every time, and the server counts nothing and raises no terminal error —
/// bounding it there would need the per-playback retry state the contract
/// forbids (docs/streaming/ADAPTIVE-QUALITY.md, "Native stall-reopen server boundary").
/// Without a budget here a starved link reopens the floor rung forever.
///
/// The floor is the predecessor's own rung, never 360: a sub-360 source and a
/// starved network prior both resolve below the ladder, so this state machine
/// compares heights and never names one.
struct StallReopenBudget: Equatable {
    /// Two reopens that fail to buy a lower rung. The first is a genuine retry
    /// — a transient link may have recovered — and the second is the last one
    /// worth a viewer's patience before the ordinary stall failure is shown.
    static let floorAttemptLimit = 2

    /// Film progress that separates one starvation episode from the next. The
    /// loop this bounds is tight by construction: five seconds of playback
    /// rearms `SameDeliveryStallRecoveryState`, so a floor-locked link stalls
    /// again almost immediately. A minute of real film means the link recovered
    /// and a later stall deserves its own retries — otherwise a brief blip ten
    /// minutes in would disable automatic recovery for the rest of the title.
    static let episodeProgressMs = 60_000

    private(set) var floorAttempts = 0
    /// Film position of the last reopen that spent budget, so the distance to
    /// the next stall can be measured without a wall clock.
    private(set) var lastAttemptPositionMs: Int?

    var allowsAnotherAttempt: Bool { floorAttempts < Self.floorAttemptLimit }

    /// Called with each stall before the budget is read. A stall a full minute
    /// of film past the last unproductive reopen is a new episode, not the loop.
    mutating func observedStall(at positionMs: Int) {
        guard let lastAttemptPositionMs,
              positionMs - lastAttemptPositionMs >= Self.episodeProgressMs
        else { return }
        floorAttempts = 0
    }

    /// Record what a bound stall reopen actually resolved to. Only a strictly
    /// lower rung is evidence that stepping down is still available; a height
    /// the server could not state — `0` for a remux of an unprobed source, or
    /// absent from a server predating the field — is not that evidence and
    /// spends budget, because an unbounded retry loop is the worse failure.
    mutating func resolved(height: Int?, previousHeight: Int?, at positionMs: Int) {
        lastAttemptPositionMs = positionMs
        guard let height, height > 0,
              let previousHeight, previousHeight > 0,
              height < previousHeight
        else {
            floorAttempts += 1
            return
        }
        floorAttempts = 0
    }

    /// A viewer command, a new title, or any other open that builds its own
    /// session retires the budget: it bounds automatic recovery, not the
    /// viewer.
    mutating func reset() {
        floorAttempts = 0
        lastAttemptPositionMs = nil
    }
}

/// The film-time target of an interactive seek. AVPlayer's clock is not a
/// safe place to accumulate remote presses: a growing HLS session is replaced
/// for every seek, and while that replacement is in flight the old item's
/// local clock and the new session's film-time base briefly coexist. Keeping
/// the latest target here makes ten presses mean ten steps, regardless of
/// which item AVPlayer happens to expose between them.
struct PlayerSeekState: Equatable {
    private(set) var pendingMs: Int?
    private(set) var generation = 0
    private(set) var executedGeneration: Int?
    private(set) var requiresMediaPresentation = false
    private var lastAudioPositionMs: Int?
    private var executionSampledAt: TimeInterval?
    private var executionRate = 0.0
    private var executionActiveSeconds = 0.0
    private var plausibleProgressMs = 0.0
    private var demandSampledAt: TimeInterval?
    private var demandWasActive = false
    private var demandActiveSeconds = 0.0

    static let presentationDeadlineSeconds: TimeInterval = 8

    /// A target still coalescing or awaiting execution suppresses recovery;
    /// an executed target waiting for presentation is itself recoverable work.
    var allowsStallRecovery: Bool {
        pendingMs == nil || executedGeneration == generation
    }

    mutating func absolute(_ requestedMs: Int, durationMs: Int) -> (target: Int, generation: Int) {
        generation &+= 1
        let target = Self.clamp(requestedMs, durationMs: durationMs)
        pendingMs = target
        requiresMediaPresentation = true
        executedGeneration = nil
        lastAudioPositionMs = nil
        resetPresentationClock()
        return (target, generation)
    }

    /// Changing an in-place selection cannot present an outstanding seek.
    /// Carry that obligation into the new command even when its target is
    /// unchanged; only a selection on an already presented item can settle
    /// when the subtitle mutation completes.
    mutating func selection(
        at observedMs: Int,
        durationMs: Int,
        inPlace: Bool
    ) -> (target: Int, generation: Int) {
        let requiresMedia = (pendingMs != nil && requiresMediaPresentation) || !inPlace
        let destination = absolute(pendingMs ?? observedMs, durationMs: durationMs)
        requiresMediaPresentation = requiresMedia
        return destination
    }

    mutating func relative(
        by deltaMs: Int,
        observedMs: Int,
        durationMs: Int
    ) -> (target: Int, generation: Int) {
        absolute((pendingMs ?? observedMs) + deltaMs, durationMs: durationMs)
    }

    /// A completed seek/open is only execution. It is not presentation: the
    /// transport can complete while the old frame remains on screen.
    @discardableResult
    mutating func markExecuted(
        generation expected: Int,
        targetMs: Int,
        at now: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> Bool {
        guard expected == generation, pendingMs == targetMs else { return false }
        executedGeneration = expected
        lastAudioPositionMs = nil
        resetPresentationClock()
        executionSampledAt = now
        demandSampledAt = now
        return true
    }

    /// Integrate the observed active clock between samples. A delayed first
    /// callback can already be a second into healthy playback; a paused clock
    /// earns no forward allowance. The allowance never grows beyond the
    /// execution's eight active seconds; a pause does not spend that window.
    mutating func observePlaybackClock(
        at now: TimeInterval,
        rate: Double,
        isPlaying: Bool,
        generation expected: Int
    ) {
        guard expected == generation,
              executedGeneration == expected,
              let sampledAt = executionSampledAt,
              now.isFinite
        else { return }
        if executionRate > 0 {
            let active = min(max(0, now - sampledAt),
                             max(0, Self.presentationDeadlineSeconds - executionActiveSeconds))
            plausibleProgressMs += active * executionRate * 1_000
            executionActiveSeconds += active
        }
        executionSampledAt = max(now, sampledAt)
        executionRate = isPlaying && rate.isFinite ? max(0, rate) : 0
    }

    /// A wrong-position clock may keep advancing, so destination timeout is
    /// independent of media progress. Only time when presentation is wanted
    /// and observable spends this budget, not a viewer pause or background.
    mutating func observePresentationDemand(
        at now: TimeInterval,
        eligible: Bool,
        generation expected: Int
    ) -> Bool {
        guard expected == generation, executedGeneration == expected,
              let sampledAt = demandSampledAt, now.isFinite
        else { return false }
        if demandWasActive { demandActiveSeconds += max(0, now - sampledAt) }
        demandSampledAt = max(now, sampledAt)
        demandWasActive = eligible
        return eligible && demandActiveSeconds >= Self.presentationDeadlineSeconds
    }

    private func isPlausibleLanding(_ positionMs: Int, targetMs: Int) -> Bool {
        let delta = Double(positionMs) - Double(targetMs)
        return delta >= -250 && delta <= plausibleProgressMs + 250
    }

    private mutating func resetPresentationClock() {
        executionSampledAt = nil
        executionRate = 0
        executionActiveSeconds = 0
        plausibleProgressMs = 0
        demandSampledAt = nil
        demandWasActive = false
        demandActiveSeconds = 0
    }

    /// Only a frame from the latest executed generation, at its destination,
    /// can retire the optimistic target.
    @discardableResult
    mutating func presentedVideo(positionMs: Int, generation expected: Int) -> Bool {
        guard expected == generation,
              executedGeneration == expected,
              let target = pendingMs,
              isPlausibleLanding(positionMs, targetMs: target)
        else { return false }
        pendingMs = nil
        executedGeneration = nil
        lastAudioPositionMs = nil
        return true
    }

    /// Audio-only equivalent: the active clock must move after execution, not
    /// merely jump to the target when AVPlayer accepts the seek.
    @discardableResult
    mutating func presentedAudio(positionMs: Int, generation expected: Int) -> Bool {
        guard expected == generation,
              executedGeneration == expected,
              let target = pendingMs
        else { return false }
        guard let previous = lastAudioPositionMs else {
            guard isPlausibleLanding(positionMs, targetMs: target) else { return false }
            lastAudioPositionMs = positionMs
            return false
        }
        guard positionMs > previous,
              isPlausibleLanding(positionMs, targetMs: target)
        else { return false }
        pendingMs = nil
        executedGeneration = nil
        lastAudioPositionMs = nil
        return true
    }

    /// An in-place selection has no successor item or frame generation. Its
    /// completed AVPlayer/overlay mutation is the presentation boundary.
    @discardableResult
    mutating func presentedInPlace(generation expected: Int, targetMs: Int) -> Bool {
        guard expected == generation,
              executedGeneration == expected,
              !requiresMediaPresentation,
              pendingMs == targetMs
        else { return false }
        pendingMs = nil
        executedGeneration = nil
        lastAudioPositionMs = nil
        return true
    }

    /// Also advances the generation: a teardown must invalidate every seek
    /// still sleeping in its coalescing pause or awaiting AVPlayer, or a
    /// stop()-then-start() boundary (autoplay-next) lets a stale task fire
    /// into the next playback with the previous film's target.
    mutating func clear() {
        pendingMs = nil
        executedGeneration = nil
        requiresMediaPresentation = false
        lastAudioPositionMs = nil
        resetPresentationClock()
        generation &+= 1
    }

    private static func clamp(_ requestedMs: Int, durationMs: Int) -> Int {
        let upper = durationMs > 0 ? max(0, durationMs - 2_000) : Int.max
        return min(max(0, requestedMs), upper)
    }
}

/// How an interactive seek reaches its target. `native` moves the current
/// item's own clock — instant, no server round trip; `reopen` replaces the
/// server session at the film position, which is the only way to reach media
/// the growing playlist has not published (or has already pruned).
enum PlayerSeekRoute: Equatable {
    case native(itemMs: Int)
    case reopen
}

enum PlaybackStallAction: Equatable {
    case none
    case nudge
    case reopen
}

enum PlaybackStallKind: String, Equatable {
    case silent
    case buffering
    /// The server's own delivery clock said this player stopped fetching
    /// published media (`delivered_idle_ms` grew while `published - fetched`
    /// stayed deep). Detected from the 2-second status poll, so it fires even
    /// when AVPlayer's `timeControlStatus` flaps or lies — the wedge observed
    /// on tvOS 2160p copy-HLS sessions that froze without ever tripping the
    /// position-clock ladder.
    case delivery

    var terminalState: PlaybackStallTerminalState {
        switch self {
        case .silent:
            return PlaybackStallTerminalState(
                message: "Playback stopped responding after retrying the current stream."
            )
        case .buffering:
            return PlaybackStallTerminalState(
                message: "Playback could not resume after repeated buffering. Check the connection and try again."
            )
        case .delivery:
            return PlaybackStallTerminalState(
                message: "Playback stopped fetching media after retrying the current stream. Check the connection and try again."
            )
        }
    }
}

struct PlaybackStallEvent: Equatable {
    let kind: PlaybackStallKind
    let action: PlaybackStallAction
    let positionMs: Int
    let durationMs: Int
}

/// What a spent stall ladder tells the viewer. The transport half of it used
/// to live here as two constant `false`s the two call sites copied into
/// `isPlaying` and `wantsPlayback`; both now go through
/// `stopForBlockingSurface(revokingPlaybackIntent:)`, which is where stopping
/// a player belongs, and what is left is the sentence.
struct PlaybackStallTerminalState: Equatable {
    let message: String
}

enum SameDeliveryStallRecoveryOutcome: String, Equatable {
    case reopen
    case terminal
    /// The server decided this stall. Kept distinct from `terminal` and
    /// `reopen` because "the client stopped after its own budget ran out" and
    /// "the server said this recipe is finished" are different events, and a
    /// log that spells them the same way cannot tell whether the control
    /// plane is doing anything.
    case serverTerminal = "server_terminal"
    case serverHold = "server_hold"
    case serverRetryResource = "server_retry_resource"
}

enum SameDeliveryStallRecoveryDecision: Equatable {
    case reopen
    case stop(PlaybackStallTerminalState)

    var outcome: SameDeliveryStallRecoveryOutcome {
        switch self {
        case .reopen: return .reopen
        case .stop: return .terminal
        }
    }
}

/// The same stream gets one reconnect. Five seconds of later film-clock
/// progress resets this state in the controller, so a new interruption may
/// recover while an immediate repeat stops instead of looping.
struct SameDeliveryStallRecoveryState: Equatable {
    private(set) var attempted = false

    mutating func next(for kind: PlaybackStallKind) -> SameDeliveryStallRecoveryDecision {
        guard !attempted else { return .stop(kind.terminalState) }
        attempted = true
        return .reopen
    }

    mutating func reset() { attempted = false }
}

/// Server-truth wedge detector, fed by the 2-second session-status poll.
///
/// The position-clock ladder can only see what AVPlayer admits. The failure
/// this catches is the one where AVPlayer admits nothing: it keeps reloading
/// the playlist, requests no media, and reports whatever `timeControlStatus`
/// it likes — observed live on tvOS with 250 s of published, fetchable media
/// waiting. The server already measures that wedge precisely
/// (`delivered_idle_ms` since the last completed media delivery, and how much
/// published media the client has not fetched), and this client already polls
/// those numbers every two seconds.
///
/// **Server evidence alone is not enough, and assuming it was shipped a
/// regression.** A healthy player with a full forward buffer looks identical
/// from the server's side: `preferredForwardBufferDuration` is 60 s, so once
/// AVPlayer is topped up it stops fetching for a long stretch, while the
/// producer runs ~2× realtime and parks at the 180 s ahead-window cap. Build
/// 63 fired on exactly that state and killed a healthy 2160p session every
/// ~2.4 minutes (`stall:stall-delivery ms≈19000` with `ahead_seconds` 150-180
/// and `recent_speed` ≈ 2.0, each reopen reaching first frame in under a
/// second). The discriminator is the film clock: **a player whose position is
/// advancing is not starving, whatever the delivery meter says.** Buffered
/// runway is the corroborating guard — media already in hand means the next
/// fetch is a top-up, not a rescue.
///
/// Two consecutive qualifying polls fire one recovery through the same
/// bounded same-delivery ladder as every other stall; any ineligible or
/// healthy sample resets the confirmation count.
struct DeliveryStarvationDetector: Equatable {
    /// A starved steady-state player completes a segment fetch every segment
    /// duration (~6-10 s here). Sixteen seconds of completed-delivery silence
    /// is beyond any healthy cadence *for a client that has nothing buffered*.
    static let deliveredIdleThresholdMs = 16_000
    /// Require a real backlog, so playing out the tail of a finished stream
    /// (published == fetched) can never qualify. Note this is nearly always
    /// true mid-film — the ahead window is deliberately deep — so it excludes
    /// the end of a stream and nothing else.
    static let pendingMediaThresholdMs = 10_000
    /// The film clock must be stuck too. Samples are 2 s apart and ordinary
    /// playback advances ~2000 ms between them, so this only tolerates
    /// rounding.
    static let positionToleranceMs = 250
    /// A client holding more than this much buffered media ahead of the
    /// playhead is topping up, not starving. Well under the 60 s forward
    /// buffer preference so an ordinary top-up cycle can never qualify.
    static let runwayCeilingSeconds = 10.0
    static let confirmationsRequired = 2

    private(set) var confirmations = 0
    private var lastPositionMs: Int?

    mutating func observe(
        deliveredIdleMs: Int?,
        publishedEndMs: Int?,
        fetchedEndMs: Int?,
        positionMs: Int,
        runwaySeconds: Double?,
        eligible: Bool
    ) -> Bool {
        let previousPositionMs = lastPositionMs
        lastPositionMs = positionMs
        // An unknown runway (no item, no loaded ranges) is not evidence of
        // health — fall through to the position check, which is.
        let starvedOfBuffer = (runwaySeconds ?? 0) <= Self.runwayCeilingSeconds
        let clockStuck = previousPositionMs.map {
            abs(positionMs - $0) <= Self.positionToleranceMs
        } ?? false
        guard eligible,
              clockStuck,
              starvedOfBuffer,
              let deliveredIdleMs, deliveredIdleMs >= Self.deliveredIdleThresholdMs,
              let publishedEndMs,
              let fetchedEndMs,
              publishedEndMs - fetchedEndMs >= Self.pendingMediaThresholdMs
        else {
            confirmations = 0
            return false
        }
        confirmations += 1
        guard confirmations >= Self.confirmationsRequired else { return false }
        confirmations = 0
        return true
    }

    mutating func reset() {
        confirmations = 0
        lastPositionMs = nil
    }
}

/// Global brake on automatic session replacements. Each recovery path is
/// individually bounded, but their budgets reset on different evidence, and
/// the early-end guard keys on a repeated *position* — a reopen that lands a
/// few seconds off each time evades it forever. The observed failure: nine
/// sessions opened in sixteen seconds, none reaching first frame. Whatever
/// the loop, the fourth automatic reopen inside a rolling minute stops
/// playback with the visible failure screen instead.
struct RecoveryReopenBudget: Equatable {
    static let windowSeconds: TimeInterval = 60
    static let maxAutomaticReopens = 3

    private(set) var reopenTimes: [TimeInterval] = []

    mutating func admit(
        at now: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> Bool {
        reopenTimes.removeAll { now - $0 > Self.windowSeconds }
        guard reopenTimes.count < Self.maxAutomaticReopens else { return false }
        reopenTimes.append(now)
        return true
    }

    mutating func reset() { reopenTimes.removeAll() }
}

/// Per-AVPlayerItem establishment gate. Every replacement begins unarmed and
/// has to advance five seconds on its own clock before buffering recovery may
/// treat a wait as a mid-playback interruption.
struct PlayerAttachmentRecoveryState: Equatable {
    private(set) var attachedAtPositionMs = 0
    private(set) var establishedPlayback = false

    mutating func opened(at positionMs: Int) {
        attachedAtPositionMs = max(0, positionMs)
        establishedPlayback = false
    }

    mutating func observe(positionMs: Int, playing: Bool) {
        guard playing,
              positionMs >= attachedAtPositionMs + 5_000
        else { return }
        establishedPlayback = true
    }
}

/// Detect sustained clock progress without a ready frame on the current visible
/// local layer. Dimensions are metadata, not decoded-frame evidence.
struct BlackFrameWatchdog {
    private weak var item: AVPlayerItem?
    private(set) var lastPositionMs: Int?
    private(set) var blackMs = 0
    private(set) var presentedVideo = false
    private(set) var fired = false

    mutating func opened() {
        item = nil
        lastPositionMs = nil
        blackMs = 0
        presentedVideo = false
        fired = false
    }

    mutating func bind(_ currentItem: AVPlayerItem?) {
        guard let currentItem else { opened(); return }
        guard item !== currentItem else { return }
        opened()
        item = currentItem
    }

    /// A stale task can retire its own trigger, never a replacement item's.
    mutating func cancelRecovery(for currentItem: AVPlayerItem) {
        guard item === currentItem else { return }
        fired = false
        lastPositionMs = nil
        blackMs = 0
    }

    /// Cancellation must rearm the same unready item, not permanently latch it.
    mutating func recoveryIsEligible(for currentItem: AVPlayerItem, frameReady: Bool, eligible: Bool) -> Bool {
        guard item === currentItem else { return false }
        if frameReady { presentedVideo = true }
        guard eligible, !presentedVideo else {
            cancelRecovery(for: currentItem)
            return false
        }
        return fired
    }

    @MainActor
    mutating func observe(
        item currentItem: AVPlayerItem,
        positionMs: Int,
        frameReady: Bool,
        localTargetExpected: Bool,
        hasVideoSource: Bool,
        playing: Bool
    ) -> Bool {
        bind(currentItem)
        if frameReady {
            presentedVideo = true
            lastPositionMs = nil
            blackMs = 0
            return false
        }
        guard !fired, !presentedVideo else { return false }
        guard hasVideoSource, playing, localTargetExpected else {
            lastPositionMs = nil
            blackMs = 0
            return false
        }
        guard let lastPositionMs else {
            self.lastPositionMs = positionMs
            return false
        }
        let delta = positionMs - lastPositionMs
        self.lastPositionMs = positionMs
        guard delta >= 0, delta <= PlayerController.blackFrameSampleCeilingMs else {
            blackMs = 0
            return false
        }
        blackMs += delta
        guard blackMs >= PlayerController.blackFrameDecodeFailureMs else { return false }
        fired = true
        return true
    }
}

/// What the playback surface adapter made of a raw outcome, in the
/// fixture's vocabulary. File scope, not nested: it crosses the
/// `nonisolated static` adapter boundary.
struct PlaybackSurfaceOutcome: Equatable {
    let source: String
    var actions: [PlaybackFault.Action] = []
    var positionMs: Int? = nil
    var detail: String? = nil
}

struct PlaybackStallObservation: Equatable {
    let delta: Int
    let stagnantDurationMs: Int
}

/// Deltas AVPlayer's cumulative access-log counter and holds the detector's
/// latest sub-threshold stagnant interval until the next 10-second progress
/// report. A new item resets the counter because AVPlayer does too.
struct PlaybackStallObservationState: Equatable {
    private var lastNumberOfStalls = 0
    private var pendingStagnantDurationMs = 0

    mutating func noteRecoveredStagnation(_ durationMs: Int?) {
        pendingStagnantDurationMs = max(pendingStagnantDurationMs, durationMs ?? 0)
    }

    mutating func take(numberOfStalls: Int?) -> PlaybackStallObservation? {
        guard let numberOfStalls else { return nil }
        let current = max(0, numberOfStalls)
        let delta = current >= lastNumberOfStalls
            ? current - lastNumberOfStalls
            : current
        lastNumberOfStalls = current
        defer { pendingStagnantDurationMs = 0 }
        guard delta > 0 else { return nil }
        return PlaybackStallObservation(
            delta: delta,
            stagnantDurationMs: pendingStagnantDurationMs
        )
    }

    mutating func reset() {
        lastNumberOfStalls = 0
        pendingStagnantDurationMs = 0
    }
}

enum PlaybackCompatibilityFallback: Equatable {
    case none
    case hdrBase
    case transcode

    /// Names the rung in the client failure log, so a device log says which
    /// recovery a failure actually bought rather than only that it happened.
    var telemetryName: String {
        switch self {
        case .none: return "none"
        case .hdrBase: return "hdr-base"
        case .transcode: return "transcode"
        }
    }
}

/// What put a playback into the compatibility ladder. Only `itemFailure`
/// carries an AVFoundation error; the other two are client-observed verdicts
/// with no NSError to print, which is exactly why they need naming.
enum PlaybackCompatibilityLadderCause: String, Equatable {
    case itemFailure = "item-failed"
    case readinessTimeout = "readiness-timeout"
    case blackFrames = "black-frames"
}

/// One ladder decision, recorded in `ApplePlaybackFailureLog.detail`.
struct PlaybackCompatibilityLadderStep: Equatable {
    let cause: PlaybackCompatibilityLadderCause
    let fallback: PlaybackCompatibilityFallback
}

/// Monotonic elapsed-time sampling policy for AVPlayer's silent-wait failure mode. A
/// temporary buffer wait gets room to recover on its own; only sustained lack
/// of film-time progress rebuilds the item, which is the in-player equivalent
/// of the back-out-and-play-again workaround.
///
/// One shared clock counts stagnation regardless of `timeControlStatus`. The
/// previous design ran a separate detector per regime and zeroed each one
/// whenever AVPlayer crossed between `.playing` and
/// `.waitingToPlayAtSpecifiedRate`; a starving 4K session flaps between those
/// faster than either detector's threshold, so a real freeze never latched
/// (observed on tvOS: sessions died server-side as `idle` with no stall beacon
/// ever sent). The regime now only *labels* the eventual event — via a tally
/// majority — instead of gating the count.
struct PlaybackStallDetector: Equatable {
    /// Consecutive stagnant 2-second samples before a nudge / a reopen, once
    /// this item has established playback (5 s of real progress).
    static let establishedNudgeChecks = 3
    static let establishedReopenChecks = 6
    /// An unestablished item — the first attach, or the item a recovery
    /// reopen just created — may legitimately buffer for a while before its
    /// first frame, so it gets a longer leash and no nudge. Before this
    /// threshold existed the unestablished state had NO detector at all: a
    /// reopen that landed into continued starvation froze forever with no
    /// error. Fifteen checks ≈ 30 s, then the ladder decides (one bounded
    /// reopen, then the visible terminal screen).
    static let unestablishedReopenChecks = 15

    private(set) var lastPositionMs: Int?
    private(set) var lastAdvanceAt: TimeInterval?
    private(set) var stagnantChecks = 0
    private(set) var stagnantSince: TimeInterval?
    private(set) var recoveredDurationMs: Int?
    /// How many of the current stagnation's samples were taken while AVPlayer
    /// reported `.waitingToPlayAtSpecifiedRate`. Majority picks the event
    /// kind used for the accurate recovery message. Neither kind is decoder
    /// evidence; only an actual item failure may spend the compatibility
    /// ladder.
    private(set) var waitingSamples = 0

    /// The regime majority of the stagnation run that produced the most
    /// recent `.reopen`, snapshotted at fire time because firing restarts
    /// the counters.
    private(set) var firedWaitingMajority = false

    /// Ties go to `.buffering`, the conservative description for an ambiguous
    /// stagnation. The distinction labels the observation; it does not infer
    /// its cause.
    static func waitingMajority(waitingSamples: Int, stagnantChecks: Int) -> Bool {
        waitingSamples * 2 >= stagnantChecks
    }

    mutating func sample(
        positionMs: Int,
        shouldMonitor: Bool,
        established: Bool,
        waitingRegime: Bool,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> PlaybackStallAction {
        guard shouldMonitor else {
            recordRecovery(at: observedAt)
            clearSample()
            return .none
        }
        guard let lastPositionMs else {
            self.lastPositionMs = positionMs
            stagnantSince = observedAt
            return .none
        }

        // Ordinary playback advances much farther than this between the
        // two-second samples. A backwards discontinuity means an item/seek
        // changed under the monitor and establishes a new baseline too.
        if positionMs >= lastPositionMs + 250 || positionMs < lastPositionMs - 250 {
            recordRecovery(at: observedAt)
            self.lastPositionMs = positionMs
            lastAdvanceAt = observedAt
            stagnantChecks = 0
            stagnantSince = observedAt
            waitingSamples = 0
            return .none
        }

        stagnantChecks += 1
        if waitingRegime { waitingSamples += 1 }
        let reopenChecks = established
            ? Self.establishedReopenChecks
            : Self.unestablishedReopenChecks
        if stagnantChecks >= reopenChecks {
            firedWaitingMajority = Self.waitingMajority(
                waitingSamples: waitingSamples, stagnantChecks: stagnantChecks
            )
            self.lastPositionMs = positionMs
            stagnantChecks = 0
            waitingSamples = 0
            return .reopen
        }
        if established && stagnantChecks == Self.establishedNudgeChecks {
            firedWaitingMajority = Self.waitingMajority(
                waitingSamples: waitingSamples, stagnantChecks: stagnantChecks
            )
            return .nudge
        }
        return .none
    }

    func stagnantDurationMs(at observedAt: TimeInterval) -> Int {
        guard let stagnantSince else { return 0 }
        return max(0, Int(((observedAt - stagnantSince) * 1_000).rounded()))
    }

    /// Age of the last sampled film-clock advance. Unlike the recovery
    /// duration, no baseline means unavailable rather than a measured zero.
    func progressAgeMs(at observedAt: TimeInterval) -> Int? {
        guard let lastAdvanceAt else { return nil }
        return max(0, Int(((observedAt - lastAdvanceAt) * 1_000).rounded()))
    }

    mutating func takeRecoveredDurationMs() -> Int? {
        defer { recoveredDurationMs = nil }
        return recoveredDurationMs
    }

    private mutating func recordRecovery(at observedAt: TimeInterval) {
        guard stagnantChecks > 0 else { return }
        recoveredDurationMs = max(
            recoveredDurationMs ?? 0,
            stagnantDurationMs(at: observedAt)
        )
    }

    private mutating func clearSample() {
        lastPositionMs = nil
        stagnantChecks = 0
        stagnantSince = nil
        lastAdvanceAt = nil
        waitingSamples = 0
    }

    mutating func reset() {
        clearSample()
        recoveredDurationMs = nil
        firedWaitingMajority = false
    }
}

/// Owns the shared stall clock so predicate gating, kind labeling, and
/// measured duration are one testable policy rather than parallel expressions
/// inside an asynchronous AVPlayer loop.
///
/// One detector, not one per regime: `timeControlStatus` crossing between
/// `.playing` and `.waitingToPlayAtSpecifiedRate` must never restart the
/// count (the flap itself is a symptom of the stall being measured). The
/// regime tally only decides the event's kind — `.buffering` or `.silent` —
/// for diagnostics and viewer wording. Neither timer-only result claims a
/// decoder error.
struct PlaybackRecoveryMonitor: Equatable {
    private(set) var progressDetector = PlaybackStallDetector()
    private var recoveredStagnantDurationMs: Int?

    @MainActor
    mutating func sample(
        positionMs: Int,
        timeControlStatus: AVPlayer.TimeControlStatus,
        shouldMonitor: Bool,
        establishedPlayback: Bool,
        observedAt: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> PlaybackStallEvent? {
        let action = progressDetector.sample(
            positionMs: positionMs,
            shouldMonitor: shouldMonitor,
            established: establishedPlayback,
            waitingRegime: PlayerController.shouldMonitorBufferingStall(
                timeControlStatus: timeControlStatus
            ),
            observedAt: observedAt
        )
        if let recovered = progressDetector.takeRecoveredDurationMs() {
            recoveredStagnantDurationMs = max(recoveredStagnantDurationMs ?? 0, recovered)
        }
        guard action != .none else { return nil }
        // `stagnantSince` survives a fire (only real progress or a gate change
        // clears it), so this duration describes the whole run that fired.
        return PlaybackStallEvent(
            kind: progressDetector.firedWaitingMajority ? .buffering : .silent,
            action: action,
            positionMs: positionMs,
            durationMs: progressDetector.stagnantDurationMs(at: observedAt)
        )
    }

    mutating func takeRecoveredStagnantDurationMs() -> Int? {
        defer { recoveredStagnantDurationMs = nil }
        return recoveredStagnantDurationMs
    }

    func progressAgeMs(at observedAt: TimeInterval) -> Int? {
        progressDetector.progressAgeMs(at: observedAt)
    }

    mutating func reset() {
        progressDetector.reset()
        recoveredStagnantDurationMs = nil
    }
}

/// One entry of an AVPlayer legible media-selection group, reduced to the two
/// attributes the server actually authored (`LANGUAGE` and `NAME`). Keeping the
/// matching rule off AVFoundation types is what makes it testable.
struct SubtitleRenditionOption: Equatable {
    var languageTag: String?
    var displayName: String
}

/// What one subtitle selection costs: media selection inside the current item,
/// or a replacement server session.
enum SubtitleSelectionRoute: Equatable {
    case mediaSelection
    case bitmapOverlay
    case reopen
}

enum PlayerItemEndAction: Equatable {
    case reopen
    case stop
    case finish(durationMs: Int)
}

enum SameDeliveryRecoveryTransport: Equatable {
    case serverSession
    case offlineAsset
}

/// M5 addition 1 — the create "not yet" retry ladder (contract §3.3 row 6,
/// implementation plan §4.6).
///
/// Restated from `crates/plurxd/src/web/playback-policy.js` (`CREATE_RETRY`),
/// because all three clients have to agree on these numbers and only one of
/// them can run that file. `tests/playback/web-policy.test.js` reads this
/// declaration back out of the Swift and fails if a number drifts.
///
/// File scope and no actor isolation on purpose: it is arithmetic, it touches
/// no player, and the tests run it off the main actor.
enum PlaybackCreateRetry {
    /// The review's ladder, as a CLOSED list. A fourth rung is not "4 s
    /// again": after the third retry the ladder is spent and the owner says
    /// so.
    static let backoffMs: [Int] = [1_000, 2_000, 4_000]

    /// ABSOLUTE, measured from the FIRST attempt — not per attempt. A server
    /// that holds every create for three minutes cannot stretch the sequence,
    /// which is the whole reason the review asked for a deadline rather than a
    /// retry count.
    static let deadlineMs: Int = 60_000

    /// The fixture's `create_503_not_yet` codes, and only these. A refusal the
    /// server explained with any other code is not a "still building" answer
    /// and is not retried.
    static let codes: Set<String> = [
        "startup_timeout",
        "media_owner_transition",
        "vod_index_pending",
        "vod_engine_unattested",
        "transcode_capacity_pending",
        "quality_catalog_unavailable",
    ]

    enum Step: Equatable {
        /// Not a "not yet" answer: the caller's existing handling stands.
        case fail
        /// Wait this long, then re-post the SAME request identity.
        case retry(delayMs: Int)
        /// Nothing left: stop the player, then raise `exhausted`.
        case exhausted(reason: String)
    }

    /// `attempt` counts retries already made (0 before the first one);
    /// `elapsedMs` is measured from the first attempt.
    ///
    /// The deadline is checked twice on purpose: once for time already spent,
    /// and once for the retry that would START after it. Scheduling an attempt
    /// that could only begin past the deadline is how an "absolute" bound turns
    /// back into a per-attempt one.
    static func step(attempt: Int, elapsedMs: Int, isNotYet: Bool) -> Step {
        guard isNotYet else { return .fail }
        let spent = max(0, elapsedMs)
        if spent >= deadlineMs { return .exhausted(reason: "deadline") }
        let index = max(0, attempt)
        guard index < backoffMs.count else { return .exhausted(reason: "ladder_spent") }
        let delayMs = backoffMs[index]
        if spent + delayMs >= deadlineMs { return .exhausted(reason: "deadline") }
        return .retry(delayMs: delayMs)
    }

    /// Is this create refusal the fixture's `create_503_not_yet` row? Read off
    /// the server's own code; a bodiless 503 is not one.
    static func isNotYet(_ error: Error) -> Bool {
        guard let code = (error as? APIError)?.refusalCode else { return false }
        return codes.contains(code)
    }
}

/// The create-retry owner's own outcome. It is thrown only after the owner has
/// already stopped the player and raised `exhausted`, which is why `fail()`
/// answers it with silence rather than a second fault.
enum PlaybackCreateRetryError: Error, Equatable {
    case exhausted(reason: String)
}
