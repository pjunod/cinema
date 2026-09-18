import Foundation

/// One explicit return from Pause to a moving picture. The root identity and
/// deadline survive a same-delivery replacement; a successor gets a new
/// immutable binding rather than making late callbacks from the predecessor
/// look current.
struct PlaybackResumeAttempt: Equatable {
    static let fastPathSeconds: TimeInterval = 1
    static let totalSeconds: TimeInterval = 15
    static let continuingPresentationSeconds: TimeInterval = 0.25
    static let requiredRunwayWallSeconds: Double = 10

    enum RepairAdmission: Equatable {
        case admitted
        case alreadyAdmitted
        case expired
    }

    enum PresentationObservation: Equatable {
        case none
        case firstPicture(milliseconds: Int)
        case settled(firstPictureMs: Int, settledMs: Int)
    }

    let id: String
    let lifecycleGeneration: Int
    let viewerActionEpoch: Int
    let attachmentGeneration: Int
    let itemIdentity: ObjectIdentifier
    let targetMs: Int
    let startedAt: TimeInterval
    let fastPathDeadline: TimeInterval?
    let expiresAt: TimeInterval
    let pauseDurationMs: Int
    let initialRunwaySeconds: Double?
    let initialTimeControlStatus: String
    let initialWaitingReason: String?
    let selectedPath: String
    private(set) var publicationCompleted: Bool
    private(set) var repairAdmitted: Bool
    private(set) var firstPictureAt: TimeInterval?
    private(set) var lastVideoDisplaySeconds: Double?
    private(set) var lastAudioPositionMs: Int?
    private(set) var observedFrameIntervalSeconds: Double?

    init(
        id: String = UUID().uuidString,
        lifecycleGeneration: Int,
        viewerActionEpoch: Int,
        attachmentGeneration: Int,
        itemIdentity: ObjectIdentifier,
        targetMs: Int,
        startedAt: TimeInterval,
        fastPathDeadline: TimeInterval?,
        expiresAt: TimeInterval,
        pauseDurationMs: Int,
        initialRunwaySeconds: Double?,
        initialTimeControlStatus: String,
        initialWaitingReason: String?,
        selectedPath: String,
        publicationCompleted: Bool = false,
        repairAdmitted: Bool = false,
        firstPictureAt: TimeInterval? = nil,
        lastVideoDisplaySeconds: Double? = nil,
        lastAudioPositionMs: Int? = nil,
        observedFrameIntervalSeconds: Double? = nil
    ) {
        self.id = id
        self.lifecycleGeneration = lifecycleGeneration
        self.viewerActionEpoch = viewerActionEpoch
        self.attachmentGeneration = attachmentGeneration
        self.itemIdentity = itemIdentity
        self.targetMs = max(0, targetMs)
        self.startedAt = startedAt
        self.fastPathDeadline = fastPathDeadline
        self.expiresAt = expiresAt
        self.pauseDurationMs = max(0, pauseDurationMs)
        self.initialRunwaySeconds = initialRunwaySeconds
        self.initialTimeControlStatus = initialTimeControlStatus
        self.initialWaitingReason = initialWaitingReason
        self.selectedPath = selectedPath
        self.publicationCompleted = publicationCompleted
        self.repairAdmitted = repairAdmitted
        self.firstPictureAt = firstPictureAt
        self.lastVideoDisplaySeconds = lastVideoDisplaySeconds
        self.lastAudioPositionMs = lastAudioPositionMs
        self.observedFrameIntervalSeconds = observedFrameIntervalSeconds
    }

    /// The successor inherits the root clock and spent repair admission. Its
    /// item and attachment are a new binding, so predecessor callbacks fail an
    /// identity check instead of being reinterpreted as successor evidence.
    func boundToSuccessor(
        attachmentGeneration: Int,
        itemIdentity: ObjectIdentifier,
        baselineVideoDisplaySeconds: Double?
    ) -> PlaybackResumeAttempt {
        PlaybackResumeAttempt(
            id: id,
            lifecycleGeneration: lifecycleGeneration,
            viewerActionEpoch: viewerActionEpoch,
            attachmentGeneration: attachmentGeneration,
            itemIdentity: itemIdentity,
            targetMs: targetMs,
            startedAt: startedAt,
            fastPathDeadline: nil,
            expiresAt: expiresAt,
            pauseDurationMs: pauseDurationMs,
            initialRunwaySeconds: initialRunwaySeconds,
            initialTimeControlStatus: initialTimeControlStatus,
            initialWaitingReason: initialWaitingReason,
            selectedPath: "same-delivery-repair",
            publicationCompleted: publicationCompleted,
            repairAdmitted: true,
            firstPictureAt: nil,
            lastVideoDisplaySeconds: baselineVideoDisplaySeconds,
            lastAudioPositionMs: nil,
            observedFrameIntervalSeconds: nil
        )
    }

    mutating func admitRepair(at now: TimeInterval) -> RepairAdmission {
        guard now < expiresAt else { return .expired }
        guard !repairAdmitted else { return .alreadyAdmitted }
        repairAdmitted = true
        return .admitted
    }

    mutating func markPublicationCompleted() {
        publicationCompleted = true
    }

    func fastPathExpired(at now: TimeInterval) -> Bool {
        fastPathDeadline.map { now >= $0 } ?? false
    }

    func totalExpired(at now: TimeInterval) -> Bool { now >= expiresAt }

    /// A waiting transition breaks continuity. A later frame may start a new
    /// 250 ms interval, but neither a transport `.playing` state nor wall time
    /// over one old frame can settle the attempt.
    mutating func presentationInterrupted() {
        firstPictureAt = nil
        observedFrameIntervalSeconds = nil
        lastAudioPositionMs = nil
    }

    /// Video settlement needs two post-baseline display timestamps and 250 ms
    /// of continuing forward presentation. The first delta after a long pause
    /// is not treated as a frame interval; only a second fresh sample can
    /// establish one.
    mutating func observeVideo(
        displaySeconds: Double,
        at now: TimeInterval,
        waiting: Bool
    ) -> PresentationObservation {
        guard displaySeconds.isFinite, now.isFinite, !waiting else {
            if waiting { presentationInterrupted() }
            return .none
        }
        guard let previous = lastVideoDisplaySeconds else {
            lastVideoDisplaySeconds = displaySeconds
            return .none
        }
        guard displaySeconds > previous else { return .none }
        lastVideoDisplaySeconds = displaySeconds
        if firstPictureAt == nil {
            firstPictureAt = now
            return .firstPicture(milliseconds: elapsedMs(at: now))
        }
        let interval = displaySeconds - previous
        guard interval.isFinite, interval > 0 else { return .none }
        observedFrameIntervalSeconds = interval
        guard let firstPictureAt,
              observedFrameIntervalSeconds != nil,
              now - firstPictureAt >= Self.continuingPresentationSeconds
        else { return .none }
        return .settled(
            firstPictureMs: elapsedMs(at: firstPictureAt),
            settledMs: elapsedMs(at: now)
        )
    }

    /// Audio-only equivalent: the clock must advance across the same 250 ms
    /// continuity interval. A single rate or position sample proves nothing.
    mutating func observeAudio(
        positionMs: Int,
        at now: TimeInterval,
        playing: Bool,
        waiting: Bool
    ) -> PresentationObservation {
        guard playing, !waiting, now.isFinite else {
            if waiting { presentationInterrupted() }
            return .none
        }
        guard let previous = lastAudioPositionMs else {
            lastAudioPositionMs = positionMs
            return .none
        }
        guard positionMs > previous else { return .none }
        lastAudioPositionMs = positionMs
        if firstPictureAt == nil {
            firstPictureAt = now
            return .firstPicture(milliseconds: elapsedMs(at: now))
        }
        guard let firstPictureAt,
              now - firstPictureAt >= Self.continuingPresentationSeconds
        else { return .none }
        return .settled(
            firstPictureMs: elapsedMs(at: firstPictureAt),
            settledMs: elapsedMs(at: now)
        )
    }

    nonisolated static func bufferedFastPathQualifies(
        ready: Bool,
        runwaySeconds: Double?,
        intendedRate: Float,
        hasPendingDestination: Bool,
        replacementInFlight: Bool
    ) -> Bool {
        guard ready, !hasPendingDestination, !replacementInFlight,
              intendedRate.isFinite, intendedRate > 0,
              let runwaySeconds, runwaySeconds.isFinite, runwaySeconds >= 0
        else { return false }
        return runwaySeconds > Double(intendedRate) * requiredRunwayWallSeconds
    }

    private func elapsedMs(at now: TimeInterval) -> Int {
        max(0, Int(((now - startedAt) * 1_000).rounded()))
    }
}

/// Credential-free resume telemetry. This event is separate from `ttff`, so a
/// resume cannot overwrite or masquerade as the title's original cold start.
struct ApplePlaybackResumeLog: Encodable, Equatable {
    let level = "info"
    let event = "resume_attempt"
    let message: String
    let method: String
    let title: String
    let fileId: Int
    let sessionId: String?
    let attempt: String
    let phase: String
    let outcome: String
    let elapsedMs: Int
    let pauseDurationMs: Int
    let initialRunwaySeconds: Double?
    let initialTimeControlStatus: String
    let waitingReason: String?
    let path: String
    let firstPictureMs: Int?
    let settledMs: Int?
    let ua = "Apple AVPlayer"

    init(
        attempt: PlaybackResumeAttempt,
        phase: String,
        outcome: String,
        elapsedMs: Int,
        method: String,
        title: String,
        fileId: Int,
        sessionId: String?,
        firstPictureMs: Int? = nil,
        settledMs: Int? = nil
    ) {
        self.message = "resume \(phase): \(outcome)"
        self.method = method
        self.title = title
        self.fileId = fileId
        self.sessionId = sessionId
        self.attempt = attempt.id
        self.phase = phase
        self.outcome = outcome
        self.elapsedMs = max(0, elapsedMs)
        self.pauseDurationMs = attempt.pauseDurationMs
        self.initialRunwaySeconds = attempt.initialRunwaySeconds
        self.initialTimeControlStatus = attempt.initialTimeControlStatus
        self.waitingReason = attempt.initialWaitingReason
        self.path = attempt.selectedPath
        self.firstPictureMs = firstPictureMs
        self.settledMs = settledMs
    }

    enum CodingKeys: String, CodingKey {
        case level, event, message, method, title, attempt, phase, outcome, path, ua
        case fileId = "file_id"
        case sessionId = "session_id"
        case elapsedMs = "elapsed_ms"
        case pauseDurationMs = "pause_duration_ms"
        case initialRunwaySeconds = "initial_runway_seconds"
        case initialTimeControlStatus = "initial_time_control_status"
        case waitingReason = "waiting_reason"
        case firstPictureMs = "first_picture_ms"
        case settledMs = "settled_ms"
    }
}
