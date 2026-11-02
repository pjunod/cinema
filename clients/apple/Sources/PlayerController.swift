import AVKit
import Combine
import CryptoKit
import Foundation
import MediaPlayer

/// Drives one AVPlayer and executes the server-owned delivery plan. It also
/// supplies the controls AVPlayer withholds for a growing EVENT playlist: an
/// explicit on-demand timeline, reliable play/pause commands, server playback
/// telemetry, and stream restarts for audio, quality, and burn-only subtitle
/// changes. Ordinary text subtitles switch through AVPlayer media selection —
/// once the stream carries their renditions, which under the default
/// `SubtitleReadiness.onDemand` is from the first selection rather than from the
/// first frame (`needsNativeSubtitleSession`).
@MainActor
final class PlayerController: ObservableObject {
    /// AVFoundation preparation may suspend; only the controller's current
    /// operation is allowed to commit the prepared selection to the item.
    struct PreparedNativeSelection {
        var hasSubtitleOptions: Bool
        var apply: @MainActor () -> Bool
    }

    struct MediaSelectionPreparation {
        var native: @MainActor (Int?, [SubtitleTrack], AVPlayerItem) async -> PreparedNativeSelection = {
            index, tracks, item in
            let group = try? await item.asset.loadMediaSelectionGroup(for: .legible)
            return PreparedNativeSelection(
                hasSubtitleOptions: group?.options.contains { $0.mediaType == .subtitle } ?? false,
                apply: {
                    guard let group else { return index == nil }
                    return PlayerController.selectNativeSubtitle(index, tracks: tracks, in: group, of: item)
                }
            )
        }
        var audio: @MainActor (String, AVPlayerItem) async -> (@MainActor () -> Void)? = { language, item in
            guard let group = try? await item.asset.loadMediaSelectionGroup(for: .audible) else { return nil }
            let preferred = AVMediaSelectionGroup.mediaSelectionOptions(
                from: group.options,
                filteredAndSortedAccordingToPreferredLanguages: PlayerController.languageSpellings(language)
            )
            guard let option = preferred.first else { return nil }
            return { item.select(option, in: group) }
        }
    }

    struct ItemPreparation {
        var ready: @MainActor (AVPlayerItem, PlayerItemReadiness.Context) async throws -> Void = {
            try await PlayerController.awaitItemReady($0, context: $1)
        }
        // AVPlayer can complete an interrupted seek without reaching the
        // resume point. Preserve its result so preparation cannot accept it.
        var seek: @MainActor (AVPlayer, Int) async -> Bool = { player, ms in
            await player.seek(
                to: CMTime(seconds: Double(ms) / 1000.0, preferredTimescale: 600),
                toleranceBefore: .zero, toleranceAfter: .zero
            )
        }
    }

    struct SeekPreparationOwner {
        let actionEpoch: Int
        let seekGeneration: Int
        let targetMs: Int
    }

    enum DeliveryFallback: CaseIterable { case hdrBase, transcode }

    /// Delivery fallbacks are recipe changes, even when a newer viewer task
    /// cancels the particular reopen that first requested them.
    func requireDeliveryFallback(_ fallback: DeliveryFallback) {
        switch fallback {
        case .hdrBase:
            guard !forceCompatibleHDRBase else { return }
            forceCompatibleHDRBase = true
        case .transcode:
            guard !forceCompatibilityTranscode else { return }
            forceCompatibilityTranscode = true
        }
        recipeRevision.change()
    }

    private let requestPlaybackDecision: @MainActor (AppModel, PlaybackFileContext, PrePlaySelection, PlaybackQuality) async throws -> (decision: Decision, caps: DeviceCaps)
    private let requestHlsStatus: @MainActor (AppModel, String) async throws -> PlaybackSessionStatus
    private let requestHlsSession: @MainActor (AppModel, PlaybackFileContext, CreateSessionRequest) async throws -> HlsStart
    private let readControlSequence: @MainActor (PlaybackControlSession) async -> UInt64?
    private let reportPlaybackIntent: @MainActor (PlaybackControlSession) async -> UInt64?
    private let mediaSelectionPreparation: MediaSelectionPreparation
    private let itemPreparation: ItemPreparation
    private let makePlayerItem: @MainActor (URL) -> AVPlayerItem
    private let prepareGrowingPlaylist: @MainActor (URL) async throws -> Void
    private let nativeSeek: @MainActor (AVPlayer, Int) async -> Bool
    private let waitNativeSeekDeadline: @MainActor () async throws -> Void
    private let canPlayOffline: (AVURLAsset) -> Bool
    private let waitInitialDecisionDeadline: @MainActor () async throws -> Void
    /// M5's only clock. It is both the backoff between rungs and the absolute
    /// deadline, so a test can drive a sixty-second bound without spending a
    /// minute — the same reason `waitInitialDecisionDeadline` exists.
    private let waitCreateRetry: @MainActor (Int) async throws -> Void
    private let resumeNow: @MainActor () -> TimeInterval
    private let waitResumeSample: @MainActor () async -> Void
    /// Retiring a session nobody is going to attach. Injected because "a create
    /// that lands after the deadline is RELEASED, never attached" is M5's one
    /// property with a leaked transcode on the other side of it, and a test
    /// that cannot see the release cannot pin it.
    private let releaseHlsSession: @MainActor (AppModel, String) async -> Void

    init(
        player: AVPlayer = AVPlayer(),
        makePlayerItem: @escaping @MainActor (URL) -> AVPlayerItem = { AVPlayerItem(url: $0) },
        requestPlaybackDecision: (@MainActor (AppModel, Int, PrePlaySelection, PlaybackQuality) async throws -> (decision: Decision, caps: DeviceCaps))? = nil,
        mediaSelectionPreparation: MediaSelectionPreparation = MediaSelectionPreparation(),
        itemPreparation: ItemPreparation = ItemPreparation(),
        nativeSeek: @escaping @MainActor (AVPlayer, Int) async -> Bool = { player, ms in
            await player.seek(
                to: CMTime(seconds: Double(ms) / 1000.0, preferredTimescale: 600),
                toleranceBefore: .zero, toleranceAfter: .zero
            )
        },
        waitNativeSeekDeadline: @escaping @MainActor () async throws -> Void = {
            try await Task.sleep(for: .seconds(PlayerController.seekPresentationDeadlineSeconds))
        },
        canPlayOffline: @escaping (AVURLAsset) -> Bool = { $0.assetCache?.isPlayableOffline == true },
        waitInitialDecisionDeadline: @escaping @MainActor () async throws -> Void = {
            try await Task.sleep(for: .seconds(20))
        },
        waitCreateRetry: @escaping @MainActor (Int) async throws -> Void = {
            try await Task.sleep(for: .milliseconds($0))
        },
        releaseHlsSession: @escaping @MainActor (AppModel, String) async -> Void = {
            await $0.endHlsSession($1)
        },
        prepareGrowingPlaylist: @escaping @MainActor (URL) async throws -> Void = {
            try await GrowingPlaylistPreparation.wait(for: $0)
        },
        requestHlsSession: (@MainActor (AppModel, Int, CreateSessionRequest) async throws -> HlsStart)? = nil,
        requestHlsStatus: @escaping @MainActor (AppModel, String) async throws -> PlaybackSessionStatus = {
            try await $0.hlsStatus($1)
        },
        readControlSequence: @escaping @MainActor (PlaybackControlSession) async -> UInt64? = {
            await $0.controlSequence
        },
        reportPlaybackIntent: @escaping @MainActor (PlaybackControlSession) async -> UInt64? = {
            await $0.reportIntent()
        },
        resumeNow: @escaping @MainActor () -> TimeInterval = {
            ProcessInfo.processInfo.systemUptime
        },
        waitResumeSample: @escaping @MainActor () async -> Void = {
            try? await Task.sleep(for: PlayerController.resumeSampleDelay)
        },
        displayWakeOwner: DisplayWakeOwner? = nil
    ) {
        self.player = player
        self.makePlayerItem = makePlayerItem
        displayWake = PlaybackDisplayWake(owner: displayWakeOwner)
        self.requestPlaybackDecision = { model, context, selection, quality in
            let id = try context.localID()
            if let requestPlaybackDecision { return try await requestPlaybackDecision(model, id, selection, quality) }
            return try await model.playbackDecision(fileId: id, selection: selection, quality: quality, fileContext: context)
        }
        self.mediaSelectionPreparation = mediaSelectionPreparation
        self.itemPreparation = itemPreparation
        self.prepareGrowingPlaylist = prepareGrowingPlaylist
        self.nativeSeek = nativeSeek
        self.waitNativeSeekDeadline = waitNativeSeekDeadline
        self.canPlayOffline = canPlayOffline
        self.waitInitialDecisionDeadline = waitInitialDecisionDeadline
        self.waitCreateRetry = waitCreateRetry
        self.releaseHlsSession = releaseHlsSession
        self.requestHlsSession = { model, context, body in
            let id = try context.localID()
            if let requestHlsSession { return try await requestHlsSession(model, id, body) }
            return try await model.createHlsSession(fileId: id, body: body, fileContext: context)
        }
        self.requestHlsStatus = requestHlsStatus
        self.readControlSequence = readControlSequence
        self.reportPlaybackIntent = reportPlaybackIntent
        self.resumeNow = resumeNow
        self.waitResumeSample = waitResumeSample
        textTranslationObservation = subtitleTextTranslation.objectWillChange.sink { [weak self] in self?.objectWillChange.send() }
        progressiveCaptionObservation = progressiveCaptions.objectWillChange.sink { [weak self] in
            self?.objectWillChange.send()
        }
    }

    /// The server retains 180 seconds behind the download frontier: the 120 s
    /// forward lead measured on a physical iPad, 30 s for back-buffering, and
    /// 30 s for retry/reload. `preferredForwardBufferDuration` is a hint, not a
    /// cap — AVPlayer fetched twice this 60 s preference. Keep growing HLS
    /// sessions inside that contract while leaving direct and completed-VOD
    /// items under AVPlayer's normal policy.
    static let growingHLSForwardBufferSeconds: TimeInterval = 60
    nonisolated static let repeatedEndToleranceMs = 250
    nonisolated static let naturalEndToleranceMs = 15_000
    /// How long an attached item may sit at `.unknown` before the wait is
    /// spent. A resume has always been bounded by this because it had to seek;
    /// a fresh start was not bounded at all, which is what let a title AVPlayer
    /// neither readies nor fails hold the open forever behind a black screen.
    nonisolated static let itemReadinessDeadlineSeconds = 15
    /// Separate from the observed readiness path: these delays coalesce seek
    /// intent and sample resume presentation, respectively.
    private static let seekCoalescingDelay: Duration = .milliseconds(100)
    private static let resumeSampleDelay: Duration = .milliseconds(50)
    /// Film time that may pass on a black screen before the picture is judged
    /// undecodable. Above `PlayerAttachmentRecoveryState`'s five-second
    /// establishment window on purpose: audio-only progress must not be able
    /// to certify a playback that has never drawn a frame.
    static let blackFrameDecodeFailureMs = 6_000
    /// The most film time one periodic sample may contribute to that total.
    /// The observer runs twice a second, so anything past this is a seek or an
    /// item replacement, not playback — and must not be counted as black.
    static let blackFrameSampleCeilingMs = 2_000
    nonisolated static let gracefulRepeatedEndFraction = 0.95
    static let playbackStartFailureTitle = "Couldn't start playback."
    static let playbackStoppedFailureTitle = "Playback stopped."
    /// The web's sentence for the same row, moved rather than rewritten (§7:
    /// no wording pass). A still-starting session is not a failure.
    static let stillPreparingTitle = "Still preparing this stream…"
    static let createRetryExhaustedMessage =
        "The server is still building this stream and did not finish in a minute."
    static let earlyEndFailureTitle = "Playback stopped early."
    static let repeatedEarlyEndMessage =
        "Playback ended early at the same position after retrying the stream."
    static let readinessTimeoutMessage =
        "The stream never became ready to play."
    static let blackFrameFailureMessage =
        "The film clock advanced with no decoded picture."

    /// A fresh start hands `seekWhenReady` nothing to wait on, so before this
    /// only a resume was bounded. Bound the completed-VOD/direct cold start the
    /// same way.
    ///
    /// Two exclusions, both because the wait is then somebody else's: a growing
    /// session may still be filling the server's publish gate, and a reopen
    /// that lands paused is not a viewer sitting in front of a black screen —
    /// it is an item deliberately prepared at rate 0.
    nonisolated static func shouldBoundFreshStartReadiness(
        isVOD: Bool,
        startMs: Int,
        seeksAfterAttach: Bool,
        resumesPlayback: Bool
    ) -> Bool {
        isVOD && startMs <= 0 && !seeksAfterAttach && resumesPlayback
    }

    static func configureBuffering(_ item: AVPlayerItem, growingHLS: Bool) {
        item.preferredForwardBufferDuration = growingHLS
            ? growingHLSForwardBufferSeconds
            : 0
    }

    /// AVPlayer can announce the temporary end of a growing playlist. A
    /// server duration proves where the title should end; a finite item
    /// duration is a safe fallback for direct/offline files whose catalog row
    /// was never probed. One early end may reopen, but the replacement ending
    /// at the same clock position is terminal: reopening it again cannot make
    /// progress and otherwise creates an unbounded session loop.
    nonisolated static func endAction(
        knownDurationMs: Int,
        itemDurationMs: Int?,
        isGrowingPlaylist: Bool,
        endedAt: Int,
        previousUncorroboratedEndMs: Int? = nil
    ) -> PlayerItemEndAction {
        let corroboratedDuration = knownDurationMs > 0
            ? knownDurationMs
            : isGrowingPlaylist
                ? nil
                : itemDurationMs.flatMap { $0 > 0 ? $0 : nil }
        let repeatedAtSamePosition = previousUncorroboratedEndMs
            .map {
                abs(max(0, endedAt) - max(0, $0)) < repeatedEndToleranceMs
            }
            ?? false
        guard let durationMs = corroboratedDuration else {
            guard repeatedAtSamePosition else { return .reopen }
            if isGrowingPlaylist { return .stop }
            return .finish(durationMs: max(endedAt, previousUncorroboratedEndMs ?? 0))
        }
        guard endedAt >= durationMs - naturalEndToleranceMs else {
            guard repeatedAtSamePosition else { return .reopen }
            let terminalPosition = max(endedAt, previousUncorroboratedEndMs ?? 0)
            if Double(terminalPosition) / Double(durationMs) >= gracefulRepeatedEndFraction {
                return .finish(durationMs: terminalPosition)
            }
            return .stop
        }
        return .finish(durationMs: durationMs)
    }

    @Published private(set) var player: AVPlayer {
        didSet {
            restorePreparedIncumbentBufferPreference()
            syncDisplayWake()
        }
    }
    @Published private(set) var stagedSurfacePlayer: AVPlayer?
    private weak var playbackSurface: PlayerSurfaceView?
    #if DEBUG
    var localVideoEvidenceForTesting: ((AVPlayer, AVPlayerItem) -> LocalVideoEvidence)?
    func setBlackFrameWatchdogForTesting(_ watchdog: BlackFrameWatchdog) { blackFrameWatchdog = watchdog }
    var blackFrameWatchdogForTesting: BlackFrameWatchdog { blackFrameWatchdog }
    /// Enter the real queued continuation without opening a network stream.
    func queueBlackFrameRecoveryForTesting(for item: AVPlayerItem, at position: Int) -> Task<Void, Never> {
        started = true
        return queueBlackFrameRecovery(for: item, at: position)
    }
    #endif

    func currentLocalVideoEvidence() -> LocalVideoEvidence {
        guard let item = player.currentItem else { return .unavailable }
        #if DEBUG
        if let read = localVideoEvidenceForTesting { return read(player, item) }
        #endif
        return playbackSurface?.videoEvidence(for: player, item: item) ?? .unavailable
    }

    private var warmPredecessor: AVPlayer?

    func attachPlaybackSurface(_ surface: PlayerSurfaceView, attached: Bool) {
        if attached { playbackSurface = surface }
        else if playbackSurface === surface { playbackSurface = nil }
    }

    private func adoptWarmPlayer(_ next: AVPlayer) {
        if let timeObserver {
            player.removeTimeObserver(timeObserver)
            self.timeObserver = nil
        }
        player = next
        addPeriodicObserver()
    }

    private func applyWarmPredecessorTransport() {
        guard let previous = warmPredecessor else { return }
        if wantsPlayback { previous.playImmediately(atRate: preferredRate) }
        else { previous.pause() }
    }

    @Published private(set) var decision: Decision? {
        didSet { syncDisplayWake() }
    }
    /// The screensaver and auto-lock stay off while this title's video is
    /// playing or buffering toward play, and only then. It follows `player`
    /// through a prepared commit and `decision` through every open, so an
    /// audiobook (no source video codec) never holds the display awake.
    private let displayWake: PlaybackDisplayWake

    private func syncDisplayWake() {
        displayWake.track(player, hasVideo: decision?.source?.videoCodec != nil)
    }
    /// Immutable facts used to obtain `decision`; every session opened by this
    /// controller repeats them even if another screen probes in the meantime.
    private var autoDesiredCandidate: QualityCandidate?
    private var autoActiveCandidateId: String?
    private var autoRouteProtocol: String?
    private var autoNextTickMs = 0
    private var autoCostRefreshTask: Task<Void, Never>?
    private var autoCostRefreshDeadlineTask: Task<Void, Never>?
    private var autoCostRefreshGeneration = 0
    private var autoCostRefreshNextMs = 0
    private var autoUpgradeSinceMs: Int?
    private var autoUpgradeEvidence = AutoUpgradeEvidenceWindow()
    private var autoLastSwitchMs: Int?
    private var autoLastEvaluation: (item: ObjectIdentifier, attempt: String, atMs: Int)?
    private var autoSwitchTimes: [Int] = []
    private var autoBlockedUntil: [String: Int] = [:]
    private var autoPreparing = false
    private var autoVoluntary = true
    private var autoExposed = false
    private var autoMildSamples = 0
    private var autoTransferPressure = AutoTransferPressureEvidence()
    private var autoDecoderRejected: Set<String> = []
    private var autoDecodePressure = AutoDecodePressureWindow()
    private var autoDecodeQualityResponseUsed = false
    private var autoRecoveryCause: AutoRecoveryCauseTicket?
    private var autoPreparedTargetRevision: UInt64?
    private var autoPreparedViewerEpoch: Int?
    /// An original-quality preparation a viewer boundary asked for. It is an
    /// ordinary Auto prepared replacement in every respect — the incumbent
    /// keeps playing and the ordinary commit aligns and hands off — except
    /// that a fresh link proof stands in for the mid-play quiet window.
    /// Nothing waits on it.
    @MainActor private final class AutoBoundaryAttempt {
        let attempt: Attempt
        let item: ObjectIdentifier
        let sessionId: String
        let candidateId: String
        let previous: QualityCandidate?

        init(attempt: Attempt, item: ObjectIdentifier, sessionId: String, candidateId: String,
             previous: QualityCandidate?) {
            self.attempt = attempt
            self.item = item
            self.sessionId = sessionId
            self.candidateId = candidateId
            self.previous = previous
        }
    }
    private var autoBoundaryAttempt: AutoBoundaryAttempt?
    /// The viewer boundary the next ordinary Auto evaluation owes a re-plan.
    private var autoBoundaryReplan = AutoBoundaryReplan()
    private var autoTransportRevision: UInt64 = 1
    private var autoDestinationRevision: UInt64 = 1
    private var autoRecipeRevision: UInt64 = 1
    private var autoEnvelopeSelection: MediaIntentSelection?
    private var autoStagedStatus: PlaybackSessionStatus?
    private var autoStagedStatusObservedMs: Int?

    private var decisionCaps: DeviceCaps?
    private var presentationTarget: PresentationTarget?
    private var presentationRevision: UInt64 = 0

    /// The view supplies active backing pixels before AVPlayerLayer letterboxing.
    func updatePresentationTarget(widthPx: Int?, heightPx: Int?) {
        guard let widthPx, let heightPx,
              (1...16384).contains(widthPx), (1...16384).contains(heightPx) else {
            presentationTarget = nil
            autoUpgradeSinceMs = nil
            return
        }
        guard presentationTarget?.widthPx != widthPx || presentationTarget?.heightPx != heightPx else { return }
        autoUpgradeSinceMs = nil
        presentationRevision = min(presentationRevision + 1, 9_007_199_254_740_991)
        presentationTarget = PresentationTarget(widthPx: widthPx, heightPx: heightPx, revision: presentationRevision)
    }
    @Published private(set) var sessionStatus: PlaybackSessionStatus?
    /// Last successful status response for a stall report. The visible status
    /// is allowed to become unavailable when a poll fails, but that failure is
    /// precisely when diagnostics must retain the last server evidence and say
    /// how old it was.
    private var diagnosticSessionStatus: PlaybackSessionStatus?
    private var diagnosticSessionStatusObservedAt: Date?
    private var preparationSample: PlayerItemReadiness.Sample?
    var sessionStatusAgeMs: Int? {
        guard sessionStatus != nil, let observedAt = diagnosticSessionStatusObservedAt else {
            return nil
        }
        return max(0, Int(Date().timeIntervalSince(observedAt) * 1_000))
    }
    var presentationProgressAgeMs: Int? {
        playbackRecoveryMonitor.progressAgeMs(
            at: ProcessInfo.processInfo.systemUptime
        )
    }
    /// The blocking, answer-me surface the old `failed` flag stood for: a
    /// prompt or a terminal the viewer has to dismiss. A full-screen
    /// `preparing` covers the picture but asks nothing, and keeps today's
    /// routing (PLAYBACK-SURFACE-CONTRACT.md §4).
    var isPlaybackBlocked: Bool { surface.entersFailedRouting }

    /// Which of the two shapes a PROGRESS fault — `preparing`, `buffering` or
    /// `recovering` — is drawn in, or `nil` for nothing at all.
    ///
    /// One property, because there is now one owner of that pixel. Until this
    /// branch the same pixel had two: `isChangingStream` drew a spinner and
    /// the legacy `isPlaybackWaiting` drew a second one straight off
    /// `timeControlStatus`, and which of them covered the picture was decided
    /// by the order of two `if`s in a view. Both are now faults.
    ///
    /// But one owner is not one render. §3.1 gives a progress fault two, and
    /// the difference is the whole point of the class: full-screen **while the
    /// attached picture is not presenting**, an in-chrome indicator **while it
    /// is**. `recovering` retires on presentation that postdates the raise, so
    /// a compatibility rung, a node failover or a readiness deadline is an
    /// indicator over a picture that is playing perfectly well — and drawing
    /// that as the full-screen box is the overlay-over-a-moving-picture this
    /// contract exists to remove. Neither is the input contract's `failed`:
    /// both cover pixels and ask the viewer nothing (§4).
    var progressSurfaceRender: ProgressRender? {
        Self.progressRender(for: surface.surface)
    }

    enum ProgressRender: String, Equatable, Sendable, CaseIterable {
        /// Covers the picture. Only ever over one that is not presenting.
        case blocking
        /// Beside the picture, which is presenting behind it.
        case indicator
    }

    /// Pure, so the distinction is provable rather than read off a view: a
    /// SwiftUI body is not something a unit test can look at.
    nonisolated static func progressRender(for surface: PlaybackSurface) -> ProgressRender? {
        switch surface.kind {
        case .blocking: return surface.entersFailedRouting ? nil : .blocking
        case .indicator: return .indicator
        case .none, .banner: return nil
        }
    }

    @Published private(set) var currentMs = 0
    @Published private(set) var knownDurationMs = 0
    @Published private(set) var isPlaying = false {
        didSet { if isPlaying != oldValue { updateNowPlaying() } }
    }
    /// An open, a reopen, a failover or a prepared commit is in flight.
    ///
    /// Contract §3.3 row 10: that is a `client_preparing` fault — the staged
    /// start every client paints, with no refusal behind it — and what it
    /// draws is the presenter's to decide. The raise hangs off the flag's own
    /// transition rather than off the four sites that set it, because a dozen
    /// sites clear it; keying both halves to the one transition is what makes
    /// the fault's life exactly the flag's life.
    ///
    /// The fault carries the viewer request it belongs to, so it is retired by
    /// that request settling or being superseded and never by the
    /// predecessor's own picture: a warm quality change happens over a moving
    /// picture, and a fault evidence could retire would be swept away in the
    /// same step that raised it.
    @Published private(set) var isChangingStream = false {
        didSet {
            guard isChangingStream != oldValue else { return }
            if isChangingStream {
                raiseStreamChangePreparing()
            } else {
                settleStreamChangePreparing()
            }
        }
    }
    let progressiveCaptions = ProgressiveCaptionSession()
    let subtitleTextTranslation = SubtitleTextTranslation()
    private var textTranslationObservation: AnyCancellable?
    private var progressiveCaptionObservation: AnyCancellable?
    private var progressiveCaptionTask: Task<Void, Never>?
    var progressiveCaptionOutputSelected: Bool { progressiveCaptions.selected || progressiveCaptionTask != nil }
    var captionIntent: CaptionIntent {
        if progressiveCaptions.selected { return .progressive(progressiveCaptions.assetId) }
        return selectedSubtitle.map(CaptionIntent.source) ?? .off
    }
    @Published private(set) var selectedSubtitle: Int?
    @Published private(set) var selectedAudio: Int?
    @Published private(set) var selectedHeight: Int?
    /// `nil` height means either Auto or Original. Keep the viewer's explicit
    /// no-encode choice distinct so control and replacement bodies do not
    /// silently grant adaptation authority.
    @Published private(set) var selectedQualityIsOriginal = false
    @Published private(set) var qualityChangeRetained = false
    private var manualQualityRetention = ManualQualityRetention()
    private var playbackControlOrigin: String?
    private var pendingPreparedControl: (actionId: String, sessionId: String, bootstrap: ControlBootstrap, origin: String)?
    private weak var unprovenPreparedItem: AVPlayerItem?
    @Published private(set) var encoder: String?
    /// The dynamic range of the bytes this playback is actually receiving —
    /// `"dolby_vision" | "hdr10" | "hlg" | "sdr"`, or nil against a server that
    /// does not report it. Purely a readout: nothing in this controller ever
    /// reads it back, and no decision, capability, or session request depends
    /// on it (MEDIA-BADGES-PLAN.md §9).
    @Published private(set) var effectiveProcessing: EffectiveProcessingReport?
    @Published private(set) var effectiveProcessingGeneration: String?
    /// The server's Dolby Vision enhancement answer for the current HLS
    /// session (FEL / RPU / DYN). Held for the session and cleared wherever
    /// the session's delivery answer is — but not on a seek: a seek clears
    /// `effectiveProcessing`, and `PlaybackEnhancement.presented` hides a
    /// report-gated answer until the next generation's report returns, while
    /// a native Dolby Vision answer stays true across the seek.
    @Published private(set) var playbackEnhancement: PlaybackEnhancement?
    @Published private(set) var deliveredRange: String?
    /// Take a delivery answer, both halves at once.
    ///
    /// The range and the profile are one answer and are assigned together,
    /// deliberately. A session that reports a range but omits the profile is
    /// saying "no profile", not "keep the one you had": the legacy
    /// single-ffmpeg copy path serves the HDR10 base for a title the decision
    /// said would be converted, which is the *normal first watch* of a
    /// converting title, before its fragment index exists. Two independent
    /// `??` fallbacks would keep the decision's profile through exactly that
    /// and paint `DV P7 → DV P8` over HDR10 — and each half would look correct
    /// on its own, which is why this is one function rather than two lines.
    ///
    /// A session that reports no range at all says nothing about either, and
    /// the decision's answer stands for both.
    private func adoptSessionDelivery(_ hls: HlsStart, decision: Decision) {
        if let range = hls.deliveredDynamicRange {
            deliveredRange = range
            deliveredDolbyVisionProfile = hls.deliveredDolbyVisionProfile
        } else {
            effectiveProcessing = nil
            effectiveProcessingGeneration = nil
            playbackEnhancement = nil
            deliveredRange = decision.deliveredDynamicRange
            deliveredDolbyVisionProfile = decision.deliveredDolbyVisionProfile
        }
    }

    /// The Dolby Vision profile on the wire, when the delivery carries any.
    ///
    /// Moves with `deliveredRange` and never on its own — see
    /// `adoptSessionDelivery`. Read together they say "Dolby Vision, and it is
    /// Profile 8"; read apart they can say "Profile 8" over an HDR10 stream.
    @Published private(set) var deliveredDolbyVisionProfile: Int?
    @Published private(set) var isVOD = false
    /// The playback surface, as one projection of the player.
    ///
    /// It replaces `failed`, `playbackError`, `playbackFailureTitle` and
    /// `playbackNotice`, which were four independent fields that any call site
    /// could write and no call site was ever told had gone stale. Every former
    /// writer now raises a typed fault; the presenter decides what is drawn
    /// (docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3).
    @Published private(set) var surface = PlaybackSurfaceModel()
    /// The wait sentence as it is now — runway and the server's parked-request
    /// count — refreshed on the surface clock. The fault keeps the sentence
    /// from the instant its wait began, which is always "0.0 s client loaded";
    /// the view draws this instead for a wait (`PlaybackWaitPresentation`).
    @Published private(set) var waitDetail: String?
    @Published private(set) var finished = false
    @Published private(set) var pgsOverlayWindow: PGSOverlayWindow?
    @Published private(set) var pgsOverlayStatus: PGSOverlayStatus = .off
    @Published private(set) var lastTTFFMs: Int?
    @Published private(set) var playbackControlSummary: String?

    private var baseMs = 0
    private var itemId = 0
    private var fileId = 0
    private var fileContext: PlaybackFileContext?
    private func contextForFile(_ id: Int) throws -> PlaybackFileContext {
        guard let fileContext else { throw APIError.badURL }
        _ = try fileContext.localID(expected: id)
        return fileContext
    }
    private var progressOffsetMs = 0
    private var itemDurationMs: Int?
    private var title = ""
    #if os(iOS)
    private var offlineId: String?
    private var offlineAssetURL: URL?
    #endif
    private var audioOverride: Int?
    /// The detail screen's pre-play choice for *this* playback. It travels to
    /// `/decision`, so the server's verdict and delivery plan already account
    /// for it — which is exactly why it is not an `audioOverride`: overriding
    /// would force a copy session onto a choice the server judged direct-
    /// playable, downgrading delivery past what the verdict states.
    private struct InitialDecisionRequest {
        var selection = PrePlaySelection.none
        var quality = PlaybackQuality.auto
        var explicitHeight: Int?
    }
    /// Desired input exists before metadata does. Resolved/displayed selections
    /// below are outputs of this request, never defaults allowed to overwrite it.
    private var initialDecisionRequest = InitialDecisionRequest()
    private var prePlaySelection: PrePlaySelection { initialDecisionRequest.selection }
    private var initialDecisionGeneration = 0
    private var initialDecisionDeadlineTask: Task<Void, Never>?
    private weak var model: AppModel?
    private var timeObserver: Any?
    private let accessLogSampler = PlayerAccessLogSampler()
    private var lastPreparedAccessLogSequence: Int?
    private var interactiveSeekTask: Task<Void, Never>?
    private var seekPresentationTask: Task<Void, Never>?
    private var resumeIntentTask: Task<Void, Never>?
    private var resumePresentationTask: Task<Void, Never>?
    private var resumeRepairTask: Task<Void, Never>?
    private var resumeAttempt: PlaybackResumeAttempt?
    private var resumeRepairPendingReason: String?
    /// A failed retained item waits only for the ordered Resume publication.
    /// Its AVFoundation classification is preserved when publication lands;
    /// it is never collapsed into the timer-only same-delivery route.
    private var resumePendingFailedItem: AVPlayerItem?
    private var pauseBeganAt: TimeInterval?
    private var explicitViewerPause: (item: ObjectIdentifier, session: String?, attachment: Int, lifecycle: Int)?
    /// The server retired this presentation while the viewer was paused.
    ///
    /// A rolling session ends 180 s after an accepted Hold
    /// (`ROLLING_PAUSE_GRACE`): the control exchange then answers
    /// `410 pause_grace_expired`, and AVPlayer — which keeps reloading a live
    /// playlist while paused — fails the item on the next reload with a
    /// 404/410 it reports as "resource unavailable". Nobody is watching, so
    /// that is not a failure to show. The sliding-HLS contract (§9.5) says a
    /// paused client stays paused and opens one replacement at the saved
    /// position on resume; this is that latch. `setPlaybackRequested(true)`
    /// consumes it and `stop()` clears it.
    ///
    /// It names the session it is about and is honoured only while that
    /// session is still the attached one. It is deliberately NOT cleared at
    /// the top of `open()`: a create that fails puts the dead item back
    /// (`restoreAfterFailedChange`) with `sessionId` unchanged, and a latch
    /// cleared there would leave Play starting a dead item. A successor that
    /// attaches replaces `sessionId`, which retires the latch by identity.
    private var pausedRetirement: PausedRetirement?
    struct PausedRetirement: Equatable {
        /// The rolling session the server retired.
        var sessionId: String
        /// The film position to reopen at, when the item that knew it is
        /// already dead. Nil means the item is still attached and its own
        /// clock is read at resume.
        var positionMs: Int?
    }

    /// The latch, only while it still describes the attached session.
    private var currentPausedRetirement: PausedRetirement? {
        guard let retired = pausedRetirement, retired.sessionId == sessionId else { return nil }
        return retired
    }
    private var seekPresentationLifecycle: [AnyCancellable] = []
    private var seekPresentationBackgrounded = false
    private var seekVideoOutput: AVPlayerItemVideoOutput?
    private var seekOutputWakeup: SeekOutputWakeup?
    // MARK: the second pipeline
    //
    //  player          -- authoritative, audible, on the layer --> viewer
    //  preparedPlayer  -- muted, retained staging layer ------->  nothing
    //
    // The incumbent stays authoritative until the moment of commit. The
    // successor is muted on its own attached hidden layer; audible ownership
    // moves only after aligned decoded media is ready for exposure.
    /// The muted second `AVPlayer` with its retained staging layer. Nil whenever no staging is
    /// live, and every exit sets it back to nil — dismiss, background, session
    /// end, seek, another quality change. On tvOS a leaked second player
    /// survives screensaver activation.
    private var preparedPlayer: AVPlayer?
    /// The successor's item stays in its own player through promotion.
    private var preparedItem: AVPlayerItem?
    /// Item-specific decoded alignment proof retained across warm promotion.
    private var preparedVideoOutput: AVPlayerItemVideoOutput?
    private var preparedMonitor: Task<Void, Never>?
    private var preparedLifecycle: [AnyCancellable] = []
    /// The film position the successor was primed to, and the boundary the
    /// commit's first qualifying frame has to reach or pass.
    private var preparedAcquisitionFilmPositionMs = 0
    private var preparedAcquisitionAnchors: PreparedAcquisitionAnchors?
    /// The item-local seek the successor still owes. An `AVPlayerItem` one
    /// statement old has status `.unknown` and silently drops a seek, which
    /// would leave the successor primed to its own zero and commit a stream
    /// that replays film the viewer has already watched. Nil once it has been
    /// issued, which is also what lets the readiness monitor move on.
    private var preparedSeekMs: Int?
    /// What the commit's alignment seek came back with, and which alignment it
    /// belongs to. Written by the seek's own task and read by the bounded wait
    /// beside it; both are `@MainActor`, so a pair of stored properties is the
    /// whole bridge. The generation is what stops an abandoned seek — one this
    /// commit gave up on — reporting into a later one.
    private var preparedAlignmentOutcome: Bool?
    private var preparedAlignmentGeneration = 0
    private var preparedFrameBudget: PreparedActiveWallBudget?
    /// One attachment-owned inspector; its immutable interval proof is reused
    /// only for that item, player, viewer epoch and prepared action.
    private var preparedCadenceTask: Task<Void, Never>?
    private var preparedCadenceProof: PreparedDecodedCadence?
    private var preparedCadenceOwner: (item: ObjectIdentifier, player: ObjectIdentifier, epoch: Int, action: String)?
    private var preparedCadenceFailed = false
    private var preparedCadenceGeneration = 0
    private var preparedIncumbentBufferLease: (item: AVPlayerItem, player: AVPlayer,
        epoch: Int, actionId: String, policy: PreparedIncumbentBufferPreference)?


    /// One physical deadline from pipeline construction through exposure.
    private var preparedOverlapStartedAtMs: Int?
    private var preparedOverlapRemainingMs: Int {
        guard let began = preparedOverlapStartedAtMs else { return 0 }
        return max(0, PreparedReplacementBounds.overlapMs
            - max(0, Int(ProcessInfo.processInfo.systemUptime * 1_000) - began))
    }
    /// How long the viewer's picture was interrupted the last time a prepared
    /// handoff fell back, in milliseconds. Nil until one does. Published so
    /// the developer surfaces can show the number that is currently missing
    /// from every Apple record of this capability.
    @Published private(set) var preparedFallbackInterruptionMs: Int?
    /// The whole client-side transaction: which staging is live, what is owed
    /// about it, and whether an offered one is a replay. `lazy` because it
    /// holds this controller as its host.
    private(set) lazy var preparedReplacement = PreparedReplacementCoordinator(
        host: self, now: PlaybackControlSession.monotonicMs)
    /// A completed media-segment transaction, never an access-log average.
    struct AutoCompletedTransfer: Sendable {
        let bodyBytes: Int64
        let bodyDurationSeconds: Double?
        let completedAtMs: Int
        let origin: String?
        let networkLoad: Bool
        let fromLocalCache: Bool
        let producerPaced: Bool?
        let statusCode: Int?
        let segmentId: String
        let mediaDurationSeconds: Double?
        var receipt: String? = nil
        var etag: String? = nil
        var installedSessionId: String? = nil
        var installedCandidateId: String? = nil
        var observedMediaDurationMs: Int? = nil
        var stageScope: UUID? = nil
        func ageMs(nowMs: Int) -> Int { nowMs >= completedAtMs ? nowMs - completedAtMs : Int.max }
    }
    private(set) var latestAutoCompletedTransfer: AutoCompletedTransfer?
    private var autoTransferMetricTask: Task<Void, Never>?
    private var autoStagedTransferMetricTask: Task<Void, Never>?
    private var autoStagedTransfers: [AutoCompletedTransfer] = []
    private struct AutoStagedObservation {
        let scope: UUID
        let item: ObjectIdentifier
        let pipeline: ObjectIdentifier
        let sessionId: String
        let candidateId: String
        let recipeDigest: [UInt8]
        let credentialOrigin: String
        let credentialToken: String?
        let startedAtMs: Int
        let deadlineMs: Int
        var playlistURL: URL?
        var intervals: [String: AutoVODAdvertisedInterval]?
    }
    private var autoStagedObservation: AutoStagedObservation?
    private var autoStagedPlaylistTask: Task<Void, Never>?
    private var autoLinkClaims: [String: (completedAtMs: Int, negative: Bool)] = [:]

    private func autoTransferOriginCurrent(_ sample: AutoCompletedTransfer) -> Bool {
        guard let origin = sample.origin,
              let actual = Session.canonicalOrigin(origin),
              let primary = Session.shared.canonicalPrimaryOrigin else { return false }
        return actual == primary
    }

    private func currentLinkReceipt() -> String? {
        let now = PlaybackControlSession.monotonicMs()
        guard model?.displayAwareAuto == true, selectedHeight == nil, !selectedQualityIsOriginal,
              let sample = latestAutoCompletedTransfer,
              let sessionId, sample.installedSessionId == sessionId,
              let candidateId = autoActiveCandidateId, sample.installedCandidateId == candidateId,
              sample.statusCode == 200, sample.networkLoad, !sample.fromLocalCache,
              autoTransferOriginCurrent(sample),
              sample.producerPaced == false, sample.ageMs(nowMs: now) <= 15_000,
              let duration = sample.bodyDurationSeconds, duration > 0, duration <= 120,
              sample.bodyBytes > 0, let receipt = sample.receipt,
              UUID(uuidString: receipt)?.uuidString.lowercased() == receipt,
              let claim = autoLinkClaims[receipt], claim.completedAtMs == sample.completedAtMs,
              let etag = sample.etag, !etag.isEmpty,
              let url = URL(string: sample.segmentId), url.pathComponents.contains(sessionId)
        else { return nil }
        return receipt
    }

    private func retireAutoTransferMetrics() {
        autoTransferMetricTask?.cancel()
        autoTransferMetricTask = nil
        latestAutoCompletedTransfer = nil
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        autoMildSamples = 0
        autoLinkClaims.removeAll()
    }

    private func installAutoTransferMetrics(for item: AVPlayerItem, staged: Bool = false,
                                           stagedAction: PreparedReplacementAction? = nil) {
        if staged {
            autoStagedTransferMetricTask?.cancel()
            autoStagedTransfers.removeAll()
        } else { retireAutoTransferMetrics() }
        guard #available(iOS 18, tvOS 18, *) else { return }
        let installedSessionId = sessionId
        let installedCandidateId = autoActiveCandidateId
        let stagedObservation = staged ? autoStagedObservation : nil
        let task = Task { @MainActor [weak self, weak item] in
            guard let item else { return }
            do {
                for try await event in item.metrics(forType: AVMetricHLSMediaSegmentRequestEvent.self) {
                    guard let self, !Task.isCancelled, (staged ? self.preparedItem === item : self.player.currentItem === item) else { return }
                    guard !event.isMapSegment, let resource = event.mediaResourceRequestEvent,
                          resource.errorEvent == nil,
                          !staged || (event.byteRange.location == 0 && event.byteRange.length == 0) else { continue }
                    if staged, let observation = stagedObservation {
                        self.beginAutoStagedPlaylistLookup(event.indexFileURL, observation: observation, item: item)
                    }
                    // Only an actual completed transaction supplies a body span.
                    // Missing metrics, caches, errors and unknown pacing never
                    // become an unpaced-link sample by observing a stationary clock.
                    guard let transactions = resource.networkTransactionMetrics?.transactionMetrics else { continue }
                    for transfer in transactions.sorted(by: {
                        ($0.responseEndDate ?? .distantPast) < ($1.responseEndDate ?? .distantPast)
                    }) {
                        guard let end = transfer.responseEndDate,
                              transfer.countOfResponseBodyBytesReceived > 0 else { continue }
                        let age = Date().timeIntervalSince(end)
                        guard age.isFinite, age >= 0, age <= 120 else { continue }
                        let duration = transfer.responseStartDate.map { end.timeIntervalSince($0) }
                            .flatMap { $0.isFinite && $0 > 0 && $0 <= 120 ? $0 : nil }
                        let response = transfer.response as? HTTPURLResponse
                        if staged && response?.expectedContentLength != transfer.countOfResponseBodyBytesReceived { continue }
                        let paced = response?.value(forHTTPHeaderField: "X-Plurx-Producer-Paced")
                        let observedMediaDuration = response?.value(forHTTPHeaderField: "X-Plurx-Link-Media-Duration-Ms")
                            .flatMap { Int($0) }.flatMap { (1...Int(UInt32.max)).contains($0) ? $0 : nil }
                        let now = PlaybackControlSession.monotonicMs()
                        let ageMs = Int(age * 1000)
                        let url = response?.url ?? resource.url
                        let origin = url.flatMap { URLComponents(url: $0, resolvingAgainstBaseURL: false) }
                            .flatMap { components -> String? in
                                guard let scheme = components.scheme, let host = components.host else { return nil }
                                return "\(scheme)://\(host)\(components.port.map { ":\($0)" } ?? "")"
                            }
                        let sample = AutoCompletedTransfer(
                            bodyBytes: transfer.countOfResponseBodyBytesReceived,
                            bodyDurationSeconds: duration,
                            completedAtMs: now >= ageMs ? now - ageMs : 0,
                            origin: origin,
                            networkLoad: transfer.resourceFetchType == .networkLoad,
                            fromLocalCache: resource.wasReadFromCache || transfer.resourceFetchType == .localCache,
                            producerPaced: paced == "0" ? false : (paced == "1" ? true : nil),
                            statusCode: response?.statusCode,
                            segmentId: url?.absoluteString ?? "",
                            mediaDurationSeconds: event.segmentDuration.isFinite && event.segmentDuration > 0
                                ? event.segmentDuration : nil,
                            receipt: response?.value(forHTTPHeaderField: "X-Plurx-Link-Receipt"),
                            etag: response?.value(forHTTPHeaderField: "ETag"),
                            installedSessionId: staged ? stagedObservation?.sessionId : installedSessionId,
                            installedCandidateId: staged ? stagedObservation?.candidateId : installedCandidateId,
                            observedMediaDurationMs: observedMediaDuration,
                            stageScope: stagedObservation?.scope
                        )
                        if staged {
                            guard let observation = stagedObservation,
                                  self.autoStagedObservation?.scope == observation.scope,
                                  self.autoStagedObservationCurrent(observation),
                                  let action = stagedAction else { continue }
                            if self.preparedReplacement.activeAction?.sessionId == action.sessionId {
                                self.reportStagedLinkSample(sample, action: action, item: item)
                            }
                            self.autoStagedTransfers.removeAll { $0.segmentId == sample.segmentId }
                            self.autoStagedTransfers.append(sample)
                            if self.autoStagedTransfers.count > 4 { self.autoStagedTransfers.removeFirst() }
                        } else {
                            self.observeAutoTransferPressure(sample, item: item)
                        }
                    }
                }
            } catch {
                // Metrics are optional observation, never a playback failure.
            }
        }
        if staged { autoStagedTransferMetricTask = task } else { autoTransferMetricTask = task }
    }

    private var currentAutoTransferPressureOwner: AutoTransferPressureOwner? {
        guard let item = player.currentItem, let sessionId, let candidate = autoActiveCandidateId,
              let origin = Session.shared.canonicalPrimaryOrigin else { return nil }
        return AutoTransferPressureOwner(item: ObjectIdentifier(item), attempt: playbackAttemptId,
            viewerEpoch: viewerActionEpoch, session: sessionId, candidate: candidate, origin: origin)
    }

    private var autoTransferPressureOwnerCurrent: Bool {
        currentAutoTransferPressureOwner.map(autoTransferPressure.owns) == true
    }

    private func observeAutoTransferPressure(_ sample: AutoCompletedTransfer, item: AVPlayerItem) {
        guard player.currentItem === item, let owner = currentAutoTransferPressureOwner else { return }
        autoTransferPressure.bind(owner)
        let now = PlaybackControlSession.monotonicMs()
        // Body observation and positive advisory reporting do not depend on a
        // catalog arriving first. Only a measured current candidate classifies
        // pressure, under the same route protocol that can act on it.
        let enabled = model?.displayAwareAuto == true && model?.autoAbr == true &&
            model?.displayAwareAutoProtocol == "route-v1" && decision?.displayAwareAutoProtocol == "route-v1" &&
            autoRouteProtocol == "route-v1" && selectedHeight == nil && !selectedQualityIsOriginal
        let current = enabled ? measuredCostCatalog(decision?.qualityCandidates ?? []).first(where: {
            $0.hasValidIdentity && $0.id == owner.candidate
        }) : nil
        let cost = current.flatMap { autoDownsideCostBps($0, sample: sample, sessionId: owner.session, nowMs: now) }
        guard let evidence = autoTransferPressure.observe(sample, owner: owner,
            canonicalOrigin: sample.origin.flatMap(Session.canonicalOrigin),
            downsideCost: cost, knownPeak: current?.peakBps != nil, nowMs: now) else { return }
        latestAutoCompletedTransfer = sample
        autoMildSamples = autoTransferPressure.mildSamples(nowMs: now)
        reportCandidateLinkSample(negative: false)
        if evidence.urgent { tickDisplayAwareAuto(pressureOnly: true) }
    }

    private func autoStagedObservationCurrent(_ observation: AutoStagedObservation) -> Bool {
        let now = PlaybackControlSession.monotonicMs()
        let credentials = Session.shared.credentials
        return now >= observation.startedAtMs && now < observation.deadlineMs &&
            credentials.origin == observation.credentialOrigin && credentials.token == observation.credentialToken &&
            preparedItem.map(ObjectIdentifier.init) == observation.item &&
            preparedPlayer.map(ObjectIdentifier.init) == observation.pipeline &&
            autoDesiredCandidate?.id == observation.candidateId &&
            autoDesiredCandidate?.recipeDigest == observation.recipeDigest &&
            preparedReplacement.activeAction?.sessionId == observation.sessionId &&
            preparedReplacement.activeAction?.effectiveSelection.candidateId == observation.candidateId
    }

    /// One actual media-playlist lookup per stage, never a cadence or a renewed trial.
    private func beginAutoStagedPlaylistLookup(_ url: URL, observation: AutoStagedObservation, item: AVPlayerItem) {
        guard autoStagedObservation?.scope == observation.scope,
              autoStagedObservation?.playlistURL == nil, autoStagedObservationCurrent(observation),
              Session.canonicalOrigin(url.absoluteString) == Session.shared.canonicalPrimaryOrigin,
              url.pathComponents.contains(observation.sessionId), url.lastPathComponent.hasSuffix(".m3u8") else { return }
        autoStagedObservation?.playlistURL = url
        let remaining = observation.deadlineMs - PlaybackControlSession.monotonicMs()
        guard remaining > 0 else { return }
        var request = URLRequest(url: url, cachePolicy: .reloadIgnoringLocalCacheData,
                                 timeoutInterval: Double(remaining) / 1000)
        if let token = observation.credentialToken {
            request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        autoStagedPlaylistTask = Task { @MainActor [weak self, weak item] in
            let configuration = URLSessionConfiguration.ephemeral
            configuration.timeoutIntervalForResource = Double(remaining) / 1000
            configuration.urlCache = nil
            let loader = URLSession(configuration: configuration, delegate: AutoVODPlaylistRedirectRefusal(), delegateQueue: nil)
            defer { loader.invalidateAndCancel() }
            do {
                let (bytes, response) = try await loader.bytes(for: request)
                guard let response = response as? HTTPURLResponse, response.statusCode == 200,
                      response.url == url, response.expectedContentLength <= 1_048_576 else { return }
                var body = Data()
                for try await byte in bytes {
                    guard !Task.isCancelled, body.count < 1_048_576,
                          PlaybackControlSession.monotonicMs() < observation.deadlineMs else { return }
                    body.append(byte)
                }
                guard response.expectedContentLength < 0 || response.expectedContentLength == body.count,
                      let self, let item, self.preparedItem === item,
                      self.autoStagedObservation?.scope == observation.scope,
                      self.autoStagedObservationCurrent(observation),
                      let intervals = autoVODAdvertisedIntervals(body) else { return }
                self.autoStagedObservation?.intervals = intervals
            } catch { /* Missing optional proof keeps the healthy incumbent. */ }
        }
    }

    private func reportCandidateLinkSample(negative: Bool) {
        let now = PlaybackControlSession.monotonicMs()
        autoLinkClaims = autoLinkClaims.filter { now >= $0.value.completedAtMs && now - $0.value.completedAtMs <= 15_000 }
        guard model?.displayAwareAuto == true, selectedHeight == nil, !selectedQualityIsOriginal,
              let sample = latestAutoCompletedTransfer,
              let sessionId, sample.installedSessionId == sessionId,
              let candidateId = autoActiveCandidateId, sample.installedCandidateId == candidateId,
              let runway = bufferedRunwaySeconds(), runway.isFinite, runway >= 0,
              sample.statusCode == 200, sample.networkLoad, !sample.fromLocalCache,
              autoTransferOriginCurrent(sample),
              sample.producerPaced == false, sample.ageMs(nowMs: now) <= 15_000,
              let duration = sample.bodyDurationSeconds, duration > 0, duration <= 120,
              sample.bodyBytes > 0, let receipt = sample.receipt, receipt.count == 36,
              let etag = sample.etag, !etag.isEmpty,
              let url = URL(string: sample.segmentId), url.pathComponents.contains(sessionId),
              url.lastPathComponent.range(of: "^seg[0-9]+\\.(m4s|ts)$", options: .regularExpression) != nil
        else { return }
        if negative {
            guard autoLinkClaims[receipt]?.negative == false, wantsPlayback, surface.presenting,
                  seekState.pendingMs == nil, preparedPlayer == nil,
                  player.timeControlStatus == .waitingToPlayAtSpecifiedRate,
                  let media = sample.observedMediaDurationMs, duration * 1000 > Double(media),
                  runway <= 1.5 else { return }
        } else if autoLinkClaims[receipt] != nil { return }
        if autoLinkClaims.count >= 32, autoLinkClaims[receipt] == nil { return }
        postClientLog(AppleCandidateLinkLog(session_id: sessionId, link_sample: .init(
            receipt: receipt, object_name: url.lastPathComponent, etag: etag,
            body_bytes: sample.bodyBytes, body_duration_ms: Int((duration * 1000).rounded()),
            age_ms: sample.ageMs(nowMs: now), negative: negative,
            media_duration_ms: sample.observedMediaDurationMs,
            presenting: surface.presenting, stalled: player.timeControlStatus == .waitingToPlayAtSpecifiedRate,
            runway_ms: Int(min(runway * 1000, Double(UInt32.max))))))
        autoLinkClaims[receipt] = (sample.completedAtMs, negative)
    }

    private func negativeLinkPayload() -> AppleCandidateLinkLog? {
        let now = PlaybackControlSession.monotonicMs()
        guard let receipt = currentLinkReceipt(), let sample = latestAutoCompletedTransfer,
              let sessionId, let duration = sample.bodyDurationSeconds,
              let media = sample.observedMediaDurationMs, duration * 1000 > Double(media),
              let etag = sample.etag, let url = URL(string: sample.segmentId),
              autoLinkClaims[receipt]?.negative == false, wantsPlayback, surface.presenting,
              seekState.pendingMs == nil, preparedPlayer == nil,
              player.timeControlStatus == .waitingToPlayAtSpecifiedRate,
              let runway = bufferedRunwaySeconds(), runway.isFinite, runway >= 0, runway <= 1.5
        else { return nil }
        return AppleCandidateLinkLog(session_id: sessionId, link_sample: .init(
            receipt: receipt, object_name: url.lastPathComponent, etag: etag,
            body_bytes: sample.bodyBytes, body_duration_ms: Int((duration * 1000).rounded()),
            age_ms: sample.ageMs(nowMs: now), negative: true, media_duration_ms: media,
            presenting: true, stalled: true, runway_ms: Int(runway * 1000)))
    }

    private func reportAutoRecoveryCause(_ cause: AutoRecoveryCause) {
        guard let sessionId, let current = decision?.qualityCandidates?.first(where: {
            $0.hasValidIdentity && $0.id == autoActiveCandidateId
        }) else { return }
        postClientLog(AppleCandidateRecoveryLog(session_id: sessionId, candidate_recovery: .init(
            cause: cause.rawValue, candidate_id: current.id, recipe_digest: current.recipeDigest,
            decoder_failed: false, rendered_elapsed_ms: 0, position_progress_ms: 0,
            dropped_frames: 0, runway_ms: Int(max(0, min(bufferedRunwaySeconds() ?? 0, 120)) * 1000))))
    }

    private func acknowledgeNegativeLink(_ payload: AppleCandidateLinkLog, receipt: String, deadline: Double) async -> Bool {
        guard payload.link_sample.negative, payload.link_sample.receipt == receipt,
              let body = try? JSONEncoder().encode(payload) else { return false }
        return await acknowledgeClientEvidence(body, receipt: receipt, header: "X-Plurx-Link-Accepted", deadline: deadline)
    }

    private func acknowledgeClientEvidence(_ body: Data, receipt: String, header: String, deadline: Double) async -> Bool {
        let remaining = (deadline - ProcessInfo.processInfo.systemUptime) * 1000
        guard remaining.isFinite, remaining >= 1,
              let url = Session.shared.url("/api/v1/client-log") else { return false }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        Session.shared.authorize(&request)
        let frozenRequest = request
        let waitMs = Int(min(remaining, 250))
        return await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                guard let (_, response) = try? await URLSession.shared.data(for: frozenRequest),
                      let response = response as? HTTPURLResponse,
                      ProcessInfo.processInfo.systemUptime < deadline,
                      autoNegativeLinkAcknowledgement(receipt: receipt,
                        values: response.value(forHTTPHeaderField: header).map { [$0] } ?? [],
                        status: response.statusCode, sameEndpoint: response.url == url, remainingMs: waitMs)
                else { return false }
                return true
            }
            group.addTask {
                try? await Task.sleep(for: .milliseconds(waitMs))
                return false
            }
            let accepted = await group.next() ?? false
            group.cancelAll()
            return accepted
        }
    }

    private var itemObserver: AVPlayerItemObserver?
    private var itemEventTask: Task<Void, Never>?
    private var lastItemFailureDetail: (item: AVPlayerItem, detail: PlayerItemFailure.Detail)?
    private var statusTask: Task<Void, Never>?
    private var recoveryTask: Task<Void, Never>?
    private let audioSessionObserver = PlaybackAudioSessionObserver()
    /// Interruption state, deliberately NOT `@Published`. Every transition is
    /// already announced through the one surface as `present(.systemPaused(_))`,
    /// and nothing outside this controller observes the flag — the only reads
    /// are here and they are plain conditionals. Publishing it would make the
    /// player carry a second surface for a fact the first one already states,
    /// which is exactly what the pinned published-field inventory in
    /// `tests/operations/test_playback_surface_fence.py` exists to refuse.
    private(set) var systemPaused = false
    /// When the current system hold began, so a Play that ends it is a resume
    /// after a pause of the hold's real length.
    private var systemPauseBeganAt: TimeInterval?
    /// Feeds the SURFACE section of the Playback debug ledger and the
    /// `surface_*` client-log events. Fed from the presenter's log entries,
    /// which is the only half of a fault's life the presenter can know.
    private(set) var surfaceHistory = PlaybackSurfaceHistory()
    private var surfaceEpoch = ContinuousClock.now
    private var surfaceLifecycleObservation: [AnyCancellable] = []
    /// The presenter's own clock (see `startSurfaceClock`).
    private var surfaceClockTask: Task<Void, Never>?
    /// The viewer request the in-flight open's `client_preparing` fault was
    /// raised with, so the settle names the request the raise named even after
    /// the viewer has moved the epoch on.
    private var streamChangePreparingIntent: Int?
    /// Which row-18 facts this playback has already logged, so a reporter that
    /// fails every cadence is one `surface_log_only` and not thirty.
    private var surfaceLogOnlyReasons: Set<String> = []
    /// The last position the surface's evidence sampler saw, so "the clock
    /// moved" is a delta rather than a guess.
    private var lastSurfaceSampleMs: Int?
    /// When the sampler last ran, on the controller's monotonic clock, so the
    /// presenter's clock knows whether AVPlayer's observer has gone quiet.
    private var lastSurfaceSampleAtMs: Int?
    /// The create-retry sequence in flight (M5). `createRetryEpoch` names it;
    /// `createRetryExpiredEpoch` is the epoch whose absolute deadline has
    /// passed, rather than a shared flag two sequences could write.
    ///
    /// Both halves are keyed on the epoch because they have to be. Two
    /// sequences overlap by the obvious route — the watchdog raises
    /// `exhausted` at sixty seconds, the viewer taps Try again, and the new
    /// open starts while the old create is still in flight — and a
    /// controller-wide `Bool` let the newer one clear the older one's expiry,
    /// while an unconditional retirement of the counter let the older one's
    /// exit leave the newer one with no watchdog at all.
    private var createRetryEpoch = 0
    private var createRetryExpiredEpoch: Int?
    /// Whether this playback has ever presented. It is the whole of the
    /// difference between the `start` and `attached` contexts.
    private var surfaceHasPresented = false
    /// Declared by AVKit, never inferred: a PiP window is presentation this
    /// controller cannot see in `presentationSize`.
    private var pictureInPictureIsActive = false
    private var diagnosticAuthorizationGeneration: UInt64?
    var diagnosticTitle: String? {
        diagnosticAuthorizationGeneration == Session.shared.playbackAuthorization.generation && started ? title : nil
    }
    private func recordDiagnosticPlayback(_ kind: String = "sample", action: String? = nil) {
        guard let generation = diagnosticAuthorizationGeneration, generation == Session.shared.playbackAuthorization.generation, started else { return }
        var facts: [String: DiagnosticFact] = ["state": .text(surface.presenting ? "playing" : (wantsPlayback ? "waiting" : "paused")),
            "playhead_ms": .number(UInt64(min(604_800_000, max(0, currentMs)))), "mode": .text(["direct","remux","transcode"].contains(clientLogMethod) ? clientLogMethod : "unknown"),
            "source_codec": .text(Self.diagnosticCodec(decision?.source?.videoCodec)), "delivered_codec": .text("unknown"), "render_codec": .text("unknown")]
        if let action { facts["action"] = .text(action) }
        if let runway = bufferedRunwaySeconds(), runway.isFinite { facts["headroom_ms"] = .number(UInt64(min(604_800_000, max(0, runway * 1000)))) }
        if let item = player.currentItem, let sample = accessLogSample(for: item), sample.values.numberOfDroppedVideoFrames >= 0 { facts["dropped_frames"] = .text(String(sample.values.numberOfDroppedVideoFrames)) }
        if let sessionId, sessionId.utf8.count <= 256 { facts["correlation"] = .text("s-" + DiagnosticRecorder.digest(Data(sessionId.utf8))) }
        DiagnosticRecorder.shared.record("playback", kind, facts, generation: generation)
    }
    nonisolated private static func diagnosticCodec(_ text: String?) -> String {
        let value = text?.lowercased() ?? "unknown"
        if ["h264","hevc","av1","vp9","aac","ac3","eac3","dts","truehd","opus","flac","pcm"].contains(value) { return value }
        if value == "h265" { return "hevc" }; return value == "unknown" ? value : "other"
    }
    private var started = false
    private(set) var loadingTask: Task<Void, Never>?
    /// A title owns its decision and all subsequent asynchronous work. A new
    /// title may reuse this controller while an old request is still waking.
    private var lifecycleGeneration = 0
    /// This player's passive control reporting. Nothing about playback
    /// depends on it: a server that offers no bootstrap leaves it silent.
    private let playbackControl = PlaybackControlSession()
    /// When AVPlayer began waiting for data, or nil while it is not waiting.
    /// The protocol separates waiting from stalled by how long, and AVPlayer
    /// reports only that it is waiting.
    private var controlWaitingSince: Date?
    /// Evidence a recovery owner is about to act on, published for the next
    /// exchange. It is set immediately before the owner reports and cleared
    /// when the player renders again, so the server sees why this client
    /// thought it was stuck rather than only that it was.
    private var controlObservationOverride: ClientObservation?
    private var controlRenderOverride: RenderState?
    /// The film position when the override was published, so progress past it
    /// can expire the override without trusting `timeControlStatus`.
    private var controlEvidencePositionMs: Int?
    private var sessionId: String?
    private var activeMediaPath: String?
    private var activeMediaAuthenticated = false
    /// The native subtitle selection the *current item* was opened with. When
    /// the viewer's choice is burned into the transcode this is nil, and a
    /// failover that re-applied the viewer-facing selection instead would
    /// select the source's text track on top of the burned-in one.
    private var activeNativeSubtitle: Int?
    private var lastReportedMs = 0
    private var usesDirectTimeline = false
    private var canRetryCurrentItemWithHDRBase = false
    private var dolbyVisionFallbackAttempted = false
    private var forceCompatibleHDRBase = false
    private var canRetryCurrentItemWithTranscode = false
    private var compatibilityFallbackAttempted = false
    private var forceCompatibilityTranscode = false
    /// Once real playback has begun, a transport interruption is not evidence
    /// that the device rejected HDR. Rebuild the same delivery once instead of
    /// silently replacing a picture the viewer has already watched with SDR.
    private var attachmentRecovery = PlayerAttachmentRecoveryState()
    /// The only evidence a decoder is producing nothing while the film clock
    /// runs. Scoped to one attached item, exactly like `attachmentRecovery`.
    private var blackFrameWatchdog = BlackFrameWatchdog()
    private var establishedHDRRetryAttempted = false
    private var ttffMeasurement = ApplePlaybackTTFFState()
    private var seekMeasurement = ApplePlaybackSeekMeasurement()
    private var requestedSeekGeneration: Int?
    private var ttffReason = "cold-start"
    private var diagnosticProbesEnabled = false
    private var stallObservation = PlaybackStallObservationState()
    /// An item that ends before its expected boundary gets one reopen. A
    /// second end at the same position finishes an otherwise uncorroborated
    /// local item, or stops a growing/known-duration stream visibly, instead
    /// of creating an unbounded session loop.
    private(set) var lastUncorroboratedEndMs: Int?
    /// A burn is part of the current video frames. Leaving one requires one
    /// reopen; native-to-native and native-to-Off never do.
    private var activeBurnedSubtitle: Int?
    /// Read once, in `start`, rather than per open: a viewer who changes the
    /// setting from another device or another tab of Settings must not have the
    /// stream rebuilt under them mid-film. The choice a title started with is
    /// the choice it finishes with.
    private var subtitleReadiness: SubtitleReadiness = .onDemand
    /// Read once, in `start`, for the same reason: the Settings preference is
    /// the rung a title *starts* on. It shapes the `/decision` request and
    /// resolves to `selectedHeight` once the ladder is known; the player's
    /// own quality menu owns every change after that.
    private var startingQuality: PlaybackQuality { initialDecisionRequest.quality }
    /// Sticky for this playback. Once a native text track has been asked for —
    /// by automatic selection at cold start, or by the viewer — the stream keeps
    /// its subtitle renditions, including after subtitles are turned off again:
    /// dropping back to direct play would be a second restart nobody asked for,
    /// and the next selection would only have to pay for a third.
    private var wantsNativeSubtitleRenditions = false
    /// True while the player is on the raw file instead of an HLS session.
    /// P2-7: the first native subtitle selection has to create the session.
    private var isDirectPlayback = false
    /// Holds the newest seek/track intent that arrived mid-change so it wins
    /// instead of vanishing.
    private var reopenQueue = PlayerReopenQueue()
    private(set) var recipeRevision = PlayerRecipeRevision()
    /// Optimistic absolute film position for an interactive seek. It is also
    /// the base for the next relative press until the newest seek lands.
    private var seekState = PlayerSeekState()
    /// Production-linked read seam for intent/selection regression tests.
    var pendingPlaybackIntentForTesting: (targetMs: Int?, generation: Int) {
        (seekState.pendingMs, seekState.generation)
    }
    var pendingTransportTasksForTesting: (publication: Task<Void, Never>?, seek: Task<Void, Never>?) {
        (resumeIntentTask, interactiveSeekTask)
    }
    var resumeOwnershipForTesting: (
        id: String,
        publicationCompleted: Bool,
        repairAdmitted: Bool,
        expiresAt: TimeInterval
    )? {
        resumeAttempt.map {
            ($0.id, $0.publicationCompleted, $0.repairAdmitted, $0.expiresAt)
        }
    }
    var openGenerationForTesting: Int { openGeneration }
    /// The `detail` of the last `attempt_stale` event this controller raised,
    /// or `nil` if no migrated fence has refused a continuation yet. Only
    /// ever the latest one, so it holds nothing a long session accumulates.
    private(set) var lastAttemptStaleDetail: String?
    /// Production-linked overlay seams. Only the decision's track list and the
    /// selection publication are supplied; the selection, window load, tick,
    /// seek and failure paths are the production ones.
    func selectPGSOverlayForTesting(_ index: Int?, tracks: [SubtitleTrack], atMs position: Int) {
        currentMs = position
        selectedSubtitle = index
        updatePGSOverlaySelection(index, tracks: tracks)
    }
    /// The viewer's pick is published before the overlay hears of it.
    func publishSelectedSubtitleForTesting(_ index: Int?) { selectedSubtitle = index }
    var pgsOverlayLoadingRangeForTesting: Range<Int>? { pgsOverlayLoadingRange }
    /// Every explicit viewer command invalidates recovery work that crossed an
    /// await. Session generation alone cannot see pause/resume or native seek.
    private var viewerActionEpoch = 0

    /// Read live so the Developer switch takes effect without rebuilding or
    /// relaunching. Readiness rows never participate in this answer.
    private var boundedResumeEnabled: Bool { SettingsStore().boundedResumeEnabled }

    /// M3. What the last prepared switch measured.
    ///
    /// Observation only. Every input is a read of an access log AVFoundation
    /// already writes, taken on the status poll this controller already runs;
    /// nothing here is awaited on the commit path and nothing in this
    /// controller reads a value back to decide anything, so instrumenting the
    /// switch cannot be the thing that changes it. The arithmetic lives in
    /// `PreparedSwitchMeasurement.swift` as pure functions.
    private var preparedSwitch = PreparedSwitchMeasurement.Recorder()

    /// The three rows the info panel shows, computed on demand.
    var preparedSwitchReading: PreparedSwitchMeasurement.Reading { preparedSwitch.reading() }

    /// Consume each real worker sample at most once, without reading a native
    /// access log on the main actor. Host time, film position and eligibility
    /// describe this poll together; the shared cache separately bounds age.
    private func sampleThePreparedSwitch() {
        guard let item = player.currentItem, let sample = accessLogSample(for: item),
              lastPreparedAccessLogSequence != sample.sequence else { return }
        lastPreparedAccessLogSequence = sample.sequence
        let event = sample.values
        let observedAtMs = PlaybackControlSession.monotonicMs()
        let decodePressure = autoDecodePressure.observe(
            attachment: ObjectIdentifier(item), candidate: autoActiveCandidateId,
            nowMs: observedAtMs, positionMs: realPositionMs(),
            cumulativeDropped: event.numberOfDroppedVideoFrames,
            eligible: wantsPlayback && surface.presenting && player.rate > 0 &&
                seekState.pendingMs == nil && !isChangingStream &&
                (bufferedRunwaySeconds() ?? 0) >= 10)
        if decodePressure {
            let pressureAttempt = snapshotAttempt()
            let position = realPositionMs()
            let evidence = autoDecodePressure.evidence
            Task { @MainActor [weak self] in
                guard let self, self.attemptStillCurrent(pressureAttempt, fence: .preparedPressureEntry),
                      !self.isChangingStream else { return }
                guard await self.noteAutoDecodeFailure(pressure: evidence),
                      self.attemptStillCurrent(pressureAttempt, fence: .preparedPressureAcknowledgement) else { return }
                await self.reopen(at: position, intent: .sameDeliveryRepair)
            }
        }
        preparedSwitch.note(
            atMs: observedAtMs,
            droppedFrames: event.numberOfDroppedVideoFrames >= 0
                ? event.numberOfDroppedVideoFrames : nil,
            stalls: event.numberOfStalls >= 0 ? event.numberOfStalls : nil
        )
    }
    /// Request sequence already published by a predecessor for the seek that a
    /// replacement is about to attach. It lives above control-session lifetime.
    private var pendingControlSequence: UInt64?
    private var playbackRecoveryMonitor = PlaybackRecoveryMonitor()
    /// A clock freeze without an AVPlayerItem failure is not proof that the
    /// decoder rejected the stream. The same is true of a sustained buffering
    /// wait. Reconnect the exact delivery once; if the replacement freezes too,
    /// stop visibly instead of walking the HDR/SDR compatibility ladder on a
    /// guess.
    private var sameDeliveryStallRecovery = SameDeliveryStallRecoveryState()
    /// The floor bound the server contract leaves to the client. Independent of
    /// `sameDeliveryStallRecovery`, which five seconds of recovered playback
    /// resets — a link starved at the floor stalls, reopens, plays just long
    /// enough to rearm that state, and stalls again, which is exactly the
    /// unbounded loop this counts.
    private var stallReopenBudget = StallReopenBudget()
    /// The normalized output height of the session currently playing, as the
    /// server resolved it. Nil for direct play, and for a server that predates
    /// the field. Read only to decide whether a stall reopen actually bought a
    /// lower rung.
    private var sessionHeight: Int?
    private var deliveryStarvation = DeliveryStarvationDetector()
    /// One advisory control hold, with an absolute deadline measured from the
    /// stall's first stagnant sample. Control may explain a pause; it may not
    /// own a frozen picture indefinitely.
    private var deferredStall: (event: PlaybackStallEvent, deadline: TimeInterval)?
    private var recoveryReopenBudget = RecoveryReopenBudget()
    /// Identifies the newest `open()`. An older attempt that wakes from its
    /// awaits afterwards must not replace the item, clear the transition
    /// state, or report its own failure over the newer one's (P2-6).
    private var openGeneration = 0
    /// The transport the viewer asked for, which is not what AVPlayer reports
    /// while it buffers or after an item fails. Reopens restore this.
    ///
    /// It is also the presenter's `playback_requested` (contract §3.1, ruled
    /// 2026-09-13): a `buffering` fault is about a player that WANTS media, so
    /// a viewer who pauses makes it about nothing.
    ///
    /// Two writers, and the split is the honest statement of what happens —
    /// the earlier claim that the owner could never move this was simply not
    /// true, because five terminal stops did. The viewer's own pause and play
    /// is `togglePlayPause`. The owner's is
    /// `stopForBlockingSurface(revokingPlaybackIntent:)`, the single stop
    /// helper §3.4 obliges, and it moves this only on the stops that genuinely
    /// have nothing left to want. When it does, the presenter is told, and
    /// that is right rather than incidental: a wait standing under a prompt
    /// the owner has just stopped the player for is about nothing either.
    /// What cannot happen is the reverse — a stop that leaves the intent alone
    /// quietly retiring a fault — because the flag IS the event. No new
    /// detector and no new timer.
    private(set) var wantsPlayback = true {
        didSet {
            guard wantsPlayback != oldValue else { return }
            if !wantsPlayback { restorePreparedIncumbentBufferPreference() }
            preparedFrameBudget?.update(
                nowMs: Int(ProcessInfo.processInfo.systemUptime * 1_000),
                playbackRequested: wantsPlayback
            )
            autoTransportRevision = min(autoTransportRevision + 1, 9_007_199_254_740_991)
            applyWarmPredecessorTransport()
            present(.playbackRequested(wantsPlayback))
        }
    }
    /// The last rate the player was genuinely playing at, so a viewer paused
    /// at 1.5× resumes at 1.5× rather than at the 0 the transport reports
    /// while paused (P2-5).
    private var preferredRate: Float = 1 {
        didSet {
            if preferredRate != oldValue {
                restorePreparedIncumbentBufferPreference()
                applyWarmPredecessorTransport()
                autoTransportRevision = min(autoTransportRevision + 1, 9_007_199_254_740_991)
                updateNowPlaying()
            }
        }
    }
    /// The viewer's audio language, kept because this controller now performs
    /// media selection itself instead of leaving it to AVPlayer criteria.
    private var audioLanguage = "eng"
    /// Stable for this player instance. Server-side supersession uses it to
    /// replace this player's own stream without touching another device.
    private let playbackId = UUID().uuidString
    /// Fresh for every attached AVPlayerItem. `playbackId` cannot serve this
    /// purpose because it deliberately stays stable across session reopens;
    /// telemetry needs the opposite so the stalled predecessor and recovered
    /// successor never collapse into one attempt.
    private var playbackAttemptId = UUID().uuidString
    private var lastMarkerSkipEndMs: Int?
    private var markerOfferLedger = MarkerOfferLedger()
    private var markerAutoSkipLedger = MarkerAutoSkipLedger()
    private var pgsOverlayTrackIndex: Int?
    private var pgsOverlayManifest: PGSOverlayManifest?
    private var pgsOverlayPrepareTask: Task<Void, Never>?
    private var pgsOverlayWindowTask: Task<Void, Never>?
    /// The source range the in-flight window load covers, so a periodic tick
    /// cannot cancel the very load that will cover its position.
    private var pgsOverlayLoadingRange: Range<Int>?
    /// Which window load is current; an older load's outcome is not news.
    private var pgsOverlayLoadGeneration = 0
    /// Consecutive failed window loads in this failure episode. The first
    /// raises the notice; ticks then retry silently on
    /// `PGSOverlayPolicy.windowRetryDelaysMs` and stop when those are spent.
    /// A success, a seek, a reselection or a new item ends the episode.
    private var pgsOverlayWindowFailures = 0
    private var pgsOverlayLastFailureMs = 0
    /// Monotonic milliseconds for the failure backoff. XCTest replaces it.
    var pgsOverlayClockMs: @MainActor () -> Int = {
        Int(ProcessInfo.processInfo.systemUptime * 1_000)
    }
    /// Overlay fetches. Nil reads through the model, as production does.
    var pgsOverlayFetcherForTesting: PGSOverlayFetcher?
    /// Overlay failure notices this controller raised.
    private(set) var pgsOverlayNoticeCount = 0
    private var pgsOverlaySelectionGeneration = 0
    private var pgsOverlayItemGeneration = 0
    private var pgsOverlayRevision = 0
    private var pgsOverlayImageCache: [String: CGImage] = [:]
    private var pgsOverlayImageBytes: [String: Int] = [:]
    private var pgsOverlayImageLRU: [String] = []

    private var remoteTargets: [(MPRemoteCommand, Any)] = []
    private let remoteOwnerToken = UUID()
    private var lastNowPlayingState: NowPlayingState?

    var subtitles: [SubtitleTrack] { decision?.subtitles ?? [] }
    var audioTracks: [AudioTrack] { decision?.audio ?? [] }
    var qualityRungs: [QualityRung] {
        guard decision?.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1",
              let candidates = decision?.qualityCandidates else { return decision?.ladder ?? [] }
        return manualCatalogHeights(candidates).map { QualityRung(height: $0, totalKbps: 0, peakKbps: 0) }
    }
    var activeMarker: Marker? {
        decision?.markers?.first { currentMs >= $0.startMs && currentMs < $0.endMs }
    }

    /// The marker whose button belongs on screen: the active one, unless the
    /// Skip preference is about to take it — the Android player hides the
    /// button on the same condition. Previews and estimates are never
    /// automatic, so their buttons stay whatever the preference says.
    var offeredMarker: Marker? {
        guard let marker = activeMarker,
              !MarkerAutoSkipLedger.isAutomatic(marker, autoSkip: model?.autoSkip ?? false)
        else { return nil }
        return marker
    }

    var methodLabel: String {
        #if os(iOS)
        if offlineId != nil {
            let quality = selectedHeight.map { " · \($0)p" } ?? ""
            return "Offline\(quality) · H.264 · AAC"
        }
        #endif
        return Self.onlinePlaybackMethodLabel(selectedHeight: selectedHeight,
            subtitleBurned: activeBurnedSubtitle != nil,
            fallbackMode: decision.map(Self.playbackMode) ?? "transcode",
            isVOD: isVOD, pgsOverlay: pgsOverlayIsActive,
            activeCandidateId: autoActiveCandidateId,
            candidates: autoRouteProtocol == "route-v1" ? decision?.qualityCandidates : nil,
            sessionHeight: sessionHeight, isDirectPlayback: isDirectPlayback)
    }

    /// Auto keeps its selection nil while the installed session changes rungs.
    /// Report that session's validated route and height instead of the initial
    /// decision, which may still describe its original remux incumbent.
    nonisolated static func onlinePlaybackMethodLabel(
        selectedHeight: Int?, subtitleBurned: Bool, fallbackMode: String,
        isVOD: Bool, pgsOverlay: Bool, activeCandidateId: String?,
        candidates: [QualityCandidate]?, sessionHeight: Int?, isDirectPlayback: Bool
    ) -> String {
        if subtitleBurned { return "Transcode · subtitle burn-in" }
        if let selectedHeight { return "Transcode · \(selectedHeight)p" }
        let installed = candidates?.first { $0.id == activeCandidateId && $0.hasValidIdentity }
        let mode: String
        switch installed?.route {
        case "encode": mode = "transcode"
        case "remux": mode = "remux"
        case "original": mode = isDirectPlayback ? "direct" : "remux"
        default: mode = fallbackMode
        }
        let overlay = pgsOverlay ? " · PGS overlay" : ""
        if installed?.route == "encode", let sessionHeight, sessionHeight > 0 {
            return "Transcode · \(sessionHeight)p\(overlay)"
        }
        switch mode {
        case "direct": return "Direct play\(overlay)"
        case "remux": return "Remux · HLS\(overlay)"
        default: return (isVOD ? "Transcode · cached" : "Transcode") + overlay
        }
    }

    private var clientLogMethod: String {
        isDirectPlayback ? "direct_play" : (encoder == "copy" ? "remux" : "transcode")
    }

    nonisolated static func isPGSOverlayActive(
        overlayTrackIndex: Int?,
        selectedSubtitle: Int?
    ) -> Bool {
        guard let overlayTrackIndex else { return false }
        return overlayTrackIndex == selectedSubtitle
    }

    var pgsOverlayIsActive: Bool {
        Self.isPGSOverlayActive(
            overlayTrackIndex: pgsOverlayTrackIndex,
            selectedSubtitle: selectedSubtitle
        )
    }

    var observedBitrate: Double? {
        player.currentItem.flatMap { accessLogSample(for: $0)?.values.observedBitrate }
    }

    var indicatedBitrate: Double? {
        player.currentItem.flatMap { accessLogSample(for: $0)?.values.indicatedBitrate }
    }

    var stalls: Int? {
        player.currentItem.flatMap { accessLogSample(for: $0)?.values.numberOfStalls }
    }

    /// A live snapshot for the on-device Debug stats view. This is the same
    /// evidence attached to a stall report, but reading it here has no
    /// telemetry side effect and makes the browser's diagnostic categories
    /// available without connecting a separate debugger.
    var currentDiagnosticSnapshot: ApplePlaybackDiagnosticSnapshot {
        playbackDiagnosticSnapshot(at: currentMs)
    }

    var currentSessionId: String? { sessionId }

    /// Runway of the item currently attached, or `nil` when there is no item
    /// or no usable clock. `nil` means "unknown", which the delivery watchdog
    /// must not read as "healthy".
    func bufferedRunwaySeconds(joinTolerance: Double = 0.25, gapTolerance: Double = 0.25) -> Double? {
        guard let item = player.currentItem else { return nil }
        let playhead = item.currentTime().seconds
        guard playhead.isFinite else { return nil }
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds
            let end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, end >= start else { return nil }
            return start...end
        }
        return Self.bufferedRunwaySeconds(
            playheadSeconds: playhead, ranges: ranges, joinTolerance: joinTolerance, gapTolerance: gapTolerance
        )
    }

    /// Playable seconds contiguous with the current clock. A later buffered
    /// island beyond a gap is not runway: AVPlayer still has to stop before it
    /// can reach it. Playhead membership tolerates timebase rounding separately
    /// from the gaps permitted between buffered ranges.
    nonisolated static func bufferedRunwaySeconds(
        playheadSeconds: Double,
        ranges: [ClosedRange<Double>],
        joinTolerance: Double = 0.25,
        gapTolerance: Double = 0.25
    ) -> Double {
        guard playheadSeconds.isFinite else { return 0 }
        let ranges = ranges
            .filter {
                $0.lowerBound.isFinite
                    && $0.upperBound.isFinite
                    && $0.upperBound >= $0.lowerBound
            }
            .sorted { $0.lowerBound < $1.lowerBound }
        // A tolerated range just behind the clock must not shadow media that
        // actually contains it, especially when internal gaps are forbidden.
        let containing = ranges.firstIndex(where: { $0.contains(playheadSeconds) })
        // If only rounding-tolerant membership is available, prefer the range
        // reaching furthest ahead rather than an already consumed sliver.
        let fallback = ranges.indices.filter {
            ranges[$0].lowerBound <= playheadSeconds + joinTolerance
                && ranges[$0].upperBound >= playheadSeconds - joinTolerance
        }.max { ranges[$0].upperBound < ranges[$1].upperBound }
        guard let start = containing ?? fallback else { return 0 }
        var end = ranges[start].upperBound
        for range in ranges.dropFirst(start + 1) {
            guard range.lowerBound <= end + gapTolerance else { break }
            end = max(end, range.upperBound)
        }
        return max(0, end - playheadSeconds)
    }

    private func playbackDiagnosticSnapshot(at positionMs: Int) -> ApplePlaybackDiagnosticSnapshot {
        var snapshot = ApplePlaybackDiagnosticSnapshot()
        snapshot.positionMs = max(0, positionMs)
        snapshot.timeControlStatus = Self.timeControlStatusLabel(player.timeControlStatus)
        snapshot.waitingReason = player.reasonForWaitingToPlay?.rawValue
        if let item = player.currentItem {
            let localSeconds = item.currentTime().seconds
            let loaded = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
                let range = value.timeRangeValue
                let start = range.start.seconds
                let end = CMTimeRangeGetEnd(range).seconds
                guard start.isFinite, end.isFinite, end >= start else { return nil }
                return start...end
            }
            snapshot.runway = Self.bufferedRunwaySeconds(
                playheadSeconds: localSeconds,
                ranges: loaded
            )
            snapshot.playbackBufferEmpty = item.isPlaybackBufferEmpty
            snapshot.playbackLikelyToKeepUp = item.isPlaybackLikelyToKeepUp
            snapshot.playbackBufferFull = item.isPlaybackBufferFull
            if let event = accessLogSample(for: item)?.values {
                snapshot.mediaRequests = Self.nonnegative(event.numberOfMediaRequests)
                snapshot.downloadedDuration = Self.nonnegative(event.segmentsDownloadedDuration)
                snapshot.bytesTransferred = event.numberOfBytesTransferred >= 0
                    ? Int(clamping: event.numberOfBytesTransferred)
                    : nil
                snapshot.transferDuration = Self.nonnegative(event.transferDuration)
                snapshot.observedBitrateBps = Self.nonnegative(event.observedBitrate)
                snapshot.indicatedBitrateBps = Self.nonnegative(event.indicatedBitrate)
                snapshot.accessStalls = Self.nonnegative(event.numberOfStalls)
            }
        }
        if let status = diagnosticSessionStatus {
            let ageMs = diagnosticSessionStatusObservedAt.map {
                max(0, Int(Date().timeIntervalSince($0) * 1_000))
            }
            snapshot.server = ApplePlaybackServerSnapshot(status, observedAgeMs: ageMs)
        }
        return snapshot
    }

    nonisolated private static func nonnegative<T: BinaryInteger>(_ value: T) -> Int? {
        value >= 0 ? Int(clamping: value) : nil
    }

    nonisolated private static func nonnegative(_ value: Double) -> Double? {
        value.isFinite && value >= 0 ? value : nil
    }

    nonisolated static func timeControlStatusLabel(_ status: AVPlayer.TimeControlStatus) -> String {
        switch status {
        case .paused: return "paused"
        case .waitingToPlayAtSpecifiedRate: return "waiting"
        case .playing: return "playing"
        @unknown default: return "unknown"
        }
    }

    var presentationSize: CGSize { player.currentItem?.presentationSize ?? .zero }

    func start(
        model: AppModel,
        itemId: Int,
        fileId: Int,
        startMs: Int,
        durationMs: Int,
        progressOffsetMs: Int = 0,
        itemDurationMs: Int? = nil,
        title: String,
        selection: PrePlaySelection = .none,
        initialHeight: Int? = nil,
        diagnosticProbesEnabled: Bool = false
    ) {
        guard !started else { return }
        started = true
        diagnosticAuthorizationGeneration = Session.shared.playbackAuthorization.generation
        recordDiagnosticPlayback("state", action: "play")
        lifecycleGeneration &+= 1
        let lifecycle = lifecycleGeneration
        wantsPlayback = true
        resetSurface()
        decision = nil
        #if os(iOS)
        offlineId = nil
        offlineAssetURL = nil
        #endif
        self.model = model
        self.itemId = itemId
        self.fileId = fileId
        self.fileContext = try? PlaybackFileContext.local(fileId)
        self.progressOffsetMs = max(0, progressOffsetMs)
        self.itemDurationMs = itemDurationMs
        self.knownDurationMs = durationMs
        self.currentMs = max(0, startMs)
        self.title = title
        let explicitHeight = initialHeight.flatMap { $0 > 0 ? $0 : nil }
        initialDecisionRequest = InitialDecisionRequest(
            selection: selection,
            quality: explicitHeight.map { Self.initialQuality(height: $0) } ?? model.playbackQuality,
            explicitHeight: explicitHeight
        )
        selectedAudio = selection.audioIndex
        selectedSubtitle = selection.subtitleIndex.flatMap { $0 >= 0 ? $0 : nil }
        selectedHeight = initialHeight.flatMap { $0 > 0 ? $0 : nil }
        selectedQualityIsOriginal = initialHeight == nil && startingQuality == .original
        self.diagnosticProbesEnabled = diagnosticProbesEnabled
        audioOverride = nil
        finished = false
        subtitleReadiness = model.subtitleReadiness
        markerAutoSkipLedger = MarkerAutoSkipLedger()
        wantsNativeSubtitleRenditions = false
        activeBurnedSubtitle = nil
        canRetryCurrentItemWithHDRBase = false
        dolbyVisionFallbackAttempted = false
        forceCompatibleHDRBase = false
        canRetryCurrentItemWithTranscode = false
        compatibilityFallbackAttempted = false
        forceCompatibilityTranscode = false
        ttffReason = currentMs > 0 ? "resume" : "cold-start"
        lastTTFFMs = nil
        playbackControlSummary = nil
        ttffMeasurement.opened(at: currentMs)
        attachmentRecovery.opened(at: startMs)
        blackFrameWatchdog.opened()
        establishedHDRRetryAttempted = false
        stallObservation.reset()
        lastUncorroboratedEndMs = nil
        sameDeliveryStallRecovery.reset()
        deliveryStarvation.reset()
        recoveryReopenBudget.reset()
        installSurfaceLifecycleObservation()

        #if os(iOS)
        // iOS needs an explicit playback audio session for silent-switch and
        // background/PiP behavior. Activating it on tvOS was a regression:
        // Apple TV owns the output route and AVPlayer can remain waiting even
        // though the item and server are healthy.
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
        try? AVAudioSession.sharedInstance().setActive(true)
        #endif
        installRemoteCommands()
        startAudioSessionObservation()

        audioLanguage = model.audioLang
        // P2-8, taking the plan's second option: own media selection outright.
        // Leaving automatic criteria on lets AVPlayer enable a legible
        // rendition from the master's AUTOSELECT/DEFAULT metadata and the
        // system caption preference as soon as the item loads — before the
        // explicit selection lands — which puts a text track on top of a
        // burned-in one for the first seconds of a burn session. Emptying the
        // legible criteria is not enough, because those two sources remain.
        // The plan's first option (apply the selection before `play()`) was
        // rejected: the selection needs an await, and gating the first
        // `play()` behind item readiness is the tvOS transport regression
        // recorded in `open`. Owning selection means owning audio too, which
        // `applyPreferredAudioSelection` does per item.
        player.appliesMediaSelectionCriteriaAutomatically = false
        player.automaticallyWaitsToMinimizeStalling = true
        addPeriodicObserver()
        startPlaybackRecoveryMonitor()
        startSurfaceClock()

        restartInitialDecision(lifecycle: lifecycle)
    }

    #if os(iOS)
    /// Attach a system-managed local HLS package. This branch deliberately
    /// constructs its decision-shaped snapshot locally and performs no API or
    /// capability-URL work: airplane-mode playback is a first-class source,
    /// not a remote failure fallback.
    func startOffline(model: AppModel, item offline: OfflineItem) {
        guard !started, let path = offline.localAssetRelativePath else { return }
        started = true
        lifecycleGeneration &+= 1
        let lifecycle = lifecycleGeneration
        self.model = model
        offlineId = offline.id
        offlineAssetURL = OfflineCatalog.localURL(for: path)
        itemId = offline.itemId
        fileId = offline.fileId
        fileContext = try? PlaybackFileContext.local(offline.fileId)
        knownDurationMs = offline.durationMs ?? 0
        currentMs = max(0, offline.positionMs)
        title = offline.title
        finished = false
        resetSurface()
        activeBurnedSubtitle = nil
        audioOverride = nil
        audioLanguage = model.audioLang
        selectedHeight = offline.actualHeight
        selectedQualityIsOriginal = false
        initialDecisionRequest = InitialDecisionRequest()
        selectedAudio = offline.audioLabel == nil ? nil : 0
        selectedSubtitle = offline.subtitleIndex == nil ? nil : 0
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        playbackEnhancement = nil
        deliveredRange = "sdr"
        encoder = "offline"
        isVOD = true
        usesDirectTimeline = true
        isDirectPlayback = true
        decision = Self.offlineDecision(offline)
        markerAutoSkipLedger = MarkerAutoSkipLedger()
        wantsPlayback = true
        attachmentRecovery.opened(at: currentMs)
        blackFrameWatchdog.opened()
        stallObservation.reset()
        deliveryStarvation.reset()
        recoveryReopenBudget.reset()
        lastUncorroboratedEndMs = nil
        player.appliesMediaSelectionCriteriaAutomatically = false
        player.automaticallyWaitsToMinimizeStalling = true
        try? AVAudioSession.sharedInstance().setCategory(.playback, mode: .moviePlayback)
        try? AVAudioSession.sharedInstance().setActive(true)
        installRemoteCommands()
        startAudioSessionObservation()
        installSurfaceLifecycleObservation()
        addPeriodicObserver()
        startPlaybackRecoveryMonitor()
        startSurfaceClock()
        let startMs = currentMs
        let url = OfflineCatalog.localURL(for: path)
        loadingTask = Task { await loadOffline(url: url, startMs: startMs, lifecycle: lifecycle) }
    }

    nonisolated static func offlineDecision(_ item: OfflineItem) -> Decision {
        let audio = item.audioLabel.map {
            [AudioTrack(index: 0, codec: "aac", channels: nil, language: nil, title: $0, default: true)]
        }
        let subtitles = item.subtitleIndex.map { _ in
            [SubtitleTrack(
                index: 0,
                codec: "webvtt",
                language: nil,
                title: item.subtitleLabel ?? "Downloaded",
                default: true,
                forced: false,
                text: true,
                native: true,
                overlay: nil
            )]
        }
        return Decision(
            fileId: item.fileId,
            method: "offline",
            playUrl: "",
            delivery: Delivery(mode: "direct", url: nil, sessionsUrl: nil, aac: nil, preserveDolbyVision: nil),
            reasons: ["Downloaded for offline viewing"],
            transcodeAudio: false,
            preserveDolbyVision: false,
            source: SourceSummary(
                container: "mpegts",
                videoCodec: "h264",
                videoProfile: nil,
                width: nil,
                height: item.actualHeight,
                bitDepth: 8,
                hdr: nil,
                hdrFormat: nil,
                bitrate: nil,
                durationMs: item.durationMs
            ),
            audio: audio,
            subtitles: subtitles,
            markers: item.markers,
            audioOffsetMs: 0,
            declaredOffsetMs: 0,
            ladder: [],
            deliveredDynamicRange: "sdr"
        )
    }

    private func loadOffline(url: URL, startMs: Int, lifecycle: Int) async {
        guard isCurrentLifecycle(lifecycle) else { return }
        let startMs = seekState.pendingMs ?? startMs
        let seekOwner = seekPreparationOwner(at: startMs)
        openGeneration &+= 1
        let generation = openGeneration
        let requestedRecipeRevision = recipeRevision.desired
        let attemptId = UUID().uuidString
        let asset = AVURLAsset(url: url)
        guard canPlayOffline(asset) else {
            fail(APIError.transport("Download incomplete"))
            return
        }
        let item = AVPlayerItem(asset: asset)
        Self.configureBuffering(item, growingHLS: false)
        installSeekVideoOutput(on: item)
        item.externalMetadata = [titleMetadata(title)]
        retireItemObserver()
        player.replaceCurrentItem(with: item)
        installItemObserver(for: item)
        // The attached media generation changed: every fault about the
        // generation this replaces stops being about anything and is dropped.
        present(.attach(generation))
        if resumeAttempt?.repairAdmitted == true {
            bindResumeRepairSuccessor(item, generation: generation)
        }
        recipeRevision.didAttach(requestedRecipeRevision)
        if requestedRecipeRevision == recipeRevision.desired { manualQualityRetention.didAttach() }
        playbackAttemptId = attemptId
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        latestAutoCompletedTransfer = nil
        autoMildSamples = 0
        baseMs = 0
        player.play()
        if !wantsPlayback { player.pause() }
        if startMs > 0 {
            do { try await seekWhenReady(item, ms: startMs, owner: seekOwner) }
            catch {
                guard isCurrentLifecycle(lifecycle), !isSuperseded(generation), player.currentItem === item else { return }
                fail(error)
                return
            }
        }
        guard isCurrentLifecycle(lifecycle), !isSuperseded(generation), player.currentItem === item else { return }
        await reconcileNativeMediaSelections(to: item)
        guard isCurrentLifecycle(lifecycle), !isSuperseded(generation), player.currentItem === item else { return }
        applyDisplayCriteria(for: item, generation: generation)
        if wantsPlayback { player.play() } else { player.pause() }
        isPlaying = wantsPlayback
        present(.intentSettled(viewerActionEpoch))
        attachmentRecovery.opened(at: startMs)
        blackFrameWatchdog.opened()
        updateNowPlaying()
    }

    private func reloadOffline(at positionMs: Int) async {
        guard let offlineAssetURL, started else { return }
        let lifecycle = lifecycleGeneration
        let generation = openGeneration &+ 1
        isChangingStream = true
        Session.shared.resetMediaFailover()
        playbackRecoveryMonitor.reset()
        deliveryStarvation.reset()
        await loadOffline(url: offlineAssetURL, startMs: positionMs, lifecycle: lifecycle)
        guard isCurrentLifecycle(lifecycle), !isSuperseded(generation) else { return }
        isChangingStream = false
        // The status observer holds its fire while `isChangingStream` is up;
        // a replacement that failed during its own load is caught here, the
        // same re-check `open()` performs for server sessions. `loadOffline`'s
        // own error paths have already surfaced through `fail()` — only a
        // suppressed KVO failure needs the second look.
        if !isPlaybackBlocked, let item = player.currentItem, item.status == .failed {
            await handleItemFailure(item)
        }
    }
    #endif

    @discardableResult
    private func beginViewerAction() -> Int {
        restorePreparedIncumbentBufferPreference()
        invalidateResumeAttempt(outcome: "viewer-action")
        let superseded = viewerActionEpoch
        viewerActionEpoch &+= 1
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        latestAutoCompletedTransfer = nil
        autoMildSamples = 0
        playbackControl.viewerActionChanged()
        // The viewer has moved on from whatever was being staged for them, so
        // a fault about that destination is no longer about anything.
        present(.intentSuperseded(superseded))
        playbackControl.clearVerdict()
        deferredStall = nil
        // A seek, a second quality change, an audio change: the viewer has
        // moved on from whatever was being staged for them, so it is owed
        // `aborted` and its pipeline freed. Without a fallback, because the
        // command that got here is itself the change now being made.
        preparedReplacement.abandonWithoutFallback(.aborted)
        return viewerActionEpoch
    }

    private func retainControlSequence(_ sequence: UInt64?) {
        guard let sequence else { return }
        pendingControlSequence = max(pendingControlSequence ?? 0, sequence)
    }

    func togglePlayPause() {
        recordDiagnosticPlayback("action", action: wantsPlayback ? "pause" : "play")
        // Under a system hold the transport is paused whatever the retained
        // intent says, and the control reads Play: the press is a resume.
        setPlaybackRequested(systemPaused || !wantsPlayback)
    }

    /// The one owner for explicit Play and Pause, including lock-screen input.
    /// The retained intent changes before AVPlayer can call back, and the
    /// latest task alone may retain a publication floor or resume a target.
    func setPlaybackRequested(_ requested: Bool) {
        setPlaybackRequested(requested, viewerOrigin: true)
    }

    private func setPlaybackRequested(_ requested: Bool, viewerOrigin: Bool) {
        if systemPaused, viewerOrigin { endSystemHoldForViewer(play: requested) }
        guard requested != wantsPlayback else { return }
        let requestedAt = resumeNow()
        manualQualityRetention.transportChanged(from: viewerActionEpoch, to: viewerActionEpoch &+ 1)
        let actionEpoch = beginViewerAction()
        resumeIntentTask?.cancel()

        // A buffering player reports `.waitingToPlayAtSpecifiedRate` and rate
        // zero even though Play is still the viewer's intent. Keying this
        // control from the intent prevents a wait from masquerading as a user
        // pause (and from turning a pause press into another play request).
        if !requested {
            if player.timeControlStatus == .playing, player.rate > 0 {
                preferredRate = player.rate
            }
            wantsPlayback = false
            player.pause()
            isPlaying = false
            pauseBeganAt = requestedAt
            explicitViewerPause = viewerOrigin ? player.currentItem.map {
                (ObjectIdentifier($0), sessionId, openGeneration, lifecycleGeneration)
            } : nil
        } else if let retired = currentPausedRetirement, !isChangingStream {
            // The presentation this pause was holding no longer exists on the
            // server, so there is nothing to resume in place: the buffered
            // runway ends in a playlist that now answers 404/410. Open the one
            // replacement at the saved position instead. `open()` keeps the
            // viewer's intent, which is Play from here on. The latch stays:
            // the successor's `sessionId` retires it, and if the create fails
            // the dead item comes back and the next Play must reopen again.
            wantsPlayback = true
            pauseBeganAt = nil
            explicitViewerPause = nil
            isPlaying = false
            let position = Self.pausedRetirementReopenPositionMs(
                pendingSeekMs: seekState.pendingMs,
                retired: retired,
                attachedPositionMs: player.currentItem?.status == .failed
                    ? nil
                    : realPositionMs(),
                lastObservedMs: currentMs
            )
            noteSurfaceLogOnly("paused_retirement_reopen:\(position)")
            updateNowPlaying()
            // Deliberately not `resumeIntentTask`: a Pause cancels that task,
            // and a cancelled create surfaces through `fail()`. A Pause during
            // this open is honoured by `open()` itself, which reads the intent.
            Task { @MainActor [weak self] in
                guard let self, self.started else { return }
                await self.reopen(at: position)
            }
            return
        } else {
            wantsPlayback = true
            let pauseDurationMs = pauseBeganAt.map {
                max(0, Int(((requestedAt - $0) * 1_000).rounded()))
            } ?? 0
            let samePauseOwner = explicitViewerPause.map {
                player.currentItem.map(ObjectIdentifier.init) == $0.item && sessionId == $0.session &&
                    openGeneration == $0.attachment && lifecycleGeneration == $0.lifecycle
            } == true
            pauseBeganAt = nil
            explicitViewerPause = nil
            // Play is applied below on the press, whatever the pause length.
            // A long pause only owes Auto one original-first re-plan, which
            // the ordinary evaluation serves beside the playing incumbent.
            if AutoViewerBoundary.resumeArmsReplan(viewerOrigin: viewerOrigin,
                    pauseDurationMs: pauseDurationMs, samePauseOwner: samePauseOwner) {
                autoBoundaryReplan.arm(snapshotAttempt())
            }

            let hasPendingDestination = seekState.pendingMs != nil
            let externalPresentation = player.isExternalPlaybackActive
                || pictureInPictureIsActive
            let runway = bufferedRunwaySeconds()
            let established = boundedResumeEnabled && !externalPresentation
                && started && attachmentRecovery.establishedPlayback
                && player.currentItem != nil
            let buffered = established
                && PlaybackResumeAttempt.bufferedFastPathQualifies(
                    ready: player.currentItem?.status == .readyToPlay,
                    runwaySeconds: runway,
                    intendedRate: preferredRate,
                    hasPendingDestination: hasPendingDestination,
                    replacementInFlight: isChangingStream
                )
            if hasPendingDestination || isChangingStream {
                // The predecessor is not the requested destination. Retain
                // Play, publish it, and let the current preparation owner be
                // the only code allowed to start a positive rate.
                isPlaying = false
            } else if buffered {
                beginResumeAttempt(
                    at: requestedAt,
                    actionEpoch: actionEpoch,
                    pauseDurationMs: pauseDurationMs,
                    runwaySeconds: runway,
                    path: "buffered-immediate",
                    fastPath: established
                )
                Self.applyPlaybackCommand(to: player, preferredRate: preferredRate, immediately: true)
                isPlaying = true
            } else if established {
                // Empty, unknown, disjoint, unready, or insufficient retained
                // media pays no ordinary 12-second stall wait. Publication
                // still orders the authorized create before repair begins.
                beginResumeAttempt(
                    at: requestedAt,
                    actionEpoch: actionEpoch,
                    pauseDurationMs: pauseDurationMs,
                    runwaySeconds: runway,
                    path: "same-delivery-repair",
                    fastPath: false
                )
                isPlaying = false
            } else {
                // Cold startup, external playback, and unrelated preparation
                // retain their existing owners and budgets.
                Self.applyPlaybackCommand(
                    to: player,
                    preferredRate: preferredRate,
                    immediately: false
                )
                isPlaying = true
            }
        }
        updateNowPlaying()
        let lifecycle = lifecycleGeneration
        let attachmentGeneration = openGeneration
        let itemIdentity = player.currentItem.map(ObjectIdentifier.init)
        let currentSession = sessionId
        let target = seekState.pendingMs
        let seekGeneration = seekState.generation
        let repairAfterPublication = requested
            && resumeAttempt?.viewerActionEpoch == actionEpoch
            && resumeAttempt?.fastPathDeadline == nil
        resumeIntentTask = Task { [weak self] in
            guard let self,
                  self.lifecycleGeneration == lifecycle,
                  self.viewerActionEpoch == actionEpoch,
                  self.wantsPlayback == requested,
                  self.openGeneration == attachmentGeneration,
                  self.player.currentItem.map(ObjectIdentifier.init) == itemIdentity,
                  self.sessionId == currentSession
            else { return }
            let floor = await self.reportPlaybackIntent(self.playbackControl)
            guard !Task.isCancelled,
                  self.lifecycleGeneration == lifecycle,
                  self.viewerActionEpoch == actionEpoch,
                  self.wantsPlayback == requested,
                  self.openGeneration == attachmentGeneration,
                  self.player.currentItem.map(ObjectIdentifier.init) == itemIdentity,
                  self.sessionId == currentSession
            else { return }
            self.retainControlSequence(floor)
            // Pause publishes and retains the destination; only a later Play
            // may dispatch it or admit repair. A quality change also pins a
            // seek, so dispatching here on Pause can reopen its recipe and
            // supersede the prepared owner that Pause just withdrew.
            guard requested else {
                if self.viewerActionEpoch == actionEpoch {
                    self.resumeIntentTask = nil
                }
                return
            }
            if var attempt = self.resumeAttempt,
               attempt.viewerActionEpoch == actionEpoch {
                attempt.markPublicationCompleted()
                self.resumeAttempt = attempt
            }
            if let target,
               self.seekState.pendingMs == target,
               self.seekState.generation == seekGeneration {
                self.issueSeek(
                    to: target,
                    generation: seekGeneration,
                    owningActionEpoch: actionEpoch,
                    intentAlreadyPublished: true
                )
            } else if let failedItem = self.resumePendingFailedItem {
                self.resumePendingFailedItem = nil
                await self.handleItemFailure(failedItem)
            } else if repairAfterPublication || self.resumeRepairPendingReason != nil {
                let reason = self.resumeRepairPendingReason ?? "retained-media-unusable"
                self.resumeRepairPendingReason = nil
                self.admitResumeRepair(at: self.resumeNow(), reason: reason)
            }
            if self.viewerActionEpoch == actionEpoch {
                self.resumeIntentTask = nil
            }
        }
    }

    /// The command seam used by the explicit-resume and ordinary-watchdog
    /// owners. Keeping selection separate from AVPlayer makes it possible to
    /// prove that immediate play is issued once and retains a non-unit rate.
    nonisolated static func applyPlaybackCommand(
        to player: AVPlayer,
        preferredRate: Float,
        immediately: Bool
    ) {
        if immediately {
            player.playImmediately(atRate: preferredRate)
        } else {
            player.play()
            if preferredRate != 1 { player.rate = preferredRate }
        }
    }

    private func beginResumeAttempt(
        at now: TimeInterval,
        actionEpoch: Int,
        pauseDurationMs: Int,
        runwaySeconds: Double?,
        path: String,
        fastPath: Bool
    ) {
        guard started, attachmentRecovery.establishedPlayback,
              let item = player.currentItem else { return }
        let attempt = PlaybackResumeAttempt(
            lifecycleGeneration: lifecycleGeneration,
            viewerActionEpoch: actionEpoch,
            attachmentGeneration: openGeneration,
            itemIdentity: ObjectIdentifier(item),
            targetMs: realPositionMs(),
            startedAt: now,
            fastPathDeadline: fastPath
                ? now + PlaybackResumeAttempt.fastPathSeconds
                : nil,
            expiresAt: now + PlaybackResumeAttempt.totalSeconds,
            pauseDurationMs: pauseDurationMs,
            initialRunwaySeconds: runwaySeconds,
            initialTimeControlStatus: Self.timeControlStatusLabel(player.timeControlStatus),
            initialWaitingReason: player.reasonForWaitingToPlay?.rawValue,
            selectedPath: path,
            lastVideoDisplaySeconds: sampleResumeVideoDisplaySeconds()
        )
        resumeAttempt = attempt
        reportResumeTransition(attempt, phase: "started", outcome: path, at: now)
        startResumePresentationMonitor()
    }

    private func startResumePresentationMonitor() {
        guard resumePresentationTask == nil else { return }
        resumePresentationTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, var attempt = self.resumeAttempt else { return }
                let now = self.resumeNow()
                guard self.lifecycleGeneration == attempt.lifecycleGeneration,
                      self.viewerActionEpoch == attempt.viewerActionEpoch,
                      self.wantsPlayback
                else {
                    self.invalidateResumeAttempt(outcome: "ownership-changed")
                    return
                }
                if self.player.isExternalPlaybackActive || self.pictureInPictureIsActive {
                    self.invalidateResumeAttempt(outcome: "external-presentation")
                    return
                }
                if attempt.totalExpired(at: now) {
                    self.expireResumeAttempt(at: now)
                    return
                }

                let waiting = self.player.timeControlStatus == .waitingToPlayAtSpecifiedRate
                var observation = PlaybackResumeAttempt.PresentationObservation.none
                if !attempt.repairAdmitted || !self.isChangingStream,
                   let item = self.player.currentItem,
                   attempt.attachmentGeneration == self.openGeneration,
                   attempt.itemIdentity == ObjectIdentifier(item) {
                    if self.decision?.source?.videoCodec != nil {
                        if let display = self.sampleResumeVideoDisplaySeconds() {
                            observation = attempt.observeVideo(
                                displaySeconds: display,
                                at: now,
                                waiting: waiting
                            )
                        } else if waiting {
                            attempt.presentationInterrupted()
                        }
                    } else {
                        observation = attempt.observeAudio(
                            positionMs: self.realPositionMs(),
                            at: now,
                            playing: self.player.timeControlStatus == .playing
                                && self.player.rate > 0,
                            waiting: waiting
                        )
                    }
                }
                self.resumeAttempt = attempt

                switch observation {
                case .none:
                    break
                case .firstPicture(let milliseconds):
                    self.reportResumeTransition(
                        attempt,
                        phase: "first-picture",
                        outcome: "fresh-sample",
                        at: now,
                        firstPictureMs: milliseconds
                    )
                case .settled(let firstPictureMs, let settledMs):
                    self.reportResumeTransition(
                        attempt,
                        phase: "settled",
                        outcome: "continuing-presentation",
                        at: now,
                        firstPictureMs: firstPictureMs,
                        settledMs: settledMs
                    )
                    self.resumeRepairTask?.cancel()
                    self.resumeRepairTask = nil
                    self.resumeRepairPendingReason = nil
                    self.resumePendingFailedItem = nil
                    self.resumeAttempt = nil
                    self.resumePresentationTask = nil
                    return
                }

                if let current = self.resumeAttempt,
                   !current.repairAdmitted,
                   current.fastPathExpired(at: now) {
                    self.admitResumeRepair(at: now, reason: "fast-path-no-presentation")
                }
                await self.waitResumeSample()
            }
        }
    }

    /// Use the existing per-item video output. A second consumer could steal
    /// the seek monitor's frame; resume is armed only when no seek owner is
    /// active, and a later seek invalidates this attempt before sampling.
    private func sampleResumeVideoDisplaySeconds() -> Double? {
        guard let output = seekVideoOutput else { return nil }
        let itemTime = output.itemTime(forHostTime: resumeNow())
        guard itemTime.isValid, itemTime.seconds.isFinite,
              output.hasNewPixelBuffer(forItemTime: itemTime)
        else { return nil }
        var displayTime = CMTime.invalid
        guard output.copyPixelBuffer(
            forItemTime: itemTime,
            itemTimeForDisplay: &displayTime
        ) != nil,
        displayTime.isValid, displayTime.seconds.isFinite
        else { return nil }
        return displayTime.seconds
    }

    /// Spend the single repair admission without choosing its route. Timer
    /// stalls call `admitResumeRepair`; item failures claim here and then keep
    /// their already-classified transport/HDR/compatibility path.
    private func claimResumeRepair(at now: TimeInterval, reason: String) -> PlaybackResumeAttempt? {
        guard var attempt = resumeAttempt else { return nil }
        guard attempt.publicationCompleted else {
            resumeRepairPendingReason = reason
            return nil
        }
        switch attempt.admitRepair(at: now) {
        case .expired:
            resumeAttempt = attempt
            expireResumeAttempt(at: now)
            return nil
        case .alreadyAdmitted:
            return nil
        case .admitted:
            resumeAttempt = attempt
            reportResumeTransition(attempt, phase: "repair-admitted", outcome: reason, at: now)
            reportControlEvidence(Self.stallEvidence(for: .buffering), render: .stalled)
            return attempt
        }
    }

    private func admitResumeRepair(at now: TimeInterval, reason: String) {
        guard let attempt = claimResumeRepair(at: now, reason: reason) else { return }
        let id = attempt.id
        let position = attempt.targetMs
        resumeRepairTask?.cancel()
        resumeRepairTask = Task { [weak self] in
            guard let self else { return }
            await self.retrySameDeliveryAfterStall(
                PlaybackStallEvent(
                    kind: .buffering,
                    action: .reopen,
                    positionMs: position,
                    durationMs: Int(max(0, (now - attempt.startedAt) * 1_000))
                ),
                consultControl: false,
                resumeAttemptId: id
            )
            if self.resumeAttempt?.id == id {
                self.resumeRepairTask = nil
            }
        }
    }

    private func bindResumeRepairSuccessor(_ item: AVPlayerItem, generation: Int) {
        guard let attempt = resumeAttempt, attempt.repairAdmitted else { return }
        let successor = attempt.boundToSuccessor(
            attachmentGeneration: generation,
            itemIdentity: ObjectIdentifier(item),
            baselineVideoDisplaySeconds: sampleResumeVideoDisplaySeconds()
        )
        resumeAttempt = successor
        reportResumeTransition(
            successor,
            phase: "successor-attached",
            outcome: "same-delivery-repair",
            at: resumeNow()
        )
    }

    private func expireResumeAttempt(at now: TimeInterval) {
        guard let attempt = resumeAttempt else { return }
        reportResumeTransition(
            attempt,
            phase: "terminal",
            outcome: "deadline-exhausted",
            at: now
        )
        resumeAttempt = nil
        resumeRepairPendingReason = nil
        resumePendingFailedItem = nil
        resumeIntentTask?.cancel()
        resumeIntentTask = nil
        resumeRepairTask?.cancel()
        resumeRepairTask = nil
        resumePresentationTask?.cancel()
        resumePresentationTask = nil
        // Fence a create/readiness wait that outlived the inherited deadline.
        // A late session is released by its existing generation check.
        openGeneration &+= 1
        reopenQueue.clear()
        isChangingStream = false
        stopForBlockingSurface(revokingPlaybackIntent: true)
        raiseOwnerFault(
            source: "owner_exhausted",
            title: Self.playbackStoppedFailureTitle,
            detail: "Playback did not resume within 15 seconds.",
            positionMs: attempt.targetMs
        )
    }

    private func invalidateResumeAttempt(outcome: String) {
        let repairWasAdmitted = resumeAttempt?.repairAdmitted == true
        if let attempt = resumeAttempt {
            reportResumeTransition(
                attempt,
                phase: "cancelled",
                outcome: outcome,
                at: resumeNow()
            )
        }
        resumeAttempt = nil
        resumeRepairPendingReason = nil
        resumePendingFailedItem = nil
        resumeIntentTask?.cancel()
        resumeIntentTask = nil
        resumePresentationTask?.cancel()
        resumePresentationTask = nil
        resumeRepairTask?.cancel()
        resumeRepairTask = nil
        if repairWasAdmitted {
            // Cancellation is cooperative. This generation fence is what
            // prevents a create or item-readiness await that ignores task
            // cancellation from attaching, playing, or surfacing a fault
            // after a final Pause/background/stop.
            openGeneration &+= 1
            reopenQueue.clear()
            isChangingStream = false
        }
    }

    private func reportResumeTransition(
        _ attempt: PlaybackResumeAttempt,
        phase: String,
        outcome: String,
        at now: TimeInterval,
        firstPictureMs: Int? = nil,
        settledMs: Int? = nil
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        postClientLog(ApplePlaybackResumeLog(
            attempt: attempt,
            phase: phase,
            outcome: outcome,
            elapsedMs: max(0, Int(((now - attempt.startedAt) * 1_000).rounded())),
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            sessionId: sessionId,
            firstPictureMs: firstPictureMs,
            settledMs: settledMs
        ))
    }

    /// A terminal automatic retry stays terminal until the viewer explicitly
    /// asks for another attempt. This keeps a malformed boundary from becoming
    /// an automatic loop while still offering recovery without dismissing the
    /// player.
    func retryAfterPlaybackFailure() {
        guard started, isPlaybackBlocked else { return }
        // The viewer answered the prompt. Retiring the fault is the
        // presenter's half; resetting the budget below is the owner's, and it
        // is the whole of Apple's Keep waiting primitive (contract §3.1).
        present(.userAction(.retry))
        beginViewerAction()
        let position = currentMs
        lastUncorroboratedEndMs = nil
        // The viewer explicitly asked for another attempt; the automatic
        // brake must not carry a spent window into it.
        recoveryReopenBudget.reset()
        if decision == nil {
            // A decision timeout has no AVPlayerItem or server session yet.
            // Retry the retained inputs and destination, including Pause.
            restartInitialDecision(lifecycle: lifecycleGeneration)
            return
        }
        // Retry authorizes another preparation, not a transport change.
        let lifecycle = lifecycleGeneration
        #if os(iOS)
        if offlineAssetURL != nil {
            Task {
                guard isCurrentLifecycle(lifecycle) else { return }
                await reloadOffline(at: position)
            }
            return
        }
        #endif
        Task {
            guard isCurrentLifecycle(lifecycle) else { return }
            await reopen(at: position)
        }
    }

    /// Whether the blocking surface on screen offers another attempt.
    ///
    /// The failure view no longer gates a hard-coded button on this — it
    /// renders the fault's own actions, `PlayerView.failureActions` — so this
    /// is the same question asked of the model for callers that only want the
    /// answer. Every blocking class defaults to including `retry`, which is
    /// why it is the same answer the bare `started` test gave for every fault
    /// this client raises, except an auth refusal, whose actions are Sign in
    /// and Close and where retrying the same bearer cannot work.
    var canRetryPlaybackFailure: Bool {
        guard started else { return false }
        let actions = surface.surface.actions
        guard !actions.isEmpty else { return true }
        return actions.contains(.retry) || actions.contains(.keepWaiting)
    }

    func skip(seconds: Double) {
        let request = seekState.relative(
            by: Int(seconds * 1000),
            observedMs: positionForPlaybackIntent(),
            durationMs: knownDurationMs
        )
        abandonSeekMeasurement()
        requestedSeekGeneration = request.generation
        issueSeek(to: request.target, generation: request.generation, viewerBoundary: true)
    }

    func skipActiveMarker() {
        guard let marker = activeMarker else { return }
        reportMarkerEvent(
            "marker_manual_skip",
            detail: marker.kind,
            message: "playback marker skipped"
        )
        reportMarkerEvent(
            "marker_prewarm",
            detail: "miss",
            message: "skip destination was not prewarmed"
        )
        lastMarkerSkipEndMs = marker.endMs
        beginSeek(toMs: marker.endMs, viewerOrigin: false)
    }

    /// The automatic half of the marker button — the web client's
    /// `checkMarkers` and the Android auto-skip effect, driven here by the
    /// position clock. It issues exactly the seek `skipActiveMarker` does,
    /// once per marker per playback, only while the Skip preference is on
    /// and only for a marker `Marker.isAutoSkipEligible` admits. The
    /// preference is read live, like the other two clients: nothing about
    /// the stream depends on it.
    ///
    /// Held while a seek or a stream change is in flight: `currentMs` is
    /// then an optimistic target rather than a position the film reached,
    /// and a marker it happens to fall in has not been entered.
    func autoSkipActiveMarkerIfNeeded() {
        guard let model, started,
              !isChangingStream, seekState.pendingMs == nil,
              let marker = activeMarker,
              markerAutoSkipLedger.shouldSkip(marker, autoSkip: model.autoSkip)
        else { return }
        reportMarkerEvent(
            "marker_automatic_skip",
            detail: marker.kind,
            message: "playback marker skipped"
        )
        reportMarkerEvent(
            "marker_prewarm",
            detail: "miss",
            message: "skip destination was not prewarmed"
        )
        lastMarkerSkipEndMs = marker.endMs
        beginSeek(toMs: marker.endMs, viewerOrigin: false)
    }

    func reportMarkerOffer(_ marker: Marker) {
        guard markerOfferLedger.shouldReport(
            generation: playbackAttemptId,
            marker: marker
        ) else { return }
        reportMarkerEvent(
            "marker_offer",
            detail: marker.kind,
            message: "playback marker offered"
        )
    }

    func seek(toMs requested: Int) {
        recordDiagnosticPlayback("action", action: "seek")
        beginSeek(toMs: requested, viewerOrigin: true)
    }

    private func beginSeek(toMs requested: Int, viewerOrigin: Bool) {
        autoDestinationRevision = min(autoDestinationRevision + 1, 9_007_199_254_740_991)
        resetAutoQualityBudgetForViewer()
        if let markerEnd = lastMarkerSkipEndMs,
           requested < markerEnd - 1_000,
           currentMs >= markerEnd - 1_000 {
            reportMarkerEvent(
                "marker_seek_back",
                detail: "undo",
                message: "viewer sought behind the last marker destination"
            )
            lastMarkerSkipEndMs = nil
        }
        let request = seekState.absolute(requested, durationMs: knownDurationMs)
        abandonSeekMeasurement()
        requestedSeekGeneration = request.generation
        issueSeek(to: request.target, generation: request.generation, viewerBoundary: viewerOrigin)
    }

    private func issueSeek(
        to target: Int,
        generation: Int,
        owningActionEpoch: Int? = nil,
        intentAlreadyPublished: Bool = false,
        viewerBoundary: Bool = false
    ) {
        // Relative jumps and manual/automatic markers all enter here too.
        // `playbackEnhancement` is deliberately kept: clearing the report is
        // what hides its report-gated pills (see its declaration).
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        let actionEpoch = owningActionEpoch ?? beginViewerAction()
        // The seek is executed below without waiting for anything optional.
        // The original-first re-plan a viewer seek is owed is served later by
        // the ordinary Auto evaluation, beside the incumbent at its new place.
        if viewerBoundary { autoBoundaryReplan.arm(snapshotAttempt()) }
        // Move the visible timeline immediately. The old implementation left
        // it on the paused predecessor for the whole server round trip, which
        // made tvOS look as though the progress command had not worked.
        currentMs = target
        ttffMeasurement.rebasePosition(at: target)
        pgsOverlaySeeked(to: target)
        // Every viewer, relative and marker discontinuity revokes the old
        // caption traversal. Ordinary item/quality attachments keep it alive.
        progressiveCaptions.mediaChanged(seek: true, positionMs: target)
        playbackRecoveryMonitor.reset()
        deliveryStarvation.reset()
        interactiveSeekTask?.cancel()
        let seekAttempt = snapshotAttempt()
        interactiveSeekTask = Task { [self] in
            // Coalesce native and replacement seeks alike. Executing every
            // scrub event makes AVPlayer and the server race old destinations.
            if !intentAlreadyPublished {
                try? await Task.sleep(for: Self.seekCoalescingDelay)
            }
            guard !Task.isCancelled,
                  seekAttempt.seek == generation,
                  seekAttempt.viewerAction == actionEpoch,
                  attemptStillCurrent(seekAttempt, fence: .seekIntent)
            else { return }
            if !intentAlreadyPublished {
                retainControlSequence(await playbackControl.reportIntent())
            }
            guard !Task.isCancelled,
                  attemptStillCurrent(seekAttempt, fence: .seekIntentAfterControl)
            else { return }
            if requestedSeekGeneration == generation {
                #if os(iOS)
                let reportsSeek = offlineId == nil
                #else
                let reportsSeek = true
                #endif
                if reportsSeek, let previous = seekMeasurement.dispatched(
                    generation: generation,
                    method: decision != nil && player.currentItem != nil ? clientLogMethod : nil,
                    fileId: fileId, attempt: playbackAttemptId
                ) { postClientLog(previous) }
            }
            if recipeRevision.needsReopen {
                await reopen(at: target)
                return
            }
            let route = Self.seekRoute(
                targetMs: target,
                baseMs: baseMs,
                usesDirectTimeline: usesDirectTimeline,
                isVOD: isVOD,
                isChangingStream: isChangingStream,
                seekableRangesMs: currentItemSeekableRangesMs()
            )
            switch route {
            case .native(let itemMs):
                let item = player.currentItem
                let nativeAttempt = snapshotAttempt()
                let outcome = await PlayerSeekCompletion.run(
                    operation: { [player, nativeSeek] in await nativeSeek(player, itemMs) },
                    waitForDeadline: { [self] in try await waitNativeSeekDeadline() }
                )
                // Only the newest seek may publish or escalate; an older
                // completion arriving after AVPlayer cancelled it must not.
                guard attemptStillCurrent(nativeAttempt, fence: .nativeSeekCompletion),
                      player.currentItem === item
                else { return }
                guard !Task.isCancelled else { return }
                switch outcome {
                case .cancelled:
                    return
                case .finished(true):
                    break
                case .finished(false), .timedOut:
                    // Cancel only while this attempt still owns the item;
                    // a stale timeout must never cancel a newer seek on it.
                    if outcome == .timedOut { item?.cancelPendingSeeks() }
                    guard wantsPlayback, !isPlaybackBlocked, !finished,
                          seekState.markExecuted(generation: generation, targetMs: target)
                    else { return }
                    if UIApplication.shared.applicationState == .background,
                       decision?.source?.videoCodec != nil {
                        // Retain the presentation obligation across background
                        // suspension; its active-demand clock resumes in front.
                        beginSeekPresentationMonitor(generation: generation, targetMs: target)
                        return
                    }
                    await retrySameDeliveryAfterStall(
                        PlaybackStallEvent(
                            kind: .buffering, action: .reopen, positionMs: target,
                            durationMs: outcome == .timedOut
                                ? Int(Self.seekPresentationDeadlineSeconds * 1_000) : 0
                        ),
                        consultControl: false
                    )
                    return
                }
                // A seek or Pause may have invalidated an awaited native
                // subtitle choice. Reconcile that retained choice on the
                // same item before acknowledging the new destination.
                if let item { await reconcileNativeMediaSelections(to: item) }
                guard attemptStillCurrent(nativeAttempt, fence: .nativeSeekAfterSelection),
                      player.currentItem === item
                else { return }
                let landed = realPositionMs()
                // A growing window can go stale between the route decision
                // and the landing — AVPlayer then clamps somewhere else
                // entirely. The reopen path is the truth for that target;
                // `pendingMs` still names it, so the bar holds the target
                // and the reopen drain completes it.
                if !usesDirectTimeline, !isVOD, abs(landed - target) > 5_000 {
                    await reopen(at: target)
                    return
                }
                if wantsPlayback {
                    player.play()
                    if preferredRate != 1 { player.rate = preferredRate }
                } else {
                    player.pause()
                }
                isPlaying = wantsPlayback
                guard seekState.markExecuted(generation: generation, targetMs: target) else { return }
                currentMs = landed
                beginSeekPresentationMonitor(generation: generation, targetMs: target)
                updateNowPlaying()
            case .reopen:
                await reopen(at: target)
            }
        }
    }

    /// The current item's seekable windows, in its own local clock. Empty
    /// while no item is attached or the playlist has not loaded yet — both
    /// route a seek to the reopen path.
    private func currentItemSeekableRangesMs() -> [ClosedRange<Int>] {
        guard let item = player.currentItem else { return [] }
        return item.seekableTimeRanges.compactMap { value in
            let range = value.timeRangeValue
            let start = range.start.seconds
            let duration = range.duration.seconds
            guard start.isFinite, duration.isFinite, duration > 0 else { return nil }
            let lower = Int(start * 1000)
            let upper = Int((start + duration) * 1000)
            guard upper > lower else { return nil }
            return lower...upper
        }
    }

    func selectSubtitle(_ index: Int?) {
        let wasProgressive = progressiveCaptionOutputSelected
        retireProgressiveCaptionSelection()
        progressiveCaptions.deselect()
        player.allowsExternalPlayback = !pgsOverlayIsActive
        let cold = started && decision == nil
        let requestedIndex = index ?? PrePlaySelection.subtitleOff
        guard wasProgressive || index != selectedSubtitle
                || (cold && initialDecisionRequest.selection.subtitleIndex != requestedIndex)
        else { return }
        if Self.subtitleUsesOverlay(index, in: subtitles), player.isExternalPlaybackActive {
            showPlaybackNotice(Self.pgsOverlayExternalPlaybackNotice)
            return
        }
        if Self.subtitleBurnWouldDiscardHDR(
            index,
            tracks: subtitles,
            deliveredRange: deliveredRange
        ) {
            showPlaybackNotice(Self.hdrSubtitleNotice)
            return
        }
        // The refused notice is not cleared here: a retirement reason is a
        // property of the class, and `degraded` retires on its own five-second
        // clock or when the change this selection starts attaches (contract
        // §3.1). Nothing else about this selection changes.
        let actionEpoch = beginViewerAction()
        let activeOverlay = pgsOverlayTrackIndex
        selectedSubtitle = index
        refreshAutoCatalogForViewerRecipe()
        if cold {
            initialDecisionRequest.selection.subtitleIndex = requestedIndex
            let destination = seekState.absolute(
                positionForPlaybackIntent(), durationMs: knownDurationMs
            )
            currentMs = destination.target
            recipeRevision.change()
            restartInitialDecision(lifecycle: lifecycleGeneration)
            return
        }
        let route = Self.subtitleSelectionRoute(
            for: index,
            tracks: subtitles,
            activeBurn: activeBurnedSubtitle,
            isDirectPlayback: isDirectPlayback,
            activeOverlay: activeOverlay
        )
        let destination = seekState.selection(
            at: positionForPlaybackIntent(),
            durationMs: knownDurationMs,
            inPlace: route != .reopen
        )
        if route == .reopen { recipeRevision.change() }
        currentMs = destination.target
        // Set before the reopen is scheduled: the open it leads to reads this
        // to decide it may no longer direct-play, and it stays set for the rest
        // of the title, so turning subtitles off again costs no second restart.
        if let index, !Self.subtitleRequiresBurn(index, in: subtitles) {
            wantsNativeSubtitleRenditions = true
        }
        Task {
            retainControlSequence(await playbackControl.reportIntent())
            guard actionEpoch == viewerActionEpoch, selectedSubtitle == index else { return }
            guard destination.generation == seekState.generation else { return }
            await applySubtitleSelection(
                index,
                route: route,
                destination: destination,
                expectedActionEpoch: actionEpoch
            )
        }
    }

    /// Nonfatal playback feedback shares the red player banner with recovery
    /// messages, but not their lifetime: it is a `degraded` fault, which the
    /// contract times at five seconds — the same five seconds this used to
    /// schedule for itself. A second notice supersedes the first by recency
    /// while the older fault expires on its own clock, which is what
    /// "replacing the notice restarts its clock" always meant.
    func showPlaybackNotice(_ message: String) {
        raiseSurfaceNotice(source: "degraded_notice", context: surfaceContext, detail: message)
    }

    /// A server hold: the same banner, and the contract's thirty-second clock
    /// rather than a duration each call site passed for itself.
    func showPlaybackHold(_ message: String) {
        raiseSurfaceNotice(source: "control_hold", context: .attached, detail: message)
    }

    @discardableResult private func applySubtitleSelection(
        _ index: Int?,
        route: SubtitleSelectionRoute,
        destination: (target: Int, generation: Int)? = nil,
        expectedActionEpoch: Int
    ) async -> Bool {
        guard !Task.isCancelled, expectedActionEpoch == viewerActionEpoch else { return false }
        let selectionAttempt = snapshotAttempt()
        // Overlay teardown/installation changes visible presentation and
        // external-output behavior, so it belongs after the urgent intent
        // publication and stale-action guard just like the AVPlayer mutation.
        updatePGSOverlaySelection(index)
        switch route {
        case .reopen:
            let incumbent = player.currentItem
            var applied = false
            await reopen(at: destination?.target ?? positionForPlaybackIntent(), applied: { applied = $0 })
            return applied && !Task.isCancelled && attemptStillCurrent(selectionAttempt, fence: .subtitleReopenCompletion)
                && player.currentItem != nil && player.currentItem !== incumbent
        case .bitmapOverlay:
            // Moving from a native rendition to bitmap presentation must also
            // turn AVPlayer's legible option off. The synchronized layer then
            // owns the only subtitle pixels without replacing the video item.
            let applied = await applyNativeSubtitleSelection(
                nil,
                to: player.currentItem,
                expectedActionEpoch: expectedActionEpoch
            )
            if applied, Self.subtitleMutationIsCurrent(
                expectedActionEpoch: expectedActionEpoch,
                currentActionEpoch: viewerActionEpoch
            ) {
                settleInPlaceSubtitleDestination(destination)
                return true
            }
            return false
        case .mediaSelection:
            // Selection belongs to AVPlayerItem, not the HLS session. This is
            // the no-restart path that preserves video copy, HDR, position,
            // and the viewer's selected quality.
            let applied = await applyNativeSubtitleSelection(
                index,
                to: player.currentItem,
                expectedActionEpoch: expectedActionEpoch
            )
            if applied,
               Self.subtitleMutationIsCurrent(
                expectedActionEpoch: expectedActionEpoch,
                currentActionEpoch: viewerActionEpoch
               ) {
                settleInPlaceSubtitleDestination(destination)
                return true
            }
            return false
        }
    }

    private func settleInPlaceSubtitleDestination(
        _ destination: (target: Int, generation: Int)?
    ) {
        guard let destination,
              destination.generation == seekState.generation,
              destination.target == seekState.pendingMs
        else { return }
        if seekState.requiresMediaPresentation {
            // The subtitle command superseded the old seek task's action
            // epoch. Reissue its destination through the normal seek route;
            // selecting subtitles alone cannot prove that the target played.
            issueSeek(to: destination.target, generation: destination.generation)
            return
        }
        guard seekState.markExecuted(
                generation: destination.generation,
                targetMs: destination.target
              ),
              seekState.presentedInPlace(
                generation: destination.generation,
                targetMs: destination.target
              )
        else { return }
        currentMs = realPositionMs()
        playbackControlPlayerChanged()
        updateNowPlaying()
    }

    func selectAudio(_ index: Int) {
        guard index != selectedAudio else { return }
        let actionEpoch = beginViewerAction()
        let destination = seekState.absolute(
            positionForPlaybackIntent(),
            durationMs: knownDurationMs
        )
        currentMs = destination.target
        selectedAudio = index
        configureProgressiveCaptions()
        refreshAutoCatalogForViewerRecipe()
        recipeRevision.change()
        if started && decision == nil {
            // The selection-aware decision decides whether this track can
            // play directly, exactly as it does for a detail-screen choice.
            initialDecisionRequest.selection.audioIndex = index
            restartInitialDecision(lifecycle: lifecycleGeneration)
            return
        }
        audioOverride = index
        Task {
            retainControlSequence(await playbackControl.reportIntent())
            guard actionEpoch == viewerActionEpoch,
                  destination.generation == seekState.generation,
                  selectedAudio == index
            else { return }
            await reopen(at: destination.target)
        }
    }

    func selectQuality(_ height: Int?) {
        let incumbentQuality = playbackControlSelection().quality
        let incumbentRecipeAttached = !recipeRevision.needsReopen
        resetAutoQualityBudgetForViewer()
        autoDesiredCandidate = nil
        let cold = started && decision == nil
        let requestedQuality = height.map { Self.initialQuality(height: $0) } ?? .auto
        guard height != selectedHeight || selectedQualityIsOriginal || qualityChangeRetained
                || (cold && (initialDecisionRequest.quality != requestedQuality
                    || initialDecisionRequest.explicitHeight != height))
        else { return }
        let actionEpoch = beginViewerAction()
        // Whether the viewer already had a seek in the air. It decides whose
        // destination the pin below belongs to — theirs, or this change's —
        // and so which of the two the fallback must resume at.
        let carryingASeek = seekState.pendingMs != nil
        // The presentation destination every in-place selection creates
        // synchronously. It stays: the progress bar, the presentation monitor
        // and the stall-recovery suppression are all built on it.
        let destination = seekState.absolute(
            positionForPlaybackIntent(),
            durationMs: knownDurationMs
        )
        currentMs = destination.target
        manualQualityRetention.begin(.init(viewerEpoch: actionEpoch, incumbent: incumbentQuality,
            seekGeneration: destination.generation, carryingSeek: carryingASeek,
            incumbentRecipeAttached: incumbentRecipeAttached))
        qualityChangeRetained = false
        selectedHeight = height
        selectedQualityIsOriginal = false
        recipeRevision.change()
        if cold {
            initialDecisionRequest.quality = requestedQuality
            initialDecisionRequest.explicitHeight = height
            restartInitialDecision(lifecycle: lifecycleGeneration)
            return
        }
        Task {
            let prepared = await offerPreparedQualityChange()
            guard actionEpoch == viewerActionEpoch,
                  selectedHeight == height,
                  !selectedQualityIsOriginal
            else { return }
            // The prepared path owns this change now: it will either commit a
            // successor or fall back to exactly the reopen below. Doing both
            // would change the stream twice for one tap.
            guard !prepared else { return }
            if retainManualQualityFailure() { return }
            // Sampled here, not at the tap — and nil when a viewer seek has
            // taken the position meanwhile, because that seek carries the new
            // selection already and owns where the film lands.
            guard let target = QualityChangeReopen.target(
                seekGenerationAtTap: destination.generation,
                seekGenerationNow: seekState.generation,
                positionNowMs: QualityChangeReopen.positionNowMs(
                    pinnedAtTapMs: destination.target,
                    livePositionMs: liveFilmPositionMs(),
                    carryingASeek: carryingASeek
                )
            ) else { return }
            await reopen(at: target)
        }
    }

    private var hasHealthyQualityIncumbent: Bool {
        started && !isChangingStream && attachmentRecovery.establishedPlayback
            && player.currentItem?.status == .readyToPlay && player.currentItem?.error == nil
            && player.currentItem !== unprovenPreparedItem
    }

    @discardableResult
    private func retainManualQualityFailure() -> Bool {
        if qualityChangeRetained,
           manualQualityRetention.retained?.viewerEpoch == viewerActionEpoch { return true }
        guard let retained = manualQualityRetention.retain(viewerEpoch: viewerActionEpoch,
            incumbentHealthy: hasHealthyQualityIncumbent) else { return false }
        qualityChangeRetained = true
        // Desired selection stays saved; the revision now names the restored
        // standing wire recipe, which is still attached to this exact item.
        recipeRevision.didAttach(recipeRevision.desired)
        if seekState.generation == retained.seekGeneration {
            if retained.carryingSeek, let target = seekState.pendingMs {
                issueSeek(to: target, generation: retained.seekGeneration,
                    owningActionEpoch: viewerActionEpoch)
            } else {
                seekState.clear()
                currentMs = realPositionMs()
            }
        }
        Caps.PreparedHandoffTelemetry.shared.note(outcome: "retained current")
        showPlaybackNotice("Quality change did not complete. Playback continues. Retry or apply with restart in Quality.")
        playbackControl.reportEvidence()
        return true
    }

    func retryRetainedQuality() {
        guard qualityChangeRetained else { return }
        if selectedQualityIsOriginal { selectOriginalQuality() }
        else { selectQuality(selectedHeight) }
    }

    func applyRetainedQualityWithRestart() {
        guard qualityChangeRetained else { return }
        beginViewerAction()
        let restartAttempt = snapshotAttempt()
        manualQualityRetention.clear()
        qualityChangeRetained = false
        recipeRevision.change()
        Task { @MainActor [weak self] in
            guard let self else { return }
            self.retainControlSequence(await self.playbackControl.reportIntent())
            guard self.started,
                  self.attemptStillCurrent(restartAttempt, fence: .retainedQualityRestart) else { return }
            await self.reopen(at: self.positionForPlaybackIntent())
        }
    }

    /// The incumbent's own clock, or nil when it cannot honestly be read —
    /// nothing attached, or a replacement already in flight.
    ///
    /// Distinct from `positionForPlaybackIntent`, which answers with the
    /// destination the command in progress pinned. That is the right answer
    /// for building the next command on, and the wrong one for a change that
    /// has spent twelve seconds waiting while the picture kept playing.
    private func liveFilmPositionMs() -> Int? {
        guard !isChangingStream, player.currentItem != nil else { return nil }
        return realPositionMs()
    }

    func selectOriginalQuality() {
        guard selectedHeight != nil || !selectedQualityIsOriginal || qualityChangeRetained else { return }
        let incumbentQuality = playbackControlSelection().quality
        let incumbentRecipeAttached = !recipeRevision.needsReopen
        let actionEpoch = beginViewerAction()
        let carryingASeek = seekState.pendingMs != nil
        let destination = seekState.absolute(
            positionForPlaybackIntent(),
            durationMs: knownDurationMs
        )
        currentMs = destination.target
        manualQualityRetention.begin(.init(viewerEpoch: actionEpoch, incumbent: incumbentQuality,
            seekGeneration: destination.generation, carryingSeek: carryingASeek,
            incumbentRecipeAttached: incumbentRecipeAttached))
        qualityChangeRetained = false
        selectedHeight = nil
        selectedQualityIsOriginal = true
        recipeRevision.change()
        if started && decision == nil {
            initialDecisionRequest.quality = .original
            initialDecisionRequest.explicitHeight = nil
            restartInitialDecision(lifecycle: lifecycleGeneration)
            return
        }
        Task {
            let prepared = await offerPreparedQualityChange()
            guard actionEpoch == viewerActionEpoch,
                  selectedHeight == nil,
                  selectedQualityIsOriginal
            else { return }
            guard !prepared else { return }
            if retainManualQualityFailure() { return }
            guard let target = QualityChangeReopen.target(
                seekGenerationAtTap: destination.generation,
                seekGenerationNow: seekState.generation,
                positionNowMs: QualityChangeReopen.positionNowMs(
                    pinnedAtTapMs: destination.target,
                    livePositionMs: liveFilmPositionMs(),
                    carryingASeek: carryingASeek
                )
            ) else { return }
            await reopen(at: target)
        }
    }

    /// Publish the viewer's new selection, and — only where a prepared handoff
    /// could actually happen — wait briefly for the server to offer one.
    ///
    /// Returns true when the prepared path has taken ownership of this change,
    /// in which case the caller must not also reopen in place.
    ///
    /// VOD is not excluded. A prepared replacement is controlled by the
    /// explicit runtime capability and the server's real resource outcomes;
    /// delivery-rate telemetry may still be unknown on a new session without
    /// overriding the viewer's saved choice.
    ///
    /// The bound is `PreparedOfferWait.boundMs`, and it is not the stall ask's
    /// 1.5 seconds. The server spawns the candidate *after* it has answered
    /// the exchange that carried the ask, so a bound that only ever saw that
    /// one exchange's answer could not, even in principle, see a `prepare` —
    /// which is why every quality change reopened. This waits across
    /// exchanges, and it is affordable for a reason the stall ask's is not:
    /// the incumbent is still playing the whole time.
    private func offerPreparedQualityChange(boundary: AutoBoundaryAttempt? = nil) async -> Bool {
        let reporting = await playbackControl.isActivelyReporting
        guard Caps.controlCapabilities().dualPlayerPreparation,
              preparedReplacement.shouldAskForPreparation,
              // A paused viewer's change cannot be committed — no rate means
              // no new frame to prove the switch — so it is not worth a wait.
              wantsPlayback,
              reporting
        else {
            noteSurfaceLogOnly("prepared_offer_unavailable:capable=\(Caps.controlCapabilities().dualPlayerPreparation):active=\(preparedReplacement.hasActivePreparation):playing=\(wantsPlayback):reporting=\(reporting):seek_pending=\(seekState.pendingMs != nil)")
            retainControlSequence(await playbackControl.reportIntent())
            return false
        }
        // The floor `reportIntent` would have returned, read before the ask
        // publishes anything — `notifyUrgently` returns `sequence + 1`, and
        // `controlSequence` is nil until the first exchange, which is the same
        // floor of 1. Reading it after the ask instead would retain a *lower*
        // number whenever the ask bails without building a request, and order
        // the reopen's create as more superseded than it is.
        let floor = ((await playbackControl.controlSequence) ?? 0) + 1
        // M3. The tap, in wall time because the first frame is. `reopened()`
        // moves the incumbent's readings onto the before side of the switch
        // that is about to be asked for, so a second change in one session
        // cannot be measured against the first one's samples.
        preparedSwitch.reopened()
        preparedSwitch.note(tappedAtUnixMs: Int(Date().timeIntervalSince1970 * 1_000))
        sampleThePreparedSwitch()
        let actionEpoch = viewerActionEpoch
        let step = await playbackControl.awaitPreparedOffer(
            tappedAt: PlaybackControlSession.monotonicMs(),
            isSuperseded: { [weak self] in
                guard let self else { return true }
                return Task.isCancelled || self.viewerActionEpoch != actionEpoch ||
                    boundary.map { !self.autoBoundaryIsCurrent($0) } == true
            },
            publish: { [weak self] in self?.playbackControl.reportEvidence() }
        )
        retainControlSequence(max(floor, (await playbackControl.controlSequence) ?? 0))
        Caps.PreparedHandoffTelemetry.shared.note(
            preparation: playbackControl.latestPreparation
        )
        switch step {
        case .offered(let prepared):
            // Idempotent against the reporter's own return path, which offers
            // the same staging from `onPreparedReplacement`: whichever arrives
            // second is a replay and builds nothing. The offer decides whether
            // the prepared path actually took the change — a staging this
            // client has already settled, or one offered while a switch is in
            // flight, changes nothing, and returning true for those would drop
            // the viewer's tap.
            return preparedReplacement.offer(
                prepared, filmPositionMs: positionForPlaybackIntent()
            ).ownsTheChange
        case .reopen(let reason):
            noteSurfaceLogOnly("prepared_offer_ended:reason=\(Self.preparedOfferOutcome(reason)):seek_pending=\(seekState.pendingMs != nil)")
            Caps.PreparedHandoffTelemetry.shared.note(
                outcome: Self.preparedOfferOutcome(reason)
            )
            return false
        case .keepWaiting:
            // Unreachable: the wait only returns a terminal step. Taking the
            // ordinary in-place path is the honest answer to a step that says
            // nothing happened.
            return false
        }
    }

    /// The developer tab's wording for a wait that ended without an offer.
    /// Advisory only — nothing reads it back and nothing is gated on it.
    nonisolated static func preparedOfferOutcome(_ reason: String) -> String {
        switch reason {
        case "timed_out": return "timed out"
        case "declined": return "declined"
        case "superseded": return "superseded"
        case "owner_changed": return "owner changed"
        default: return reason
        }
    }

    /// The link rate AVFoundation itself measured, in bits per second.
    ///
    /// The server records the client's *observed* throughput as advisory
    /// evidence about link conditions. It is not a prepared-handoff gate.
    ///
    /// `observedBitrate` on the newest access-log event is AVFoundation's own
    /// empirical download rate, not the variant's declared
    /// `indicatedBitrate` — the same distinction the web client makes by
    /// sending `hls.bandwidthEstimate` rather than the level's bitrate.
    ///
    /// Nil rather than a number whenever AVFoundation has not measured one:
    /// the access log is empty before the first segment lands, and it reports
    /// `-1` for "unknown". The server retains the reading as advisory
    /// telemetry; inventing a rate would make the observation dishonest even
    /// though missing or low values no longer gate prepared handoff.
    private func accessLogSample(for item: AVPlayerItem) -> PlayerAccessLogSample? {
        guard started, player.currentItem === item else { return nil }
        let owner = PlayerAccessLogOwner(item: ObjectIdentifier(item), session: sessionId, lifecycle: lifecycleGeneration)
        let source = PlayerAccessLogItem(item)
        return accessLogSampler.value(for: owner) { source.read() }
    }

    private func observedDownloadBitsPerSecond(_ item: AVPlayerItem) -> Int64? {
        guard let event = accessLogSample(for: item)?.values else { return nil }
        return Self.observedDownloadBps(fromObservedBitrate: event.observedBitrate)
    }

    /// The reportable form of one `observedBitrate` reading.
    ///
    /// Split from the access-log lookup so the clamp is testable without an
    /// `AVPlayerItem`: the lookup is AVFoundation's, the arithmetic is ours,
    /// and it is the arithmetic that can crash or silence the reporter.
    /// `nonisolated` because it is arithmetic, not state: `PlayerController`
    /// is `@MainActor`, so a plain static would inherit that isolation and be
    /// unreachable from a synchronous test — which is the whole reason it was
    /// split out.
    nonisolated static func observedDownloadBps(fromObservedBitrate observed: Double) -> Int64? {
        guard observed.isFinite, observed > 0 else { return nil }
        // Clamped, for two reasons that both start in the same place: a
        // near-instantaneous response — a cached segment, a local proxy —
        // makes AVFoundation's bytes-over-duration blow up.
        //
        // `Int64(_: Double)` **traps** above `Int64.max`, so an absurd rate
        // would crash the app rather than being ignored. One order down is
        // worse because it is silent: the protocol caps this field at 1e13 and
        // `PlaybackControlSnapshot.isValid` rejects the *whole snapshot*, so
        // an over-cap rate stops the reporter sending anything and the
        // session's control plane goes quiet until its lease expires. One
        // unreliable field must not cost the exchange.
        return Int64(min(observed.rounded(), maximumObservedDownloadBps))
    }

    /// The protocol's own ceiling for `observed_download_bps`.
    nonisolated private static let maximumObservedDownloadBps: Double = 10_000_000_000_000

    /// Film position that a new viewer command should build on. Between
    /// commands the live AVPlayer clock is most precise; during a seek or
    /// replacement, only the optimistic target / last published film time is
    /// in the right timeline.
    private func positionForPlaybackIntent() -> Int {
        if let pending = seekState.pendingMs { return pending }
        return isChangingStream || player.currentItem == nil ? currentMs : realPositionMs()
    }

    /// Closing during preparation must retain the requested position rather
    /// than publish the zero (or brief lead-in) of an unprepared item.
    func positionForPlaybackStop() -> Int {
        currentPausedRetirement?.positionMs ?? positionForPlaybackIntent()
    }

    /// Report the final position and hand any encoder back immediately.
    private func startAudioSessionObservation() {
        audioSessionObserver.start(
            wantsPlayback: { [weak self] in self?.wantsPlayback == true },
            receive: { [weak self] event in self?.handleAudioSessionEvent(event) }
        )
    }

    func handleAudioSessionEvent(_ event: PlaybackAudioSessionObserver.Event) {
        switch event {
        case .interruption(.suspend):
            if !systemPaused { systemPauseBeganAt = resumeNow() }
            systemPaused = true
            player.pause()
            isPlaying = false
            invalidateResumeAttempt(outcome: "interrupted")
            present(.systemPaused(true))
        case .interruption(.resume):
            let wasSystemPaused = systemPaused
            systemPaused = false
            present(.systemPaused(false))
            // A late `.ended` after the viewer's own press already ended the
            // hold must not start a rate ahead of that resume's owner.
            guard wasSystemPaused, wantsPlayback, started, player.currentItem != nil,
                  !isChangingStream, resumeAttempt == nil else { return }
            Self.applyPlaybackCommand(to: player, preferredRate: preferredRate, immediately: false)
            isPlaying = true
        case .interruption(.stay):
            let wasSystemPaused = systemPaused
            systemPaused = false
            present(.systemPaused(false))
            guard wasSystemPaused, wantsPlayback else { return }
            // iOS declined automatic resume. The system suspension is over,
            // but the transport is still paused, so make the visible intent
            // agree: the next on-screen or remote Play is one real resume.
            settleSystemHoldAsPause()
        case .interruption(.ignore):
            break
        case .routeChange(let revokesIntent):
            guard revokesIntent else { return }
            systemPaused = false
            present(.systemPaused(false))
            setPlaybackRequested(false, viewerOrigin: false)
        }
    }

    /// A system hold is over and the transport is still paused: make the
    /// visible intent agree, so the next Play is one ordinary resume — through
    /// the paused-retirement latch and the bounded resume like any other.
    private func settleSystemHoldAsPause() {
        wantsPlayback = false
        isPlaying = false
        pauseBeganAt = systemPauseBeganAt ?? resumeNow()
        systemPauseBeganAt = nil
        explicitViewerPause = nil
        player.pause()
    }

    /// iOS does not promise an `.ended` for every `.began`, so a hold that only
    /// `.ended` could clear can outlive the interruption indefinitely — and
    /// with it the stall monitor, which `systemPaused` switches off. A viewer's
    /// transport press is the end of it: retire the hold, take the audio
    /// session back for a Play, and leave the press to the ordinary setter.
    private func endSystemHoldForViewer(play: Bool) {
        #if os(iOS)
        if play {
            do {
                try AVAudioSession.sharedInstance().setActive(true)
            } catch {
                // The interruption is still live (a call iOS will not yield
                // to): the hold is true, so it stays, and so does its banner.
                noteSurfaceLogOnly("system_hold_kept:activation_refused")
                return
            }
        }
        #endif
        systemPaused = false
        present(.systemPaused(false))
        noteSurfaceLogOnly("system_hold_ended:viewer")
        if play, wantsPlayback { settleSystemHoldAsPause() }
        systemPauseBeganAt = nil
    }

    private func applyDisplayCriteria(for item: AVPlayerItem, generation: Int) {
        #if os(tvOS)
        let applied = PlaybackDisplayCriteria.apply(
            item: item,
            itemIsCurrent: player.currentItem === item,
            openIsCurrent: !isSuperseded(generation)
        )
        if applied {
            NSLog("plurx display_criteria_applied %@", String(describing: item.asset.preferredDisplayCriteria))
        }
        #endif
    }

    func stop(deactivateAudioSession: Bool = true) {
        progressiveCaptionTask?.cancel(); progressiveCaptionTask = nil
        progressiveCaptions.retire()
        subtitleTextTranslation.retire()
        let position = positionForPlaybackStop()
        recordDiagnosticPlayback("terminal", action: "stop")
        diagnosticAuthorizationGeneration = nil
        cancelAutoCostCatalogRefresh()
        accessLogSampler.invalidate()
        lastPreparedAccessLogSequence = nil
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        playbackEnhancement = nil
        retireAutoTransferMetrics()
        abandonSeekMeasurement()
        requestedSeekGeneration = nil
        resetSurface()
        surfaceLifecycleObservation.removeAll()
        let wasStarted = started
        manualQualityRetention.clear()
        qualityChangeRetained = false
        pendingPreparedControl = nil
        playbackControlOrigin = nil
        unprovenPreparedItem = nil
        started = false
        lifecycleGeneration &+= 1
        loadingTask?.cancel()
        loadingTask = nil
        initialDecisionGeneration &+= 1
        initialDecisionDeadlineTask?.cancel()
        initialDecisionDeadlineTask = nil
        // Invalidate every in-flight open before any of its awaits can attach
        // a replacement item after this teardown.
        openGeneration &+= 1
        reopenQueue.clear()
        recipeRevision.clear()
        activeMediaPath = nil
        activeNativeSubtitle = nil
        Session.shared.resetMediaFailover()
        if let timeObserver {
            player.removeTimeObserver(timeObserver)
            self.timeObserver = nil
        }
        interactiveSeekTask?.cancel()
        interactiveSeekTask = nil
        resumeIntentTask?.cancel()
        resumeIntentTask = nil
        invalidateResumeAttempt(outcome: "stopped")
        pauseBeganAt = nil
        explicitViewerPause = nil
        autoBoundaryReplan.clear()
        seekPresentationTask?.cancel()
        seekPresentationTask = nil
        seekPresentationLifecycle.removeAll()
        seekVideoOutput?.setDelegate(nil, queue: nil)
        seekVideoOutput = nil
        seekOutputWakeup = nil
        restorePreparedIncumbentBufferPreference()
        viewerActionEpoch &+= 1
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        latestAutoCompletedTransfer = nil
        autoMildSamples = 0
        pendingControlSequence = nil
        itemObserver?.cancel()
        itemObserver = nil
        itemEventTask?.cancel()
        itemEventTask = nil
        lastItemFailureDetail = nil
        statusTask?.cancel()
        statusTask = nil
        recoveryTask?.cancel()
        recoveryTask = nil
        audioSessionObserver.stop()
        systemPaused = false
        systemPauseBeganAt = nil
        surfaceClockTask?.cancel()
        surfaceClockTask = nil
        clearPGSOverlaySelection()
        pgsOverlayItemGeneration &+= 1
        // Before the reporter goes, not after: a staging still live is owed
        // `aborted`, and the second player must be freed whether or not that
        // exchange makes it out. A settlement that misses its window costs the
        // server one preparation slot on a session that is ending anyway; a
        // leaked `AVPlayer` costs the device.
        preparedReplacement.playerIsEnding()
        playbackControl.end()
        playbackControlSummary = nil
        // A verdict survives a reopen because the failure it explains usually
        // arrives after one. It must not survive the title: a confident
        // sentence about the wrong film is worse than a generic one.
        playbackControl.clearVerdict()
        controlWaitingSince = nil
        controlObservationOverride = nil
        controlRenderOverride = nil
        controlEvidencePositionMs = nil
        playbackRecoveryMonitor.reset()
        stallReopenBudget.reset()
        sessionHeight = nil
        deliveryStarvation.reset()
        ttffMeasurement.reset()
        seekState.clear()
        pausedRetirement = nil
        player.pause()
        player.replaceCurrentItem(with: nil)
        #if os(tvOS)
        PlaybackDisplayCriteria.activeManager()?.preferredDisplayCriteria = nil
        #endif
        isPlaying = false
        wantsPlayback = false
        isChangingStream = false
        finished = false
        sessionStatus = nil
        diagnosticSessionStatus = nil
        diagnosticSessionStatusObservedAt = nil
        preparationSample = nil
        if wasStarted { report(position) }
        if let sessionId {
            self.sessionId = nil
            let model = model
            let releaseHlsSession = self.releaseHlsSession
            Task { if let model { await releaseHlsSession(model, sessionId) } }
        }
        let wasRemoteOwner = RemoteCommandOwner.shared.isCurrent(remoteOwnerToken)
        removeRemoteCommands()
        if wasRemoteOwner {
            MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
            MPNowPlayingInfoCenter.default().playbackState = .stopped
        }
        lastNowPlayingState = nil
        #if os(iOS)
        offlineId = nil
        offlineAssetURL = nil
        if wasStarted {
            if deactivateAudioSession {
                try? AVAudioSession.sharedInstance().setActive(
                    false,
                    options: .notifyOthersOnDeactivation
                )
            }
        }
        #endif
    }

    /// True film position, including an HLS session's server-side start base.
    func realPositionMs() -> Int {
        let seconds = player.currentTime().seconds
        let local = seconds.isFinite ? Int(seconds * 1000) : 0
        return baseMs + max(local, 0)
    }

    // MARK: - Stream lifecycle

    private func isCurrentLifecycle(_ generation: Int) -> Bool {
        started && lifecycleGeneration == generation
    }

    /// The nine epochs and the attached item, read in one turn on the main
    /// actor, so a continuation about to `await` can record which state it
    /// depends on and compare only that subset when it wakes.
    ///
    /// Taking all nine costs nine integer reads and answers the question a
    /// caller actually has, which is "did the thing *I* depend on move?" —
    /// `Attempt.stillCurrent(_:scopes:)` is where the subset is named. Reading
    /// them together also makes the pair of snapshots comparable: both are
    /// taken with no suspension inside them, so neither can be half of one
    /// state and half of another.
    func snapshotAttempt() -> Attempt {
        Attempt(
            lifecycle: lifecycleGeneration,
            open: openGeneration,
            viewerAction: viewerActionEpoch,
            initialDecision: initialDecisionGeneration,
            createRetry: createRetryEpoch,
            preparedAlignment: preparedAlignmentGeneration,
            seek: seekState.generation,
            pgsSelection: pgsOverlaySelectionGeneration,
            pgsItem: pgsOverlayItemGeneration,
            item: player.currentItem.map(ObjectIdentifier.init)
        )
    }

    /// Whether the work `captured` was taken for still belongs to anyone,
    /// judged on `fence`'s own scope set and nothing else. Every migrated
    /// fence goes through here rather than calling `stillCurrent` with a
    /// literal set, so the set lives in one table (`AttemptFence.scopes`) that
    /// a test and the census can both hold still.
    ///
    /// A refusal is the one moment the controller knows *which* epoch moved
    /// under a suspended continuation, so it is logged there as the plan's
    /// `attempt_stale` client-log event (§4) instead of returning silently:
    /// a late recovery that quietly did nothing is otherwise indistinguishable
    /// in a device log from one that was never scheduled.
    func attemptStillCurrent(_ captured: Attempt, fence: AttemptFence) -> Bool {
        let now = snapshotAttempt()
        if captured.stillCurrent(now, scopes: fence.scopes) { return true }
        let detail = Self.attemptStaleDetail(fence: fence, captured: captured, now: now)
        lastAttemptStaleDetail = detail
        reportAttemptStale(detail: detail)
        return false
    }

    /// The `detail` of an `attempt_stale` event: which fence refused, which of
    /// its scopes moved (in `Attempt.Scope` declaration order, so the value is
    /// bounded by the nine scope names), and whether the attached item was
    /// replaced as well. The item is reported, never compared: it is not a
    /// scope.
    static func attemptStaleDetail(fence: AttemptFence, captured: Attempt, now: Attempt) -> String {
        let scope = captured.staleScopes(now, scopes: fence.scopes)
            .map(\.rawValue)
            .joined(separator: ",")
        let item = captured.hasSameItem(as: now) ? "same" : "replaced"
        return "fence=\(fence.rawValue) scope=\(scope) item=\(item)"
    }

    private func reportAttemptStale(detail: String) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        postClientLog(AppleAttemptStaleLog(
            message: "a suspended continuation was refused because an epoch it depends on moved",
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            detail: detail
        ))
    }

    private func restartInitialDecision(lifecycle: Int) {
        guard isCurrentLifecycle(lifecycle), decision == nil else { return }
        loadingTask?.cancel()
        initialDecisionDeadlineTask?.cancel()
        initialDecisionGeneration &+= 1
        let generation = initialDecisionGeneration
        let request = initialDecisionRequest
        let file = fileId
        let start = positionForPlaybackIntent()
        loadingTask = Task {
            await load(startMs: start, lifecycle: lifecycle, generation: generation,
                       file: file, request: request)
        }
        initialDecisionDeadlineTask = Task {
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
                  initialDecisionGeneration == generation, decision == nil else { return }
            do { try await waitInitialDecisionDeadline() } catch { return }
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
                  initialDecisionGeneration == generation, decision == nil
            else { return }
            initialDecisionGeneration &+= 1
            loadingTask?.cancel()
            fail(APIError.transport("Playback decision did not complete in time. Try again."))
        }
    }

    private static func initialQuality(height: Int) -> PlaybackQuality {
        // The decision endpoint accepts only a transcode category; the exact
        // (including non-menu) height is retained for the session request.
        PlaybackQuality(rawValue: String(height)) ?? .p1080
    }

    private func load(
        startMs: Int, lifecycle: Int, generation: Int,
        file: Int, request: InitialDecisionRequest
    ) async {
        guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
              initialDecisionGeneration == generation, let model else { return }
        cancelAutoCostCatalogRefresh()
        accessLogSampler.invalidate()
        lastPreparedAccessLogSequence = nil
        let layoutAttempt = snapshotAttempt()
        do {
            // Allow the mounted surface to report its first layout; no panel/model
            // estimate substitutes for a hidden or unmeasured container.
            if model.displayAwareAuto {
                for _ in 0..<25 where presentationTarget == nil {
                    try await Task.sleep(for: .milliseconds(10))
                }
            }
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
                  attemptStillCurrent(layoutAttempt, fence: .autoInitialLayout) else { return }
            let target = presentationTarget
            let playbackDecision = try await Caps.PresentationContext.$target.withValue(target) {
                try await requestPlaybackDecision(model, try contextForFile(file), request.selection, request.quality)
            }
            let decision = playbackDecision.decision
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
                  initialDecisionGeneration == generation else { return }
            initialDecisionDeadlineTask?.cancel()
            initialDecisionDeadlineTask = nil
            decisionCaps = playbackDecision.caps
            self.decision = decision
            autoActiveCandidateId = decision.qualityCandidateId
            autoRouteProtocol = decision.displayAwareAutoProtocol
            if knownDurationMs <= 0 { knownDurationMs = decision.source?.durationMs ?? 0 }
            // The standing Quality preference becomes this playback's rung
            // now that the ladder says which rungs exist for this source. An
            // explicit `initialHeight` (debug acceptance) is a stronger
            // answer and is left alone.
            selectedHeight = request.explicitHeight ?? Self.startingHeight(
                for: request.quality, ladder: decision.ladder ?? []
            )
            selectedQualityIsOriginal = request.quality == .original
            if request.quality == .auto, selectedHeight == nil,
               model.displayAwareAuto, model.displayAwareAutoProtocol == "route-v1",
               decision.displayAwareAutoProtocol == "route-v1" {
                autoDesiredCandidate = decision.qualityCandidates?.first {
                    $0.hasValidIdentity && $0.id == decision.qualityCandidateId
                }
            }
            // `default` on a decision track is the server's own shared-policy
            // pick, not the muxer's flag (crates/plurxd http/stream.rs
            // overwrites both lists from `select_tracks`) — and for a
            // selection-aware request it is the *effective* pick, so a pre-play
            // audio choice already arrives marked here.
            selectedAudio = request.selection.audioIndex
                ?? decision.audio?.first(where: { $0.default })?.index
            configureProgressiveCaptions()
            let wantedSubtitle = Self.initialSubtitleIndex(
                prePlay: request.selection.subtitleIndex,
                serverSelection: decision.selection,
                tracks: decision.subtitles ?? [],
                deviceSubtitlesOff: model.subLang == "off"
            )
            let blockedByHDR = Self.startingSubtitleBlockedByHDR(
                prePlaySubtitle: request.selection.subtitleIndex,
                wanted: wantedSubtitle,
                serverSelection: decision.selection,
                tracks: decision.subtitles ?? [],
                deliveredRange: decision.deliveredDynamicRange
            )
            selectedSubtitle = blockedByHDR ? nil : wantedSubtitle
            // Say so rather than starting silently with no subtitles: the
            // viewer asked for these on the detail screen.
            if blockedByHDR, prePlaySelection.subtitleIndex != nil {
                showPlaybackNotice(Self.hdrSubtitleNotice)
            }
            // A native pre-play choice enters the session on this very open, so
            // record it the way `selectSubtitle` does — otherwise turning
            // subtitles off later would drop back to direct play and charge the
            // next selection a restart this path already paid for. Scoped to
            // the pre-play arm so the automatic path keeps its behaviour.
            if prePlaySelection.subtitleIndex != nil,
               let selectedSubtitle,
               !Self.subtitleRequiresBurn(selectedSubtitle, in: decision.subtitles ?? []) {
                wantsNativeSubtitleRenditions = true
            }
            updatePGSOverlaySelection(selectedSubtitle)
            // Use the same drain as later replacements. A remote command can
            // arrive while the decision request or first item is preparing;
            // opening directly here left that command stranded in the reopen
            // queue forever. A command issued before the decision arrived is
            // already represented by `seekState.pendingMs`.
            let initialPosition = seekState.pendingMs ?? startMs
            try await openAndDrain(
                decision: decision,
                request: PlayerReopenRequest(positionMs: initialPosition)
            )
        } catch {
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle),
                  initialDecisionGeneration == generation else { return }
            initialDecisionDeadlineTask?.cancel()
            initialDecisionDeadlineTask = nil
            reopenQueue.clear()
            // The request failed, not the viewer's retained destination.
            fail(error)
        }
    }

    private func reopen(at position: Int, intent: PlayerOpenIntent = .normal,
                        applied: ((Bool) -> Void)? = nil) async {
        var attached = false
        defer { applied?(attached) }
        #if os(iOS)
        if offlineId != nil {
            await reloadOffline(at: position)
            return
        }
        #endif
        // A growing EVENT playlist can momentarily announce its current end,
        // and several UI actions can also request a restart. Never overlap two
        // server-session replacements (see `PlayerReopenQueue`) — they share a
        // playback ID, so the newer request intentionally removes the older
        // session and can otherwise strand AVPlayer on a URL the server has
        // just deleted — but never discard one either: a request that lands
        // mid-change is remembered and replayed as the single trailing reopen.
        guard let decision, started else { return }
        let lifecycle = lifecycleGeneration
        guard let next = reopenQueue.request(
            position,
            intent: intent,
            changeInFlight: isChangingStream
        ) else {
            return
        }
        do {
            try await openAndDrain(decision: decision, request: next)
            attached = isCurrentLifecycle(lifecycle) && !Task.isCancelled
                && !isChangingStream && !recipeRevision.needsReopen
        } catch {
            guard isCurrentLifecycle(lifecycle) else { return }
            reopenQueue.clear()
            if player.currentItem != nil,
               !(error is PlaybackPreparationError && seekState.pendingMs != nil) {
                seekState.clear()
                currentMs = realPositionMs()
            }
            fail(error)
        }
    }

    /// Open, then honor whatever queued behind that open. A loop rather than
    /// recursion, so "exactly one trailing reopen per completed change" stays
    /// true however long a burst of step-seeks runs. Nothing awaits between a
    /// finished `open` and the next one, so no other request can observe the
    /// gap and start a competing replacement.
    ///
    /// Each drained request carries its own cause, so a same-delivery repair is
    /// issued exactly once. Whatever this loop replays next is either a viewer
    /// command or a genuinely new stall; one episode can never mutate twice.
    private func openAndDrain(decision: Decision, request: PlayerReopenRequest) async throws {
        let lifecycle = lifecycleGeneration
        var request = request
        while true {
            let next = request.positionMs
            try await open(decision: decision, at: next, intent: request.intent)
            guard isCurrentLifecycle(lifecycle) else { return }
            // A newer `open()` superseded this one while it ran (P2-6). The
            // transition — and with it `isChangingStream`, which only the
            // owning open clears — belongs to that newer attempt, so the queued
            // request is its to drain and not this loop's.
            guard !isChangingStream else { return }
            guard let trailing = reopenQueue.takePending() else {
                if recipeRevision.needsReopen {
                    request = PlayerReopenRequest(positionMs: positionForPlaybackIntent())
                    continue
                }
                let seekGeneration = seekState.generation
                if seekState.markExecuted(generation: seekGeneration, targetMs: next) {
                    beginSeekPresentationMonitor(generation: seekGeneration, targetMs: next)
                }
                return
            }
            request = trailing
        }
    }

    private func open(
        decision: Decision,
        at startMs: Int,
        intent: PlayerOpenIntent = .normal
    ) async throws {
        guard let model, started else { return }
        let lifecycle = lifecycleGeneration
        let requestedFile = fileId
        openGeneration &+= 1
        let generation = openGeneration
        let requestedRecipeRevision = recipeRevision.desired
        let seekOwner = seekPreparationOwner(at: startMs)
        let requestedHeight: Int?
        let requestedOriginal: Bool
        switch manualQualityRetention.retained?.incumbent {
        case .manual(let height): requestedHeight = height; requestedOriginal = false
        case .original: requestedHeight = nil; requestedOriginal = true
        case .auto, .autoCandidate: requestedHeight = nil; requestedOriginal = false
        case nil: requestedHeight = selectedHeight; requestedOriginal = selectedQualityIsOriginal
        }
        let requestedAudioOverride = audioOverride
        // A fresh stream starts at the head of the node list. Without this,
        // one film's failover leaves the index advanced for every film after
        // it in the same process, and the second one has no node left to try.
        Session.shared.resetMediaFailover()
        let attemptId = UUID().uuidString
        finished = false
        attachmentRecovery.opened(at: startMs)
        blackFrameWatchdog.opened()
        isChangingStream = true
        // Nothing is cleared here any more. A fault about the generation this
        // open replaces is dropped the moment the successor attaches, which is
        // identity rather than a call site remembering to tidy up; a fault
        // about the pending change is retired when the change settles.
        // P2-5: a reopen must not un-pause a viewer who paused before changing
        // audio, quality, or a burned subtitle — and must not pause one whose
        // player merely happens to be stopped right now, which is the state a
        // buffering item and a failed item both present.
        // What `restoreAfterFailedChange` has to put back if no successor comes
        // into existence: a stream that is still worth watching, and the
        // viewer's own transport intent rather than the player's observed
        // state, which reads as paused while an item buffers (P2-5).
        player.pause()
        // The session this open replaces. It is retired only once its successor
        // exists: releasing first meant a failed create left the viewer's item
        // pointing at a playlist this client had just deleted — buffered
        // runway, then a stall, with `fail()` deliberately quiet because an
        // item still existed. Its telemetry poll stops now; the session itself
        // does not.
        let superseded = sessionId
        let recoveryCause = autoRecoveryCause.flatMap { ticket in
            ticket.isCurrent(session: superseded, attachment: player.currentItem.map(ObjectIdentifier.init),
                             candidate: autoActiveCandidateId, proposed: autoDesiredCandidate?.id,
                             nowMs: PlaybackControlSession.monotonicMs()) && requestedHeight == nil && !requestedOriginal
                ? ticket : nil
        }
        stopStatusPolling()

        let normalMode = Self.playbackMode(decision)
        let preserveDolbyVision = Self.shouldPreserveDolbyVision(decision)
            && !forceCompatibleHDRBase
        // Captured once: `selectedSubtitle` may change while the awaits below
        // run, and the difference is reconciled when this open completes (P1-2).
        let requestedSubtitle = selectedSubtitle
        let subtitleFields = Self.sessionSubtitleFields(
            selected: requestedSubtitle,
            tracks: subtitles
        )
        let burnSubtitle = subtitleFields.burn
        let nativeSubtitle = subtitleFields.native
        let forceTranscode = burnSubtitle != nil || requestedHeight != nil || forceCompatibilityTranscode
        // A direct multi-audio asset lets AVFoundation choose by language,
        // which cannot identify duplicate-language/commentary tracks. Bind
        // the server's actual audio ordinal from the first attachment instead.
        let customAudio = requestedAudioOverride != nil
        let ordinalBoundAudio = Self.needsOrdinalBoundAudioSession(
            audioTrackCount: audioTracks.count, explicitAudio: requestedAudioOverride
        )
        // Whether this open has to be a session for no reason other than making
        // the file's text subtitles selectable. Sticky once true, so leaving
        // subtitles again does not buy a second restart.
        let needsSubtitleRenditions = Self.needsNativeSubtitleSession(
            hasNativeTextTrack: subtitles.contains(where: \.isNativeHLS),
            readiness: subtitleReadiness,
            subtitlesInUse: wantsNativeSubtitleRenditions
        )
        canRetryCurrentItemWithTranscode = normalMode != "transcode" && !forceTranscode && !customAudio
        canRetryCurrentItemWithHDRBase = false
        // P2-7, decided by Paul on 2026-08-02 (plan §2.5): stay direct until
        // the first native selection. Merely *having* native text tracks no
        // longer abolishes true direct play — every such file used to become a
        // copy session, and on Bedroom that path degrades to a compatibility
        // transcode. Entering the session is deferred to the moment a native
        // subtitle is actually chosen, which costs exactly one reopen there
        // (routed explicitly by `subtitleSelectionRoute`).
        let direct = normalMode == "direct" && !forceTranscode && !ordinalBoundAudio
            && nativeSubtitle == nil && !needsSubtitleRenditions
        let url: URL?
        var seekAfterAttach: Int?
        var nextBaseMs = 0

        // Only automatic stall recovery is bounded. Every other open — a seek,
        // a quality/audio/subtitle change, a compatibility retry — is fresh
        // intent that builds its own session, so it retires the floor budget
        // rather than inheriting a starved predecessor's exhausted one.
        if case .normal = intent { stallReopenBudget.reset() }

        if direct {
            activeBurnedSubtitle = nil
            isDirectPlayback = true
            // Direct play has no session and therefore no rung.
            sessionHeight = nil
            nextBaseMs = 0
            usesDirectTimeline = true
            isVOD = true
            encoder = nil
            // Direct play has no session, so the decision's answer stands for
            // the whole playback (MEDIA-BADGES-PLAN.md §3.2). Both halves of
            // it, for the reason `adoptSessionDelivery` gives.
            effectiveProcessing = nil
            effectiveProcessingGeneration = nil
            playbackEnhancement = nil
            deliveredRange = decision.deliveredDynamicRange
            deliveredDolbyVisionProfile = decision.deliveredDolbyVisionProfile
            let deliveryPath = try contextForFile(fileId).translatedDeliveryPath(decision.delivery?.url ?? decision.playUrl)
            activeMediaPath = clusterRelativeMediaPath(deliveryPath)
            activeMediaAuthenticated = true
            activeNativeSubtitle = nativeSubtitle
            url = Session.shared.mediaURL(deliveryPath)
            if startMs > 0 { seekAfterAttach = startMs }
        } else {
            guard let decisionCaps else {
                throw APIError.transport("Playback decision capabilities were not retained.")
            }
            let copy = !forceTranscode
                && (normalMode == "direct" || normalMode == "remux" || customAudio)
            canRetryCurrentItemWithHDRBase = copy
                && preserveDolbyVision
                && Self.hasCompatibleDolbyVisionBase(decision.source?.hdrFormat)
            // The checkmark comes from `/decision`'s shared-policy pick. Carry
            // that exact track into the initial session too: relying on an
            // omitted audio field made copy-video HLS fall back to the muxer's
            // first stream, so the UI could say English while Italian played.
            // A viewer's later explicit choice remains the stronger value.
            let chosenAudio = Self.sessionAudioIndex(
                explicit: requestedAudioOverride,
                plan: decision.delivery?.audio,
                selected: selectedAudio
            )
            let aac = copy ? Self.needsAAC(audioIndex: chosenAudio, decision: decision)
                : nil
            let reporterControlSequence = await readControlSequence(playbackControl)
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle), !isSuperseded(generation) else { return }
            let orderedControlSequence = [reporterControlSequence, pendingControlSequence]
                .compactMap { $0 }
                .max()
            var body = Self.applyOpenIntent(
                to: CreateSessionRequest(
                    playbackId: playbackId,
                    controlSequence: orderedControlSequence,
                    height: Self.burnSessionHeight(
                        burnSubtitle: burnSubtitle,
                        mode: normalMode,
                        selectedHeight: requestedHeight,
                        sourceHeight: decision.source?.height
                    ),
                    start: Double(startMs) / 1000.0,
                    audio: chosenAudio,
                    subtitleBurn: burnSubtitle,
                    // No SDR acknowledgement. The server computes the grade
                    // this session would deliver without the burn and decides
                    // for itself; a client's assertion about that grade was
                    // never evidence about it, and this one was computed from
                    // the decision's range rather than the session's anyway.
                    subtitleBurnSDR: nil,
                    nativeSubtitles: true,
                    subtitle: nativeSubtitle,
                    copy: copy ? true : nil,
                    aac: copy ? aac : nil,
                    preserveDolbyVision: copy ? preserveDolbyVision : nil,
                    hdr10: Self.sessionHDR10Request(
                        copy: copy,
                        deliveredRange: decision.deliveredDynamicRange,
                        forcesSDR: forceCompatibilityTranscode || burnSubtitle != nil
                    ),
                    caps: decisionCaps
                ),
                intent: intent,
                currentSessionId: superseded,
                selectedHeight: requestedHeight,
                qualityIsOriginal: requestedOriginal
            )
            if let recoveryCause, autoRecoveryCause?.observedAtMs == recoveryCause.observedAtMs,
               recoveryCause.isCurrent(session: superseded, attachment: player.currentItem.map(ObjectIdentifier.init),
                   candidate: autoActiveCandidateId, proposed: autoDesiredCandidate?.id,
                   nowMs: PlaybackControlSession.monotonicMs()) {
                body.previousSessionId = recoveryCause.session
                body.reopenReason = recoveryCause.cause.rawValue
                body.requestId = UUID().uuidString
                autoRecoveryCause = nil
            }
            if model.displayAwareAuto, model.displayAwareAutoProtocol == "route-v1",
               decision.displayAwareAutoProtocol == "route-v1" {
                if var caps = body.caps {
                    caps.display.presentationTarget = presentationTarget
                    body.caps = caps
                }
                let selection = playbackControlSelection()
                let envelopeSelection = MediaIntentSelection(quality: selection.quality, codec: selection.codec,
                    dynamicRange: selection.dynamicRange, audioTrack: selection.audioTrack,
                    audioOffsetMs: selection.audioOffsetMs, subtitles: selection.subtitle)
                if let prior = autoEnvelopeSelection, prior != envelopeSelection {
                    autoRecipeRevision = min(autoRecipeRevision + 1, 9_007_199_254_740_991)
                }
                autoEnvelopeSelection = envelopeSelection
                body.intent = MediaIntentEnvelope(lifetimeId: playbackId,
                    recipeRevision: autoRecipeRevision, destinationRevision: autoDestinationRevision,
                    transportRevision: autoTransportRevision, selection: envelopeSelection)
                if let candidate = autoDesiredCandidate, selectedHeight == nil, !selectedQualityIsOriginal {
                    body.height = (PlaybackControl.minimumHeight...PlaybackControl.maximumHeight).contains(candidate.targetHeight) ? candidate.targetHeight : nil
                    body.qualityAuto = true
                }
            }
            // The rung this open is stepping down *from*, read before the
            // successor overwrites it. Only a strictly lower answer proves the
            // ladder still has somewhere to go.
            let previousHeight = sessionHeight
            let hls: HlsStart
            do {
                do {
                    hls = try await createRetryingNotYet(
                        model: model,
                        file: requestedFile,
                        body: body,
                        lifecycle: lifecycle,
                        generation: generation,
                        recoveryLinkReceipt: recoveryCause?.receiptForRequest(previous: body.previousSessionId,
                            candidate: {
                                if case let .autoCandidate(_, id)? = body.intent?.selection.quality { return id }
                                return nil
                            }(), cause: body.reopenReason)
                    )
                } catch let createError {
                    guard !Task.isCancelled, isCurrentLifecycle(lifecycle), !isSuperseded(generation) else { return }
                    // The bound half of a stall reopen is the only part of this
                    // body the server can refuse on its own: the predecessor
                    // may have been superseded or retired between the stall and
                    // this create, including by a retryable failure that
                    // replays after the claim was already normalized. Recovery
                    // is still wanted, so re-post the identical recipe unbound
                    // — under a fresh request id, because the old one is spent
                    // on the refused claim. Exactly one retry, only for a bound
                    // attempt, and only for the status that means "that
                    // predecessor is not a thing I can step down from".
                    guard let unbound = Self.unboundStallRetry(
                        for: body,
                        after: createError
                    ) else { throw createError }
                    // The unbound re-post is a create like any other, so it
                    // gets the same "still building" ladder: a predecessor that
                    // went away and a node that is still starting are two
                    // different answers and the second one is worth waiting for.
                    hls = try await createRetryingNotYet(
                        model: model,
                        file: requestedFile,
                        body: unbound,
                        lifecycle: lifecycle,
                        generation: generation
                    )
                }
            } catch {
                // A superseded attempt must not report its own failure over
                // the newer open's state (P2-6), and must not put back a stream
                // the newer open has already taken ownership of.
                if isSuperseded(generation) { return }
                // Nothing came into existence to replace the current stream, so
                // nothing about it changes: same session, same player item,
                // telemetry running again, and playing if it was. The caller
                // surfaces a transient error instead of a coming stall.
                restoreAfterFailedChange(session: superseded)
                throw error
            }
            guard !isSuperseded(generation) else {
                await release(session: hls.sessionId)
                return
            }
            guard started else {
                await release(session: hls.sessionId)
                isChangingStream = false
                return
            }
            // Spend the floor budget on what the server actually answered, in
            // lockstep with the `sessionHeight` it is measured against. An
            // unbound fallback counts too: it did not buy a lower rung either.
            if case .stallReopen = intent {
                stallReopenBudget.resolved(
                    height: hls.height,
                    previousHeight: previousHeight,
                    at: startMs
                )
            } else if case .sameDeliveryRepair = intent {
                stallReopenBudget.resolved(
                    height: hls.height,
                    previousHeight: previousHeight,
                    at: startMs
                )
            } else if case .resumeRepair = intent {
                stallReopenBudget.resolved(
                    height: hls.height,
                    previousHeight: previousHeight,
                    at: startMs
                )
            }
            sessionId = hls.sessionId
            beginPlaybackControl(hls, origin: model.origin)
            activeMediaPath = clusterRelativeMediaPath(hls.playlistUrl)
            activeMediaAuthenticated = false
            activeNativeSubtitle = nativeSubtitle
            // The authoritative normalized answer for this session, and the
            // rung the next stall reopen is measured against.
            sessionHeight = hls.height
            isDirectPlayback = false
            activeBurnedSubtitle = burnSubtitle
            encoder = hls.encoder
            // The session that exists overrides the plan that was decided: a
            // burn or an explicitly picked rung forces a transcode the decision
            // never promised. A server that cannot answer leaves the decision's
            // value standing rather than blanking the badge. Set only after a
            // successful create, so a failed change leaves the still-playing
            // stream's answer alone.
            adoptSessionDelivery(hls, decision: decision)
            isVOD = hls.vod ?? false
            usesDirectTimeline = isVOD
            if knownDurationMs <= 0 { knownDurationMs = hls.durationMs ?? 0 }
            nextBaseMs = Self.sessionMediaOriginMs(hls, requestedStartMs: startMs)
            if isVOD {
                if startMs > 0 { seekAfterAttach = startMs }
            } else if let itemPosition = Self.sessionAttachSeekMs(
                requestedStartMs: startMs,
                mediaOriginMs: nextBaseMs
            ) {
                // A copy session begins on the preceding keyframe. Timeline
                // mapping alone makes progress truthful but still replays that
                // lead-in after a stall recovery; seek within the newly
                // published item so the first rendered frame lands back at the
                // requested film position.
                seekAfterAttach = itemPosition
            }
            url = Session.shared.url(hls.playlistUrl)
            startRecoveryEvidencePoll()
        }

        guard var url else {
            restoreAfterFailedChange(session: superseded)
            throw APIError.badURL
        }
        // Nothing backs a direct play, and `superseded` still names the session
        // being replaced — so it is safe to forget here and released below.
        if direct { sessionId = nil }
        // The successor exists — only now does the predecessor go. Server-side
        // supersession has already retired it (the create carried the same
        // `playback_id`); this DELETE just hands the encoder slot back at once
        // rather than at the idle reaper's convenience.
        await release(session: superseded)
        guard !isSuperseded(generation) else { return }
        // That DELETE is a network round trip, so the viewer can leave during
        // it. Attaching an item and calling `play()` after `stop()` has run
        // would resurrect a player nobody is watching — and with the background
        // audio mode on, keep it audible. `stop()` has already retired whatever
        // session this open created.
        guard started else {
            isChangingStream = false
            return
        }
        if sessionId != nil && !isVOD {
            while true {
                do {
                    try await prepareGrowingPlaylist(url)
                    break
                } catch {
                    guard !isSuperseded(generation), started, !Task.isCancelled else { return }
                    let httpStatus: Int?
                    if case let APIError.http(status) = error { httpStatus = status }
                    else { httpStatus = nil }
                    // Preparation owns the resource before AVPlayer does, so
                    // it must preserve the same bounded ingress recovery that
                    // native transport errors receive after attachment.
                    guard Self.isTransportPlaybackFailure(error: error as NSError,
                              eventDomain: nil, eventStatus: httpStatus),
                          let path = activeMediaPath,
                          let successor = Session.shared.nextMediaFailoverURL(path,
                              authenticated: activeMediaAuthenticated) else { throw error }
                    url = successor
                }
            }
            // The viewer may stop or replace this open during server
            // preparation. Its completed playlist cannot resurrect the item.
            guard !isSuperseded(generation), started, !Task.isCancelled else { return }
        }
        let item = makePlayerItem(url)
        Self.configureBuffering(item, growingHLS: sessionId != nil && !isVOD)
        installSeekVideoOutput(on: item)
        #if os(iOS)
        // The tvOS 26 SDK exposes this setter, but some shipping Apple TV
        // runtimes do not implement it and abort on the Objective-C selector.
        // iOS uses it for system presentation metadata; tvOS uses our custom
        // player chrome and does not need item-level external metadata.
        item.externalMetadata = [titleMetadata(title)]
        #endif
        retireItemObserver()
        pgsOverlayItemGeneration &+= 1
        pgsOverlayWindowTask?.cancel()
        pgsOverlayLoadingRange = nil
        pgsOverlayWindowFailures = 0
        pgsOverlayWindow = nil
        stallObservation.reset()
        player.replaceCurrentItem(with: item)
        // The authoritative direct/session delivery now exists. A seek issued
        // during the initial decision must not inherit the default label.
        seekMeasurement.bindDelivery(generation: seekState.generation, method: clientLogMethod)
        installItemObserver(for: item)
        // The attached media generation changed: every fault about the
        // generation this replaces stops being about anything and is dropped.
        present(.attach(generation))
        if intent == .sameDeliveryRepair || intent == .resumeRepair {
            bindResumeRepairSuccessor(item, generation: generation)
        }
        recipeRevision.didAttach(requestedRecipeRevision)
        if requestedRecipeRevision == recipeRevision.desired { manualQualityRetention.didAttach() }
        playbackAttemptId = attemptId
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        latestAutoCompletedTransfer = nil
        autoMildSamples = 0
        // Publish the new local-to-film mapping only once the new item is the
        // one whose clock `realPositionMs()` reads. Updating it during session
        // creation mixed the predecessor's local time into the successor's
        // base and was the source of the apparently random seek jumps.
        baseMs = nextBaseMs
        progressiveCaptions.mediaChanged()
        if seekAfterAttach == nil {
            ttffMeasurement.rebasePosition(at: realPositionMs())
        }
        refreshPGSOverlayWindow(at: startMs, reason: .force)
        // Start loading/playing immediately. Previously a resume point gated
        // this call behind item readiness and could leave tvOS permanently
        // presenting a stopped transport. That regression is why a paused
        // reopen still calls `play()` here rather than betting that an item
        // reaches `.readyToPlay` at rate 0 on every shipping tvOS: prepare
        // first and honor the latest pause immediately. A pending readiness
        // or seek cannot grant permission to resume audible playback.
        player.play()
        if !wantsPlayback { player.pause() }
        isPlaying = wantsPlayback
        if let seekAfterAttach {
            do {
                try await seekWhenReady(item, ms: seekAfterAttach, owner: seekOwner)
                ttffMeasurement.rebasePosition(at: realPositionMs())
            } catch {
                if isSuperseded(generation) { return }
                // A failed replacement surfaces here on the VOD/direct resume
                // path before the post-open re-check below can run, and its
                // status observer fired into the change-suppression window.
                // Route it through the same ladder instead of a bare failure,
                // or the compatibility fallbacks are silently lost.
                if item.status == .failed {
                    isChangingStream = false
                    await handleItemFailure(item)
                    return
                }
                throw error
            }
        } else if Self.shouldBoundFreshStartReadiness(
            isVOD: isVOD,
            startMs: startMs,
            seeksAfterAttach: seekAfterAttach != nil,
            resumesPlayback: wantsPlayback
        ) {
            // A fresh start has no seek to wait behind, so nothing used to
            // bound its wait for a first frame at all. An item AVFoundation
            // neither readies nor fails — Dolby Vision Profile 5 on a device
            // that cannot decode it — held this open forever, which is the
            // black screen viewers reported.
            do {
                try await itemPreparation.ready(item, readinessContext(for: item))
            } catch {
                if isSuperseded(generation) { return }
                if item.status == .failed {
                    isChangingStream = false
                    await handleItemFailure(item)
                    return
                }
                // Only the deadline expiring is a verdict about the media. A
                // cancelled task says nothing about the picture.
                if let preparation = error as? PlaybackPreparationError,
                   case .timedOut = preparation,
                   await retryAfterReadinessTimeout(at: startMs) { return }
                throw error
            }
        }
        guard !isSuperseded(generation) else { return }
        await reconcileNativeMediaSelections(to: item)
        guard !isSuperseded(generation) else { return }
        applyDisplayCriteria(for: item, generation: generation)
        if wantsPlayback {
            player.play()
            // Restore the rate the viewer was last actually playing at (P2-5).
            if preferredRate != 1 { player.rate = preferredRate }
        } else {
            player.pause()
        }
        isPlaying = wantsPlayback
        currentMs = startMs
        playbackRecoveryMonitor.reset()
        deliveryStarvation.reset()
        // The destination this open was asked for has landed.
        present(.intentSettled(viewerActionEpoch))
        isChangingStream = false
        updateNowPlaying()
        // The status observer holds its fire while `isChangingStream` is up,
        // so a replacement that failed during its own attach is caught here.
        if item.status == .failed {
            await handleItemFailure(item)
            return
        }
        // Native choices were reconciled above, but the attached recipe was
        // captured before session creation. A new burn/direct-session choice
        // may still require replacement; reconcile that with a fresh owner.
        if let route = Self.subtitleReconciliation(
            applied: requestedSubtitle,
            current: selectedSubtitle,
            tracks: subtitles,
            activeBurn: activeBurnedSubtitle,
            isDirectPlayback: isDirectPlayback
        ) {
            let actionEpoch = viewerActionEpoch
            await applySubtitleSelection(selectedSubtitle, route: route, expectedActionEpoch: actionEpoch)
        }
    }

    /// True when a newer `open()` has taken ownership of the player. The older
    /// attempt then returns without replacing the item, clearing
    /// `isChangingStream`, or reporting its own failure (P2-6).
    private func isSuperseded(_ generation: Int) -> Bool { generation != openGeneration }

    /// Stop polling the session being left behind. The session itself survives
    /// this call — it is retired by `release(session:)` once its replacement
    /// exists.
    private func stopStatusPolling() {
        statusTask?.cancel()
        statusTask = nil
        sessionStatus = nil
        diagnosticSessionStatus = nil
        diagnosticSessionStatusObservedAt = nil
        preparationSample = nil
    }

    /// Retire a superseded HLS session, best effort.
    private func release(session id: String?) async {
        guard let id, let model else { return }
        await releaseHlsSession(model, id)
    }

    /// Put back everything `open` disturbed before it discovered it could not
    /// produce a successor. The viewer keeps watching what they were watching.
    private func restoreAfterFailedChange(session: String?) {
        isChangingStream = false
        // The create it failed on is a round trip too: if the viewer left
        // during it there is nothing left to keep watching, and resuming here
        // would restart a player `stop()` has already paused and detached from.
        guard started else { return }
        if session != nil { startRecoveryEvidencePoll() }
        if wantsPlayback {
            player.play()
            // Restore the rate the viewer was last actually playing at (P2-5).
            if preferredRate != 1 { player.rate = preferredRate }
            isPlaying = true
        } else {
            player.pause()
            isPlaying = false
        }
    }

    private func configureProgressiveCaptions() {
        retireProgressiveCaptionSelection()
        guard started, let model, fileId > 0 else { return }
        let lifecycle = lifecycleGeneration
        subtitleTextTranslation.configure(api: model.requireAPI(), fileId: fileId)
        progressiveCaptions.configure(api: model.requireAPI(), fileId: fileId, sourceAudioIndex: selectedAudio,
            now: { [weak self] in
                guard let self else { return (0, 0, 1, true) }
                return (self.realPositionMs(), self.baseMs, Double(self.preferredRate), !self.wantsPlayback)
            }, current: { [weak self] in self?.isCurrentLifecycle(lifecycle) == true })
    }

    private func retireProgressiveCaptionSelection() {
        // An attached stream replacement owns the reopen queue until it drains.
        // Retire caption intent through viewerActionEpoch without cancelling
        // that physical owner and stranding its queued newer subtitle change.
        if !isChangingStream { progressiveCaptionTask?.cancel() }
        progressiveCaptionTask = nil
    }

    /// Explicit local caption output is chosen by the player UI before entry.
    func selectProgressiveCaptions(_ task: CaptionTask) {
        guard started, !player.isExternalPlaybackActive else {
            showPlaybackNotice("Return playback to this device to show progressive captions.")
            return
        }
        retireProgressiveCaptionSelection()
        let actionEpoch = beginViewerAction(), lifecycle = lifecycleGeneration
        let captionAttempt = snapshotAttempt()
        let route = Self.subtitleSelectionRoute(for: nil, tracks: subtitles,
            activeBurn: activeBurnedSubtitle, isDirectPlayback: isDirectPlayback, activeOverlay: pgsOverlayTrackIndex)
        if route == .reopen { recipeRevision.change() }
        selectedSubtitle = nil
        progressiveCaptionTask = Task {
            defer { if attemptStillCurrent(captionAttempt, fence: .progressiveCaptionCleanup) { progressiveCaptionTask = nil; objectWillChange.send() } }
            retainControlSequence(await playbackControl.reportIntent())
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .progressiveCaptionAfterControl) else { return }
            let applied = await applySubtitleSelection(nil, route: route, expectedActionEpoch: actionEpoch)
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .progressiveCaptionAfterSubtitleRemoval), !player.isExternalPlaybackActive else { return }
            guard applied, activeBurnedSubtitle == nil, !recipeRevision.needsReopen else {
                showPlaybackNotice("Captions were not selected because subtitle removal did not attach. Try again after the stream change finishes.")
                return
            }
            if progressiveCaptions.request(task) { player.allowsExternalPlayback = false }
        }
    }

    func selectProgressiveAsset(_ asset: CaptionAsset) {
        guard started, !player.isExternalPlaybackActive else {
            showPlaybackNotice("Return playback to this device to show progressive captions.")
            return
        }
        retireProgressiveCaptionSelection()
        let actionEpoch = beginViewerAction(), lifecycle = lifecycleGeneration
        let captionAttempt = snapshotAttempt()
        let route = Self.subtitleSelectionRoute(for: nil, tracks: subtitles,
            activeBurn: activeBurnedSubtitle, isDirectPlayback: isDirectPlayback, activeOverlay: pgsOverlayTrackIndex)
        if route == .reopen { recipeRevision.change() }
        selectedSubtitle = nil
        progressiveCaptionTask = Task {
            defer { if attemptStillCurrent(captionAttempt, fence: .progressiveAssetCleanup) { progressiveCaptionTask = nil; objectWillChange.send() } }
            retainControlSequence(await playbackControl.reportIntent())
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .progressiveAssetAfterControl) else { return }
            let applied = await applySubtitleSelection(nil, route: route, expectedActionEpoch: actionEpoch)
            guard !Task.isCancelled, isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .progressiveAssetAfterSubtitleRemoval), !player.isExternalPlaybackActive else { return }
            guard applied, activeBurnedSubtitle == nil, !recipeRevision.needsReopen else {
                showPlaybackNotice("Captions were not selected because subtitle removal did not attach. Try again after the stream change finishes.")
                return
            }
            player.allowsExternalPlayback = false
            progressiveCaptions.read(asset)
        }
    }

    nonisolated static func translatedTextCaptionIndex(
        _ result: TextCaptionResult, fileId: Int, tracks: [SubtitleTrack]
    ) -> Int? {
        guard result.fileId == String(fileId), result.language == "en",
              CaptionValidation.digest(result.downloadedSubtitleId) else { return nil }
        let matches = tracks.filter { $0.downloadedSubtitleId == result.downloadedSubtitleId && $0.isNativeHLS }
        return matches.count == 1 ? matches.first?.index : nil
    }

    func useTranslatedTextCaption(_ result: TextCaptionResult) {
        guard started, result.fileId == String(fileId), let model, let context = fileContext else { return }
        _ = beginViewerAction()
        let captionAttempt = snapshotAttempt(), lifecycle = lifecycleGeneration
        Task {
            do {
                let quality: PlaybackQuality = selectedHeight.map(Self.initialQuality(height:)) ?? (selectedQualityIsOriginal ? .original : .auto)
                let refreshed = try await requestPlaybackDecision(model, context,
                    PrePlaySelection(audioIndex: selectedAudio, subtitleIndex: selectedSubtitle ?? PrePlaySelection.subtitleOff), quality)
                guard !Task.isCancelled, isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .translatedCaptionDecision),
                      refreshed.decision.fileId == fileId else { return }
                guard let index = Self.translatedTextCaptionIndex(result, fileId: fileId, tracks: refreshed.decision.subtitles ?? []) else {
                    showPlaybackNotice("That translated caption is no longer available. Refresh its job before selecting it.")
                    return
                }
                // Bind the immutable result to its current index. Another
                // acquired caption may have shifted the original result index.
                decision?.subtitles = refreshed.decision.subtitles
                selectSubtitle(index)
            } catch { if isCurrentLifecycle(lifecycle), attemptStillCurrent(captionAttempt, fence: .translatedCaptionFailure) { showPlaybackNotice(error.localizedDescription) } }
        }
    }

    func useLocalCaptionOutput() { player.allowsExternalPlayback = false }

    // MARK: - PGS application overlay

    /// Custom application layers are not carried by Apple's system PiP or
    /// external playback surfaces. Refuse that output while PGS is selected;
    /// never replace the current HDR/Dolby Vision bytes with a hidden burn.
    nonisolated static func pictureInPictureUnavailableNotice(
        pgsOverlayIsActive: Bool
    ) -> String? {
        pgsOverlayIsActive ? pgsOverlayExternalPlaybackNotice : nil
    }

    func allowsPictureInPictureCommand() -> Bool {
        guard let notice = Self.pictureInPictureUnavailableNotice(
            pgsOverlayIsActive: pgsOverlayIsActive
        ) else {
            return true
        }
        showPlaybackNotice(notice)
        return false
    }

    private func updatePGSOverlaySelection(_ index: Int?, tracks: [SubtitleTrack]? = nil) {
        guard let index, Self.subtitleUsesOverlay(index, in: tracks ?? subtitles) else {
            clearPGSOverlaySelection()
            return
        }
        guard pgsOverlayTrackIndex != index else {
            // A reselection of the same track is a viewer asking again, so it
            // is allowed past a failure backoff the way a seek is.
            refreshPGSOverlayWindow(at: positionForPlaybackIntent(), reason: .seek)
            return
        }

        clearPGSOverlaySelection()
        pgsOverlayTrackIndex = index
        pgsOverlayStatus = .preparing
        // AirPlay/external display would carry only the video plane. Keep the
        // selection local until output-specific overlay behavior is proven.
        player.allowsExternalPlayback = false
        pgsOverlaySelectionGeneration &+= 1
        let selectionGeneration = pgsOverlaySelectionGeneration

        pgsOverlayPrepareTask = Task { [weak self] in
            guard let self, let fetcher = self.pgsOverlayFetcher() else { return }
            do {
                let clock = ContinuousClock()
                let deadline = clock.now.advanced(
                    by: .seconds(PGSOverlayPolicy.maximumPrepareSeconds)
                )
                while clock.now < deadline {
                    try Task.checkCancellation()
                    guard self.pgsOverlaySelectionGeneration == selectionGeneration,
                          self.pgsOverlayTrackIndex == index,
                          self.selectedSubtitle == index
                    else { return }
                    switch try await fetcher.manifest(self.fileId, index) {
                    case .ready(let rawManifest):
                        let manifest = try rawManifest.validated(
                            fileId: self.fileId,
                            trackIndex: index
                        )
                        guard self.pgsOverlaySelectionGeneration == selectionGeneration else {
                            return
                        }
                        self.pgsOverlayManifest = manifest
                        self.refreshPGSOverlayWindow(
                            at: self.positionForPlaybackIntent(),
                            reason: .force
                        )
                        return
                    case .preparing(let retryAfterMs):
                        try await Task.sleep(for: .milliseconds(retryAfterMs))
                    }
                }
                throw PGSOverlayError.preparationTimedOut
            } catch is CancellationError {
                return
            } catch {
                guard self.pgsOverlaySelectionGeneration == selectionGeneration else { return }
                self.pgsOverlayStatus = .failed(PGSOverlayPolicy.failureDescription(error))
                self.pgsOverlayWindow = nil
                self.raisePGSOverlayNotice(error)
            }
        }
    }

    private func clearPGSOverlaySelection() {
        pgsOverlaySelectionGeneration &+= 1
        pgsOverlayPrepareTask?.cancel()
        pgsOverlayPrepareTask = nil
        pgsOverlayWindowTask?.cancel()
        pgsOverlayWindowTask = nil
        pgsOverlayLoadingRange = nil
        pgsOverlayWindowFailures = 0
        pgsOverlayTrackIndex = nil
        pgsOverlayManifest = nil
        pgsOverlayWindow = nil
        pgsOverlayStatus = .off
        pgsOverlayImageCache.removeAll(keepingCapacity: false)
        pgsOverlayImageBytes.removeAll(keepingCapacity: false)
        pgsOverlayImageLRU.removeAll(keepingCapacity: false)
        player.allowsExternalPlayback = !progressiveCaptions.selected
    }

    /// Why a window refresh was asked for. Only `.force` skips the policy.
    enum PGSOverlayRefreshReason {
        /// The 1 s periodic observer: never cancels a covering load, and after
        /// a failure retries only on the bounded backoff.
        case tick
        /// A seek or a reselection: the same coverage rule, but a failure
        /// backoff is the viewer's to override.
        case seek
        /// A new item or a newly prepared manifest: always reload.
        case force
    }

    /// The periodic observer's overlay step, separated so XCTest drives the
    /// same call the observer makes.
    func pgsOverlayPeriodicTick(currentMs: Int) {
        if let overlayPosition = PGSOverlayPolicy.periodicRefreshPosition(
            currentMs: currentMs,
            overlayIsActive: pgsOverlayIsActive
        ) {
            refreshPGSOverlayWindow(at: overlayPosition, reason: .tick)
        }
    }

    /// `issueSeek`'s overlay step. A seek inside the published window, or
    /// inside the one already loading, keeps it: the published window's
    /// layers are timed to the item, so they are already right for the new
    /// position, and reloading would only blank them for the round trip.
    func pgsOverlaySeeked(to target: Int) {
        refreshPGSOverlayWindow(at: target, reason: .seek)
    }

    private func refreshPGSOverlayWindow(at sourceTimeMs: Int, reason: PGSOverlayRefreshReason) {
        guard let manifest = pgsOverlayManifest,
              let trackIndex = pgsOverlayTrackIndex,
              selectedSubtitle == trackIndex
        else { return }
        if reason == .seek { pgsOverlayWindowFailures = 0 }
        guard reason == .force || PGSOverlayPolicy.shouldRefresh(
            sourceTimeMs: sourceTimeMs,
            loadedRange: pgsOverlayWindow?.sourceRange,
            loadingRange: pgsOverlayLoadingRange,
            windowFailures: pgsOverlayWindowFailures,
            msSinceFailure: pgsOverlayClockMs() - pgsOverlayLastFailureMs
        ) else { return }
        if reason == .force { pgsOverlayWindowFailures = 0 }

        let sourceRange = PGSOverlayPolicy.windowRange(
            at: sourceTimeMs,
            durationMs: manifest.durationMs
        )
        let cues = Array(manifest.cues.lazy.filter {
            $0.endMs > sourceRange.lowerBound && $0.startMs < sourceRange.upperBound
        }.prefix(PGSOverlayPolicy.maximumScheduledCues))
        let selectionGeneration = pgsOverlaySelectionGeneration
        let itemGeneration = pgsOverlayItemGeneration
        let itemBaseMs = baseMs
        let generation = manifest.generation
        pgsOverlayWindowTask?.cancel()
        pgsOverlayLoadGeneration &+= 1
        let loadGeneration = pgsOverlayLoadGeneration
        pgsOverlayLoadingRange = sourceRange

        pgsOverlayWindowTask = Task { [weak self] in
            // Every exit of the current load gives its range back, whichever
            // guard it leaves by. A range left behind would tell every later
            // tick and seek that a load which no longer exists covers them.
            defer { self?.finishPGSOverlayLoad(loadGeneration) }
            guard let self, let fetcher = self.pgsOverlayFetcher() else { return }
            do {
                guard PGSOverlayPolicy.windowFitsDecodedBudget(cues) else {
                    throw PGSOverlayError.memoryLimit
                }
                var rendered: [PGSOverlayRenderableCue] = []
                rendered.reserveCapacity(cues.count)
                var decodedWindowBytes = 0
                var decodedWindowPaths: Set<String> = []
                for cue in cues {
                    try Task.checkCancellation()
                    var objects: [PGSOverlayRenderableObject] = []
                    objects.reserveCapacity(cue.objects.count)
                    for object in cue.objects {
                        try Task.checkCancellation()
                        let image: CGImage
                        if let cached = self.cachedPGSOverlayImage(object.image) {
                            image = cached
                        } else {
                            let data = try await fetcher.object(
                                self.fileId,
                                trackIndex,
                                generation,
                                object.image
                            )
                            try Task.checkCancellation()
                            guard let decoded = UIImage(data: data)?.cgImage,
                                  decoded.width == object.width,
                                  decoded.height == object.height
                            else { throw PGSOverlayError.invalidImage }
                            try self.storePGSOverlayImage(decoded, for: object.image)
                            image = decoded
                        }
                        if decodedWindowPaths.insert(object.image).inserted {
                            let (bytes, overflow) = image.bytesPerRow
                                .multipliedReportingOverflow(by: image.height)
                            guard !overflow,
                                  bytes > 0,
                                  bytes <= PGSOverlayPolicy.decodedImageBudgetBytes
                                    - decodedWindowBytes
                            else { throw PGSOverlayError.memoryLimit }
                            decodedWindowBytes += bytes
                        }
                        objects.append(PGSOverlayRenderableObject(object: object, image: image))
                    }
                    rendered.append(PGSOverlayRenderableCue(cue: cue, objects: objects))
                }

                guard self.pgsOverlaySelectionGeneration == selectionGeneration,
                      self.pgsOverlayItemGeneration == itemGeneration,
                      self.pgsOverlayTrackIndex == trackIndex,
                      self.selectedSubtitle == trackIndex,
                      // A superseded load that finished before it saw its
                      // cancellation must not publish over its successor.
                      self.pgsOverlayLoadGeneration == loadGeneration
                else { return }
                self.pgsOverlayRevision &+= 1
                self.pgsOverlayWindow = PGSOverlayWindow(
                    revision: self.pgsOverlayRevision,
                    generation: generation,
                    baseMs: itemBaseMs,
                    sourceRange: sourceRange,
                    cues: rendered
                )
                self.pgsOverlayWindowFailures = 0
                self.pgsOverlayStatus = .ready
            } catch is CancellationError {
                return
            } catch {
                guard self.pgsOverlaySelectionGeneration == selectionGeneration,
                      self.pgsOverlayLoadGeneration == loadGeneration
                else { return }
                self.pgsOverlayWindow = nil
                self.pgsOverlayWindowFailures += 1
                self.pgsOverlayLastFailureMs = self.pgsOverlayClockMs()
                self.pgsOverlayStatus = .failed(PGSOverlayPolicy.failureDescription(error))
                // Said once per failure episode. The backoff retries that
                // follow are silent; a success or a seek ends the episode.
                if self.pgsOverlayWindowFailures == 1 {
                    self.raisePGSOverlayNotice(error)
                }
            }
        }
    }

    private func finishPGSOverlayLoad(_ loadGeneration: Int) {
        if pgsOverlayLoadGeneration == loadGeneration {
            pgsOverlayLoadingRange = nil
        }
    }

    private func raisePGSOverlayNotice(_ error: Error) {
        pgsOverlayNoticeCount += 1
        showPlaybackNotice(PGSOverlayPolicy.failureNotice(error))
    }

    private func pgsOverlayFetcher() -> PGSOverlayFetcher? {
        if let pgsOverlayFetcherForTesting { return pgsOverlayFetcherForTesting }
        guard let model else { return nil }
        return PGSOverlayFetcher(model: model, fileContext: fileContext)
    }

    private func cachedPGSOverlayImage(_ key: String) -> CGImage? {
        guard let image = pgsOverlayImageCache[key] else { return nil }
        pgsOverlayImageLRU.removeAll(where: { $0 == key })
        pgsOverlayImageLRU.append(key)
        return image
    }

    private func storePGSOverlayImage(_ image: CGImage, for key: String) throws {
        let (bytes, overflow) = image.bytesPerRow.multipliedReportingOverflow(
            by: image.height
        )
        guard !overflow,
              bytes > 0,
              bytes <= PGSOverlayPolicy.decodedImageBudgetBytes
        else {
            throw PGSOverlayError.memoryLimit
        }
        while pgsOverlayImageBytes.values.reduce(0, +) + bytes
            > PGSOverlayPolicy.decodedImageBudgetBytes,
              let oldest = pgsOverlayImageLRU.first {
            pgsOverlayImageLRU.removeFirst()
            pgsOverlayImageCache.removeValue(forKey: oldest)
            pgsOverlayImageBytes.removeValue(forKey: oldest)
        }
        guard pgsOverlayImageBytes.values.reduce(0, +) + bytes
                <= PGSOverlayPolicy.decodedImageBudgetBytes
        else { throw PGSOverlayError.memoryLimit }
        pgsOverlayImageCache[key] = image
        pgsOverlayImageBytes[key] = bytes
        pgsOverlayImageLRU.removeAll(where: { $0 == key })
        pgsOverlayImageLRU.append(key)
    }

    /// One server request supplies both recovery evidence and panel state.
    /// Recovery keeps its two-second cadence regardless of panel visibility.
    private func startRecoveryEvidencePoll() {
        statusTask?.cancel()
        preparationSample = nil
        guard let polledSessionId = sessionId else { return }
        let pollAttempt = snapshotAttempt()
        statusTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self, let model = self.model else { return }
                let requestedAtMs = PlaybackControlSession.monotonicMs()
                let status = try? await self.requestHlsStatus(model, polledSessionId)
                let observedAtMs = PlaybackControlSession.monotonicMs()
                guard !Task.isCancelled,
                      self.started,
                      self.attemptStillCurrent(pollAttempt, fence: .recoveryEvidencePoll),
                      self.sessionId == polledSessionId
                else { return }
                if self.autoPreparing, self.autoVoluntary,
                   let staged = self.preparedReplacement.activeAction {
                    let stagedStatus = try? await model.hlsStatus(staged.sessionId)
                    guard !Task.isCancelled, self.started,
                          self.attemptStillCurrent(pollAttempt, fence: .recoveryEvidenceAfterStagedPoll),
                          self.sessionId == polledSessionId else { return }
                    if self.preparedReplacement.activeAction?.sessionId == staged.sessionId {
                        self.autoStagedStatus = stagedStatus
                        self.autoStagedStatusObservedMs = stagedStatus == nil ? nil : PlaybackControlSession.monotonicMs()
                    }
                }
                if status == nil {
                    // Row 18: a stats poll is telemetry. The picture is
                    // untouched and the log line is the only trace.
                    self.noteSurfaceLogOnly("session_status_unavailable")
                }
                // M3. One access-log read every two seconds, which is the
                // cadence this poll already has. Off the render path, and the
                // only sampler there is outside the commit itself.
                self.sampleThePreparedSwitch()
                if let status {
                    // A fired wedge recovery replaces the session — and
                    // `open()` cancels THIS poll task, so the recovery must
                    // not run inside it. `observeDeliveryStarvation` spawns
                    // an unstructured task (immune to this task's
                    // cancellation) and this loop ends; the successor
                    // session starts its own poll.
                    let recovered = self.observeDeliveryStarvation(status)
                    self.preparationSample = PlayerItemReadiness.Sample(
                        status: status, requestedAtMs: requestedAtMs, observedAtMs: observedAtMs
                    )
                    self.publishPanelTelemetry(status)
                    if recovered { return }
                } else {
                    self.preparationSample = nil
                    self.publishPanelTelemetry(nil)
                }
                try? await Task.sleep(nanoseconds: 2_000_000_000)
            }
        }
    }

    /// Presentation state from the same response used by recovery; no second
    /// request and no influence on the recovery clock.
    private func publishPanelTelemetry(_ status: PlaybackSessionStatus?) {
        sessionStatus = status
        if let status {
            diagnosticSessionStatus = status
            diagnosticSessionStatusObservedAt = Date()
        }
    }

    /// Server-truth wedge check, run on every 2-second status poll. AVPlayer
    /// can stop fetching published media while reporting any
    /// `timeControlStatus` it likes and while its film clock still ticks —
    /// states the position-clock monitor reads as healthy or keeps resetting
    /// on. The server's delivery clock cannot be fooled that way: when it says
    /// nothing was delivered for 16+ seconds while 10+ seconds of published
    /// media sit unfetched, and this controller still wants playback with no
    /// change or seek in flight, that is a stall whatever AVPlayer claims.
    /// Recovery goes through the same bounded same-delivery ladder as every
    /// other stall — one reopen, then the visible failure screen. Returns
    /// whether a recovery was fired, so the polling task that observed it
    /// can end itself: the recovery runs in an unstructured task because
    /// `reopen` → `open()` cancels the status-poll task, and a recovery
    /// awaited inline there would cancel itself mid-open.
    private func observeDeliveryStarvation(_ status: PlaybackSessionStatus) -> Bool {
        let eligible = started
            && wantsPlayback
            && !finished
            && !isPlaybackBlocked
            && !isChangingStream
            && seekState.allowsStallRecovery
            && player.currentItem != nil
        let position = realPositionMs()
        guard deliveryStarvation.observe(
            deliveredIdleMs: status.deliveredIdleMs,
            publishedEndMs: status.publishedEndMs,
            fetchedEndMs: status.fetchedEndMs,
            positionMs: position,
            runwaySeconds: bufferedRunwaySeconds(),
            eligible: eligible
        ) else { return false }
        currentMs = position
        let event = PlaybackStallEvent(
            kind: .delivery,
            action: .reopen,
            positionMs: position,
            durationMs: status.deliveredIdleMs ?? 0
        )
        Task { await self.retrySameDeliveryAfterStall(event) }
        return true
    }

    /// Sample the film clock independently of AVPlayer's periodic observer,
    /// which stops firing when that clock stops. One shared clock counts the
    /// stagnation whatever `timeControlStatus` reports — a starving session
    /// flaps between `.playing` and `.waitingToPlayAtSpecifiedRate` faster
    /// than any per-regime counter's threshold, which is how real freezes
    /// used to go undetected. The regime tally labels the fired event while
    /// both kinds reconnect the exact delivery. Actual item failures remain
    /// the sole owner of the codec/HDR compatibility ladder. This restores the
    /// in-player equivalent of closing and reopening a title.
    private func startPlaybackRecoveryMonitor() {
        recoveryTask?.cancel()
        recoveryTask = Task { [weak self] in
            while !Task.isCancelled {
                // The ordinary health cadence is two seconds, but a control
                // verdict owns the picture only until its absolute deadline.
                // Recompute before every sleep so the final wait ends at that
                // deadline instead of at the next cadence tick (up to two
                // seconds late). `ceil` in the helper prevents an early wake
                // from accidentally spending the client's recovery attempt.
                let now = ProcessInfo.processInfo.systemUptime
                let sleepMs = Self.recoveryPollDelayMs(
                    now: now,
                    deferredDeadline: self?.deferredStall?.deadline
                )
                if sleepMs > 0 {
                    try? await Task.sleep(for: .milliseconds(sleepMs))
                }
                guard !Task.isCancelled, let self else { return }
                let shouldMonitor = self.started
                    && self.wantsPlayback
                    && !self.systemPaused
                    && !self.finished
                    && !self.isPlaybackBlocked
                    && !self.isChangingStream
                    && self.seekState.allowsStallRecovery
                    && self.player.currentItem != nil
                let timeControlStatus = self.player.timeControlStatus
                let position = self.realPositionMs()
                if !shouldMonitor || abs(position - (self.deferredStall?.event.positionMs ?? position)) >= 250 {
                    self.deferredStall = nil
                } else if let deferred = self.deferredStall,
                          ProcessInfo.processInfo.systemUptime >= deferred.deadline {
                    self.deferredStall = nil
                    await self.retrySameDeliveryAfterStall(
                        PlaybackStallEvent(
                            kind: deferred.event.kind,
                            action: .reopen,
                            positionMs: position,
                            durationMs: Self.controlStallDeferralDeadlineMs
                        ),
                        consultControl: false
                    )
                    continue
                }
                if self.diagnosticProbesEnabled, self.player.currentItem != nil {
                    self.reportPlaybackProbe(at: position)
                }
                guard let stallEvent = self.playbackRecoveryMonitor.sample(
                    positionMs: position,
                    timeControlStatus: timeControlStatus,
                    shouldMonitor: shouldMonitor,
                    // A first-frame wait may be the server deliberately
                    // filling its publish gate, so an unestablished item gets
                    // a longer leash (~30 s) and no nudge before its bounded
                    // reopen — long enough not to duplicate a cold start,
                    // finite so a reopen that lands into continued starvation
                    // can no longer freeze forever with no error.
                    establishedPlayback: self.attachmentRecovery.establishedPlayback
                ) else { continue }
                self.autoUpgradeEvidence.bind(attachment: self.player.currentItem.map(ObjectIdentifier.init),
                    attempt: self.playbackAttemptId, nowMs: PlaybackControlSession.monotonicMs())
                self.autoUpgradeEvidence.stalled(at: PlaybackControlSession.monotonicMs())
                self.autoUpgradeSinceMs = nil
                switch stallEvent.action {
                case .none:
                    continue
                case .nudge:
                    self.applyStallRecoveryNudge()
                case .reopen:
                    self.currentMs = position
                    // A stationary film clock is presentation evidence, not a
                    // decoder error. Actual AVPlayer item failures own the HDR
                    // and codec compatibility ladder; every timer-only stall
                    // keeps the selected recipe.
                    await self.retrySameDeliveryAfterStall(stallEvent)
                }
            }
        }
    }

    /// The ordinary watchdog uses the same buffered command seam as explicit
    /// Resume, but owns no resume deadline or repair admission.
    func applyStallRecoveryNudge() {
        let buffered = PlaybackResumeAttempt.bufferedFastPathQualifies(
            ready: player.currentItem?.status == .readyToPlay,
            runwaySeconds: bufferedRunwaySeconds(),
            intendedRate: preferredRate,
            hasPendingDestination: seekState.pendingMs != nil,
            replacementInFlight: isChangingStream
        )
        Self.applyPlaybackCommand(
            to: player,
            preferredRate: preferredRate,
            immediately: buffered
        )
        isPlaying = true
    }

    /// One bounded same-recipe recovery shared by every timer-only stall. It
    /// changes no capability flag or selected format and deliberately omits
    /// the legacy bound stall ticket: that ticket asks the server to lower an
    /// Auto selection, which an unattributed presentation wait cannot justify.
    private func retrySameDeliveryAfterStall(
        _ event: PlaybackStallEvent,
        consultControl: Bool = true,
        resumeAttemptId: String? = nil
    ) async {
        if let resumeAttemptId {
            // An explicit resume repair is valid only while its root attempt
            // is still current. A cancelled task must not fall through into
            // ordinary recovery after Pause removed the attempt.
            guard let attempt = resumeAttempt,
                  attempt.id == resumeAttemptId,
                  attempt.publicationCompleted,
                  attempt.repairAdmitted else { return }
        } else if resumeAttempt != nil {
            // Every general detector funnels through the resume owner. That
            // owner publishes first, admits once, and invokes this function
            // again with `consultControl: false`; a hold can therefore never
            // consume the sole admitted repair.
            admitResumeRepair(at: resumeNow(), reason: "shared-stall-detector")
            return
        }
        let shouldConsultControl = consultControl && resumeAttemptId == nil
        if shouldConsultControl, deferredStall != nil { return }
        // The ask goes here, before the first statement, and the placement is
        // the one detail worth getting right. `next(for:)` below sets
        // `attempted` and returns `.stop` on every later call — it IS the
        // spend — so an ask placed after it would let a server `hold`
        // permanently retire the one same-delivery reopen this client had,
        // which is the exact failure a hold exists to avoid.
        let stallAttempt = snapshotAttempt()
        let deferralDeadline = shouldConsultControl
            ? ProcessInfo.processInfo.systemUptime
                + Double(max(0, Self.controlStallDeferralDeadlineMs - event.durationMs)) / 1_000
            : nil
        let verdict: ControlAction?
        if shouldConsultControl, Self.controlMayDeferStall(durationMs: event.durationMs) {
            verdict = await controlVerdictForStall(event)
        } else {
            verdict = nil
        }
        // Everything the caller checked may have changed across that await.
        guard attemptStillCurrent(stallAttempt, fence: .stallRecovery),
              started,
              stallRecoveryStillEligible
        else { return }
        // The incumbent stalled: whatever recovery follows (a hold, a
        // downgrade, a reopen), the original-first re-plan a viewer boundary
        // armed is evidence-free now and is not carried into it.
        autoBoundaryReplan.clear()
        if let verdict,
           applyStallVerdict(verdict, event: event, deferralDeadline: deferralDeadline) {
            return
        }
        await selectAutoStallRecoveryCandidate(attempt: stallAttempt, deadline: deferralDeadline)
        guard attemptStillCurrent(stallAttempt, fence: .stallCandidateReturn), started, stallRecoveryStillEligible else { return }
        var decision = sameDeliveryStallRecovery.next(for: event.kind)
        #if os(iOS)
        let hasOfflineAsset = offlineAssetURL != nil
        #else
        let hasOfflineAsset = false
        #endif
        let transport = Self.recoveryTransport(hasOfflineAsset: hasOfflineAsset)
        stallReopenBudget.observedStall(at: event.positionMs)
        decision = Self.boundedStallRecoveryDecision(
            decision,
            transport: transport,
            budgetAllowsAnotherAttempt: stallReopenBudget.allowsAnotherAttempt,
            kind: event.kind,
            reopenStorm: &recoveryReopenBudget
        )
        reportPlaybackStall(event, outcome: decision.outcome)
        switch decision {
        case .reopen:
            // Resume has its own first-picture and settled measurements. It
            // must not overwrite the title's cold-start TTFF clock.
            if resumeAttempt == nil {
                ttffReason = "stall-\(event.kind.rawValue)"
                ttffMeasurement.opened(at: event.positionMs)
            }
            #if os(iOS)
            if transport == .offlineAsset {
                await reloadOffline(at: event.positionMs)
                return
            }
            #endif
            await reopen(
                at: event.positionMs,
                intent: .sameDeliveryRepair
            )
        case .stop(let terminal):
            invalidateResumeAttempt(outcome: "repair-exhausted")
            stopForBlockingSurface(revokingPlaybackIntent: true)
            isChangingStream = false
            // The ladder and the budgets are spent and the owner has stopped
            // the player: that is `exhausted`, and it is raised only after the
            // stop, which is the one obligation the contract adds (§3.4).
            //
            // Ruling D1: a terminal verdict arms the words this failure will
            // carry rather than causing one. The client's own message is a
            // guess at why production stopped; the server's is the answer.
            raiseOwnerFault(
                source: "owner_exhausted",
                title: Self.playbackStoppedFailureTitle,
                detail: playbackControl.terminalVerdict?.message ?? terminal.message
            )
        }
    }

    /// How long a recovery owner waits for the server's verdict before
    /// deciding for itself.
    ///
    /// Deliberately far shorter than the exchange deadline. The fallback is
    /// the branch the entire fleet takes — no installed build answers with
    /// anything but `none` — so this number is added to every real stall on
    /// every device, on top of the sixteen-second idle threshold the delivery
    /// detector already waits out. A server that cannot answer inside a second
    /// and a half is a server whose answer is not worth more frozen picture
    /// than the recovery it would have replaced.
    ///
    /// Extended once, because the reporter cannot start this ask's request
    /// while another is in flight.
    static let controlAskSeconds: TimeInterval = 1.5
    static let controlAskCapSeconds: TimeInterval = 3
    nonisolated static let controlStallDeferralDeadlineMs = 20_000
    static let seekPresentationDeadlineSeconds = PlayerSeekState.presentationDeadlineSeconds
    nonisolated private static let recoveryPollCadenceMs: Int64 = 2_000

    /// Wake at the earlier of the ordinary health cadence and a control
    /// verdict's absolute deadline.
    ///
    /// The deadline is monotonic uptime, not another duration from the most
    /// recent sample. Rounding up to a whole millisecond is important: waking
    /// early would fail the deadline check and then wait another full cadence.
    nonisolated static func recoveryPollDelayMs(
        now: TimeInterval,
        deferredDeadline: TimeInterval?
    ) -> Int64 {
        guard let deferredDeadline else { return recoveryPollCadenceMs }
        let remainingMs = ceil(max(0, deferredDeadline - now) * 1_000)
        return min(recoveryPollCadenceMs, Int64(remainingMs))
    }

    nonisolated static func controlMayDeferStall(durationMs: Int) -> Bool {
        durationMs < controlStallDeferralDeadlineMs
    }

    /// Publish this owner's evidence and wait, briefly, for the verdict.
    ///
    /// A remembered verdict is used only when this ask produced none of its
    /// own: a fresh answer is always better evidence than an armed one.
    private func controlVerdictForStall(_ event: PlaybackStallEvent) async -> ControlAction? {
        // The evidence is published from inside the ask, after it has read the
        // sequence floor. Publishing first lets the pump start the next
        // request before that read lands, which makes the floor one too high
        // and rejects the very exchange that carried this stall's evidence.
        //
        // A remembered verdict is deliberately NOT consulted here. It arms the
        // words a failure carries; letting it decide would make the first
        // stall of every later session instantly fatal with no retry, because
        // a terminal stops the reporter and no fresher answer can ever
        // override it. Ruling D1 says armed, not executed.
        let evidence = Self.stallEvidence(for: event.kind)
        return await playbackControl.askForAction(
            bound: Self.controlAskSeconds,
            cap: Self.controlAskCapSeconds,
            publish: { [weak self] in
                self?.reportControlEvidence(evidence, render: .stalled)
            }
        )
    }

    /// Publish what this failure looks like, and wait briefly for the verdict.
    ///
    /// The evidence is the item's own error class rather than a stall's, and
    /// it is published from inside the ask so the sequence floor is read
    /// first — see `askForAction`.
    private func controlVerdictForItemFailure(_ item: AVPlayerItem) async -> ControlAction? {
        let evidence = ClientObservation(
            decoderState: .failed,
            errorCode: .media,
            errorDetail: "avplayer_item_failed"
        )
        return await playbackControl.askForAction(
            bound: Self.controlAskSeconds,
            cap: Self.controlAskCapSeconds,
            publish: { [weak self] in
                self?.reportControlEvidence(evidence, render: .failed)
            }
        )
    }

    /// Nothing to recover for: the player moved on while the ask was out.
    /// Is a stall recovery still the right thing to do?
    ///
    /// Both call sites gated on all of this before entering, and none of it
    /// survives an await. Seconds pass in the ask, and a viewer who paused,
    /// seeked or hit an unrelated failure in that window must not have a
    /// session reopened under them — a native seek in particular changes no
    /// generation and sets no flag, so `openGeneration` alone does not see it.
    private var stallRecoveryStillEligible: Bool {
        wantsPlayback
            && !finished
            && !isPlaybackBlocked
            && !isChangingStream
            && seekState.allowsStallRecovery
            && player.currentItem != nil
    }

    /// Act on a server verdict, or say it did not decide this stall.
    ///
    /// Returns `true` when the verdict is the decision, so the caller returns
    /// without spending its own attempt. `false` means today's path, unchanged
    /// — which is the branch every node in the fleet takes, because no client
    /// has ever completed a full-vocabulary exchange with one.
    func applyStallVerdict(
        _ verdict: ControlAction,
        event: PlaybackStallEvent,
        deferralDeadline: TimeInterval? = nil
    ) -> Bool {
        switch verdict.type {
        case "terminal":
            reportAutoRecoveryCause(.authority)
            // Ruling D1: the verdict is armed, not executed. This player is
            // stalled with nothing left to render — that is what reaching this
            // function means — so the only thing the verdict changes is whose
            // words the viewer reads.
            let terminal = event.kind.terminalState
            stopForBlockingSurface(revokingPlaybackIntent: true)
            isChangingStream = false
            raiseOwnerFault(
                source: "owner_stopped",
                title: Self.playbackStoppedFailureTitle,
                detail: verdict.message ?? terminal.message
            )
            reportPlaybackStall(event, outcome: .serverTerminal)
            return true
        case "hold":
            reportAutoRecoveryCause(.hold)
            // A hold answers why the producer paused, and production state is
            // never authority over serving: bytes already published stay
            // fetchable, so a wedged client may reconnect and fetch them. In
            // explicit lease mode the stall itself manufactures the hold, so a
            // hold allowed to decide there has no exit but the viewer backing
            // out. Spend nothing and say nothing — the reopen the caller runs
            // next is the answer.
            guard Self.holdMayDecideStall(
                kind: event.kind,
                publishedEndMs: sessionStatus?.publishedEndMs,
                fetchedEndMs: sessionStatus?.fetchedEndMs,
                runwaySeconds: bufferedRunwaySeconds(),
                durationMs: event.durationMs
            ) else { return false }
            guard deferStall(event, deadline: deferralDeadline) else { return false }
            // Production is deliberately not advancing, so a reopen would
            // churn against a server that already knows better — and it must
            // not spend the one same-delivery attempt either.
            //
            // The explanation is not optional. The recovery monitor re-enters
            // here on its own cadence, so a client that only returned would
            // leave a viewer in front of a frozen picture with no bound and
            // nothing said. The notice is transient and the monitor re-shows
            // it, which is the right shape: it disappears when the hold does.
            showPlaybackHold(Self.holdNotice(verdict.reason))
            restartDeliveryPollAfterDeferral(event)
            reportPlaybackStall(event, outcome: .serverHold)
            return true
        case "retry_resource":
            reportAutoRecoveryCause(.hold)
            // Production stopped for something that may not recur, and named
            // when to look again. The monitor is already a loop, so the honest
            // response is to spend nothing and let it come round — the pacing
            // this client can honour is "not now", not a precise interval.
            guard Self.controlMayDeferStall(durationMs: event.durationMs) else { return false }
            guard deferStall(event, deadline: deferralDeadline) else { return false }
            showPlaybackHold(Self.holdNotice(verdict.reason))
            restartDeliveryPollAfterDeferral(event)
            reportPlaybackStall(event, outcome: .serverRetryResource)
            return true
        default:
            return false
        }
    }

    /// Whether a server `hold` may decide this stall.
    ///
    /// It may not when the stall is a fetch wedge. A `.delivery` stall is one
    /// by construction, and a `.buffering` stall whose last status poll shows
    /// published media this player has not fetched while its runway is gone is
    /// the same freeze read from the decoder's side. Those bytes are already
    /// published, so the question a hold answers — why the producer paused —
    /// has no bearing on whether this client may reconnect and fetch them.
    ///
    /// A loaded buffering wait and a `.silent` freeze both have native media
    /// in hand. The recovery monitor already spent its one harmless play
    /// reevaluation before it asks for a verdict; a producer hold cannot then
    /// postpone the bounded decoder/attachment recovery.
    nonisolated static func holdMayDecideStall(
        kind: PlaybackStallKind,
        publishedEndMs: Int?,
        fetchedEndMs: Int?,
        runwaySeconds: Double?,
        durationMs: Int = 0
    ) -> Bool {
        guard controlMayDeferStall(durationMs: durationMs) else { return false }
        switch kind {
        case .delivery:
            return false
        case .silent:
            return false
        case .buffering:
            if let runwaySeconds,
               runwaySeconds > DeliveryStarvationDetector.runwayCeilingSeconds {
                return false
            }
            // Numbers the poll never carried are not evidence of a wedge, and
            // an unevidenced reopen is worse than honouring the server.
            guard let publishedEndMs, let fetchedEndMs else { return true }
            // An unknown runway is not evidence of health, the same reading
            // `DeliveryStarvationDetector` takes of the same measurement.
            let runwayGone = (runwaySeconds ?? 0)
                <= DeliveryStarvationDetector.runwayCeilingSeconds
            let unfetchedMs = publishedEndMs - fetchedEndMs
            return !(runwayGone
                && unfetchedMs >= DeliveryStarvationDetector.pendingMediaThresholdMs)
        }
    }

    private func deferStall(
        _ event: PlaybackStallEvent,
        deadline: TimeInterval?
    ) -> Bool {
        let now = ProcessInfo.processInfo.systemUptime
        let remainingMs = max(0, Self.controlStallDeferralDeadlineMs - event.durationMs)
        let absoluteDeadline = deadline ?? now + Double(remainingMs) / 1_000
        guard absoluteDeadline > now else { return false }
        deferredStall = (event, absoluteDeadline)
        return true
    }

    /// The server-side wedge signature, read from the last status poll: no
    /// completed delivery for sixteen seconds while ten or more seconds of
    /// published media sit unfetched.
    ///
    /// A `.delivery` stall short-circuits the reading. That detector cannot
    /// fire without this exact signature, so the one stall that is a wedge by
    /// construction must not be demoted to an ordinary one by a status poll
    /// that has not landed yet.
    nonisolated static func isDeliveryWedge(
        kind: PlaybackStallKind,
        deliveredIdleMs: Int?,
        publishedEndMs: Int?,
        fetchedEndMs: Int?
    ) -> Bool {
        guard kind != .delivery else { return true }
        guard let deliveredIdleMs,
              deliveredIdleMs >= DeliveryStarvationDetector.deliveredIdleThresholdMs,
              let publishedEndMs,
              let fetchedEndMs,
              publishedEndMs - fetchedEndMs
                >= DeliveryStarvationDetector.pendingMediaThresholdMs
        else { return false }
        return true
    }

    /// A deferred delivery stall leaves the poll that found it dead.
    ///
    /// `startRecoveryEvidencePoll`'s task ends itself the moment
    /// `observeDeliveryStarvation` fires, on the documented assumption that a
    /// reopen follows and `open()` will start a fresh one. A deferral breaks
    /// that assumption: nothing reopens, so the server-truth wedge detector —
    /// the one that exists because AVPlayer froze on tvOS 2160p without ever
    /// tripping the position-clock ladder — would stay dead for the rest of
    /// the session, long after the hold lifted.
    private func restartDeliveryPollAfterDeferral(_ event: PlaybackStallEvent) {
        // The task object outlives its own `return`, so there is nothing to
        // test for. `startRecoveryEvidencePoll` cancels whatever is there first, and
        // for any other stall kind the poll was never the thing that ended.
        guard event.kind == .delivery else { return }
        startRecoveryEvidencePoll()
    }

    /// The server's seven hold reasons, in the viewer's words. An unknown
    /// reason shows the generic line rather than its wire name: a viewer
    /// reading `working_set` learns less than one reading a sentence.
    nonisolated static func holdNotice(_ reason: String?) -> String {
        switch reason {
        case "demand": return "Another player is using this stream."
        case "time", "bytes": return "The server is pacing this stream."
        case "global": return "The server is busy."
        case "ahead": return "The stream is already far enough ahead."
        case "working_set", "no_room": return "The server’s playback buffer limit has been reached."
        default: return "Waiting for the server."
        }
    }

    /// Timer-only stalls establish missing presentation, not their cause.
    /// AVPlayer item failures publish their actual error separately; a loaded
    /// wait, silent clock, or delivery wedge therefore stays explicitly
    /// unknown and cannot authorize decoder or quality fallback.
    nonisolated static func stallEvidence(for _: PlaybackStallKind) -> ClientObservation {
        ClientObservation(decoderState: .unknown)
    }

    nonisolated static func recoveryTransport(
        hasOfflineAsset: Bool
    ) -> SameDeliveryRecoveryTransport {
        hasOfflineAsset ? .offlineAsset : .serverSession
    }

    /// The exhausted-floor arm of the recovery state machine. A server session
    /// that keeps being answered with the rung it is already on has nowhere
    /// left to go, so the ordinary stall failure is shown rather than another
    /// reopen. Only server sessions are bounded: an offline package has no rung
    /// to step down to and no server to ask.
    ///
    /// Separate from `SameDeliveryStallRecoveryState`, which five seconds of
    /// recovered playback rearms — that interval is exactly the tight
    /// stall/reopen/stall loop this arm has to end.
    nonisolated static func stallRecoveryDecision(
        _ proposed: SameDeliveryStallRecoveryDecision,
        transport: SameDeliveryRecoveryTransport,
        budgetAllowsAnotherAttempt: Bool,
        kind: PlaybackStallKind
    ) -> SameDeliveryStallRecoveryDecision {
        guard proposed == .reopen,
              transport == .serverSession,
              !budgetAllowsAnotherAttempt
        else { return proposed }
        return .stop(kind.terminalState)
    }

    /// Compose the two independent stall bounds, in the one order that is safe.
    ///
    /// They answer different questions and neither subsumes the other. The
    /// ladder floor asks whether reopens are still buying a strictly lower
    /// rung; it is the only bound that can answer "never again", because a
    /// link starved at the floor reproduces its own precondition forever. The
    /// rolling budget asks how many automatic reopens landed in the last
    /// minute; it is a storm cap and releases on its own once the window
    /// slides.
    ///
    /// The floor is evaluated first because `admit()` *mutates* — it records a
    /// timestamp. Consulting it ahead of the floor would charge a rolling slot
    /// to a reopen the floor then converts to `.stop`, so a genuinely
    /// unrelated stall minutes later would inherit that debt and hit the storm
    /// cap early. A reopen that never happens must not be counted as one.
    nonisolated static func boundedStallRecoveryDecision(
        _ proposed: SameDeliveryStallRecoveryDecision,
        transport: SameDeliveryRecoveryTransport,
        budgetAllowsAnotherAttempt: Bool,
        kind: PlaybackStallKind,
        reopenStorm: inout RecoveryReopenBudget,
        now: TimeInterval = ProcessInfo.processInfo.systemUptime
    ) -> SameDeliveryStallRecoveryDecision {
        let decision = Self.stallRecoveryDecision(
            proposed,
            transport: transport,
            budgetAllowsAnotherAttempt: budgetAllowsAnotherAttempt,
            kind: kind
        )
        guard decision == .reopen else { return decision }
        guard reopenStorm.admit(at: now) else { return .stop(kind.terminalState) }
        return decision
    }


    /// Stamp an open's cause onto its create body.
    ///
    /// `quality_auto` goes on every body, not only a bound one: it is the
    /// viewer's answer to the quality menu, and the server's fallback
    /// inference ("no height means Auto") misreads the promise height a
    /// subtitle burn posts as a sticky manual pick, which would leave that
    /// session permanently unsteppable.
    ///
    /// A stall ticket is honored only while it still names the session that is
    /// actually playing. A seek, a quality change, or another recovery that
    /// completed first has already replaced that predecessor, and a create
    /// naming a session the server no longer runs is refused outright — so the
    /// binding is dropped and the recovery proceeds as an ordinary reopen.
    nonisolated static func applyOpenIntent(
        to body: CreateSessionRequest,
        intent: PlayerOpenIntent,
        currentSessionId: String?,
        selectedHeight: Int?,
        qualityIsOriginal: Bool = false
    ) -> CreateSessionRequest {
        var body = body
        body.qualityAuto = selectedHeight == nil && !qualityIsOriginal
        guard case .stallReopen(let ticket) = intent,
              let currentSessionId,
              ticket.previousSessionId == currentSessionId
        else { return body }
        body.previousSessionId = ticket.previousSessionId
        body.reopenReason = Self.stallReopenReason
        body.requestId = ticket.requestId
        return body
    }

    /// The wire value for a typed stall recovery. The server accepts no other.
    nonisolated static let stallReopenReason = "stall"

    /// The document says whether HDR10 output is allowed; this echo says the
    /// session create actually requests an HDR10 transcode. Manual quality
    /// selection can turn a direct/remux decision into a non-copy create
    /// without re-running `/decision`, while a compatibility rescue and a
    /// subtitle burn are explicitly SDR even when the decision they replace
    /// described HDR10 source bytes.
    nonisolated static func sessionHDR10Request(
        copy: Bool,
        deliveredRange: String?,
        forcesSDR: Bool
    ) -> Bool? {
        !copy
            && !forcesSDR
            && deliveredRange?.lowercased() == "hdr10"
            ? true
            : nil
    }

    /// The unbound body to re-post when the server refuses a bound stall
    /// reopen, or `nil` when this failure is not that case.
    ///
    /// Narrowed to `400` on a body that actually carried the binding. Every
    /// other field of a create this client sends has already produced a
    /// session on the open that is currently playing, so a `400` here is the
    /// predecessor going away underneath the recovery — the one failure a
    /// second, unbound attempt can fix. Anything else (401, 404, 5xx,
    /// transport) is left to the caller's existing restore-and-surface path.
    nonisolated static func unboundStallRetry(
        for body: CreateSessionRequest,
        after error: Error
    ) -> CreateSessionRequest? {
        // `.httpStatus` rather than `case .http(400)`: since the adapter
        // reads refusal bodies, a 400 the server explains arrives as
        // `.refused(status: 400, …)`, and this matcher has to keep firing for
        // exactly the answers it always fired for.
        guard body.previousSessionId != nil,
              body.reopenReason == Self.stallReopenReason,
              (error as? APIError)?.httpStatus == 400
        else { return nil }
        var retry = body
        retry.previousSessionId = nil
        retry.reopenReason = nil
        // The refused claim consumed the old identity: replaying it returns
        // the same refusal rather than a session.
        retry.requestId = UUID().uuidString
        return retry
    }

    /// The decision, create and readiness failure path.
    ///
    /// What used to decide the blocking surface here was "is an item
    /// attached", which is not the same question the contract asks (§2.2). The
    /// question is **is a picture presenting**, and the presenter already knows:
    /// `surface.presenting` is the last sample of the client's own presentation
    /// proof. "An item is attached" answers yes for an item AVFoundation never
    /// readied, which is how a readiness timeout used to leave a red strip over
    /// a black screen with no way out of it.
    ///
    /// * **Nothing presenting** — there is no picture to disagree with, so the
    ///   owner stops the player and raises a blocking fault: `exhausted` when
    ///   this is a readiness verdict with the ladder spent, `stopped`
    ///   otherwise. This is where `fail()` gained its `player.pause()`.
    /// * **A picture presenting** — the viewer is watching something, so this
    ///   is about the destination that failed and not about what is on screen:
    ///   a 410 the buffer can still play out, or the refused change.
    ///
    /// R1 overrides both: a 401 or 403 beats context everywhere. The source
    /// table puts `auth_401_403` above `change_failed` precisely so a pending
    /// change cannot demote a credential refusal, and because it maps to a
    /// blocking class the owner has to stop first.
    ///
    /// The server's own sentence wins wherever it sent one: the adapter reads
    /// the refusal body and the source table classifies on its code.
    private func fail(_ error: Error) {
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        playbackEnhancement = nil
        isChangingStream = false
        ttffMeasurement.reset()
        // M5: the create-retry owner has already stopped the player and raised
        // `exhausted`, on the clock, at the moment its absolute deadline
        // passed. A second fault here would put a lower class over that prompt
        // and tell the viewer a different story about the same failure.
        if error is PlaybackCreateRetryError { return }
        let detail = (error as? LocalizedError)?.errorDescription ?? error.localizedDescription
        let title = currentMs > 0
            ? Self.playbackStoppedFailureTitle
            : Self.playbackStartFailureTitle
        let classified = Self.refusalSurfaceOutcome(for: error)?.source
        if classified == "auth_401_403" {
            // R1. Stop first: the row is `stopped`, and a blocking class raised
            // over a running player is refused by the presenter and draws
            // nothing at all — a frozen picture with no banner and no prompt.
            stopForBlockingSurface()
            raiseOwnerFault(source: "auth_401_403", error: error, title: title, detail: detail)
            return
        }
        guard surface.presenting else {
            let ladderSpent = error is PlaybackPreparationError
                && plannedCompatibilityFallback == .none
                && player.currentItem != nil
            // Stop, then raise. With the ladder spent nothing is coming that
            // could make this picture move again on its own; with no picture at
            // all the pause changes nothing a viewer can see and makes the stop
            // the fault declares literally true.
            stopForBlockingSurface()
            raiseOwnerFault(
                source: ladderSpent ? "owner_exhausted" : "owner_stopped",
                error: error,
                title: title,
                detail: detail
            )
            return
        }
        if classified == "media_owner_lost_410" {
            // D1's shape: the owner is gone but the buffered media in front of
            // the viewer is not, so this is `recovering` until the owner stops
            // — and the stop promotes this same fault, carrying the film
            // position the body gave, so Try again reopens where they were.
            raiseSurfaceNotice(
                source: "media_owner_lost_410",
                context: .attached,
                error: error,
                detail: detail
            )
            return
        }
        // A picture is presenting. This is the change that failed, not the
        // film the viewer is watching, and the predecessor keeps its own faults.
        raiseSurfaceNotice(
            source: "change_failed",
            context: .change,
            error: error,
            title: title,
            detail: detail,
            intent: viewerActionEpoch
        )
    }

    /// The recovery owner's one obligation, in one place: stop the player, and
    /// make the published transport say so, before raising a blocking fault.
    ///
    /// `revokingPlaybackIntent` is the difference between the two shapes of
    /// stop this controller has, and it is the caller's to declare. Most owner
    /// stops leave the viewer's intent alone — a viewer who was watching when a
    /// 401 arrived still wants to be watching, and Try again resumes them.
    /// The terminal ones revoke it, because nothing is coming that they could
    /// want: a film that ended early twice, a stall ladder spent, an
    /// established HDR delivery that will not descend to SDR.
    ///
    /// Those five used to write `player.pause()`, `isPlaying` and
    /// `wantsPlayback` out by hand, three lines at a time, immediately before
    /// raising. One helper is the point: §3.4's obligation has one
    /// implementation, and `wantsPlayback` — which is also the presenter's
    /// `playback_requested` — has one owner-side writer rather than five.
    private func stopForBlockingSurface(revokingPlaybackIntent: Bool = false) {
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        playbackEnhancement = nil
        abandonSeekMeasurement()
        player.pause()
        isPlaying = false
        // Deliberate, and told to the presenter: a `buffering` fault is about a
        // player that WANTS media, and a player whose owner has stopped for
        // good wants none. A wait still standing under the prompt this stop is
        // about to raise is about nothing either.
        if revokingPlaybackIntent { wantsPlayback = false }
        // The stop is presentation evidence too, and it has to be recorded
        // HERE rather than left to the next sampler tick. Every owner site
        // raises its blocking fault synchronously after this call, so a model
        // still holding a `presenting` sample sees a blocking raise over a
        // moving picture and resolves §3.2's disagreement against it — and
        // that demotion is one-way, so the terminal never comes back. A 401
        // during a quality change drew "Playback recovered" with no Sign in
        // and no Close, permanently, until this line existed.
        if let attached = surface.attached {
            present(.presenting(false, attached: attached))
        }
    }

    // MARK: - M5: the create "not yet" retry

    /// Re-post the SAME create, under the SAME request identity, while the
    /// server says it is still building this stream (contract §3.3 row 6).
    ///
    /// Three properties the review asked for, and each is a line below:
    ///
    /// * the deadline is ABSOLUTE and runs on a watchdog rather than between
    ///   attempts, so a server that holds every create for three minutes
    ///   cannot stretch the sequence;
    /// * a success that lands AFTER the deadline is released, never attached —
    ///   the viewer has already been told this attempt is over, and an encoder
    ///   nobody is watching is a hardware slot held for nobody;
    /// * a newer intent ends it: every rung re-checks the lifecycle, the open
    ///   generation and this sequence's own epoch before it retries.
    ///
    /// Row 6 is the START context. A create refused over a predecessor the
    /// viewer is still watching is a refused CHANGE (row 7), keeps its banner
    /// and is not retried, so the sequence does not run there at all.
    private func createRetryingNotYet(
        model: AppModel,
        file: Int,
        body: CreateSessionRequest,
        lifecycle: Int,
        generation: Int,
        recoveryLinkReceipt: String? = nil
    ) async throws -> HlsStart {
        let receipt = body.reopenReason == AutoRecoveryCause.link.rawValue ? recoveryLinkReceipt : currentLinkReceipt()
        guard surfaceContext == .start else {
            return try await PlaybackLinkRequestContext.$receipt.withValue(receipt) {
                return try await requestHlsSession(model, try contextForFile(file), body)
            }
        }
        // ONE identity for the whole sequence. The server persists a create's
        // answer under `request_id`, so replaying one recovers the session it
        // already made instead of spawning a second encoder — which is what
        // makes retrying a create safe at all.
        // `CreateSessionRequest.requestId` already defaults to a fresh UUID
        // (`Models.swift`), and `applyOpenIntent` only ever replaces it with a
        // ticket's. There is nothing to mint here; what matters is that the
        // SAME body — and so the same identity — is posted on every rung.
        let request = body
        let began = ContinuousClock.now
        createRetryEpoch &+= 1
        let epoch = createRetryEpoch
        let watchdog = Task { [weak self] in
            guard let self else { return }
            // Same shape as `initialDecisionDeadlineTask`: a cancelled wait
            // throws, and a cancelled watchdog expires nothing.
            do { try await self.waitCreateRetry(PlaybackCreateRetry.deadlineMs) } catch { return }
            self.expireCreateRetry(epoch: epoch, lifecycle: lifecycle, generation: generation)
        }
        // Cancelling the watchdog is not quite enough: a sleep that has already
        // elapsed cannot be cancelled out of its own continuation, and a
        // sequence that settled on the deadline's own millisecond must not be
        // answered with a full-screen prompt over the session it just attached.
        // Retiring the epoch is what makes `expireCreateRetry` a no-op there —
        // and it is retired ONLY while this sequence still owns the counter,
        // because a sequence that was superseded must not retire the epoch of
        // the one that replaced it and leave that one unwatched.
        defer {
            watchdog.cancel()
            if createRetryEpoch == epoch { createRetryEpoch &+= 1 }
        }
        var attempt = 0
        while true {
            // Re-asked before EVERY rung, not only after a failure: the sleep
            // between attempts is where a newer open, a closed player or the
            // deadline itself arrives, and a sequence that woke up stale must
            // not post another create.
            guard !Task.isCancelled,
                  isCurrentLifecycle(lifecycle),
                  !isSuperseded(generation),
                  createRetryEpoch == epoch
            else { throw CancellationError() }
            if createRetryExpiredEpoch == epoch {
                throw PlaybackCreateRetryError.exhausted(reason: "deadline")
            }
            do {
                // Not `started`: that is the controller's own "is this player
                // running" flag, and shadowing it inside a recovery sequence is
                // the kind of thing a reader has to stop and check.
                let opened = try await PlaybackLinkRequestContext.$receipt.withValue(receipt) {
                    let opened = try await requestHlsSession(model, try contextForFile(file), request)
                    return opened
                }
                guard createRetryExpiredEpoch != epoch else {
                    await release(session: opened.sessionId)
                    throw PlaybackCreateRetryError.exhausted(reason: "deadline")
                }
                return opened
            } catch let spent as PlaybackCreateRetryError {
                throw spent
            } catch {
                // A newer intent owns the surface now, and a cancelled task
                // owns nothing at all: neither retries, and neither raises.
                guard !Task.isCancelled,
                      isCurrentLifecycle(lifecycle),
                      !isSuperseded(generation),
                      createRetryEpoch == epoch
                else { throw error }
                if createRetryExpiredEpoch == epoch {
                    throw PlaybackCreateRetryError.exhausted(reason: "deadline")
                }
                let elapsedMs = Self.milliseconds(began.duration(to: ContinuousClock.now))
                switch PlaybackCreateRetry.step(
                    attempt: attempt,
                    elapsedMs: elapsedMs,
                    isNotYet: PlaybackCreateRetry.isNotYet(error)
                ) {
                case .fail:
                    throw error
                case .exhausted(let reason):
                    raiseCreateRetryExhausted()
                    throw PlaybackCreateRetryError.exhausted(reason: reason)
                case .retry(let delayMs):
                    // Still building. ONE `preparing` fault for the sequence,
                    // not one per attempt: the ledger ring is for distinct
                    // faults, and a viewer watching a spinner does not need
                    // four identical rows behind it.
                    if attempt == 0 {
                        raiseSurfaceNotice(
                            source: "create_503_not_yet",
                            context: .start,
                            title: Self.stillPreparingTitle,
                            detail: (error as? LocalizedError)?.errorDescription
                                ?? error.localizedDescription
                        )
                    }
                    try await waitCreateRetry(delayMs)
                    attempt += 1
                }
            }
        }
    }

    /// The absolute deadline fired. The owner has nothing left to try, so it
    /// stops the player and raises `exhausted` (§3.0, §3.4). The create in
    /// flight is deliberately NOT cancelled: it is still awaited, so that
    /// whatever it produces can be released rather than leaked.
    private func expireCreateRetry(epoch: Int, lifecycle: Int, generation: Int) {
        // `createRetryEpoch == epoch` is both "this sequence is still the
        // current one" and "this sequence has not settled": its `defer`
        // retires the counter on the way out.
        guard createRetryEpoch == epoch, createRetryExpiredEpoch != epoch else { return }
        guard isCurrentLifecycle(lifecycle), !isSuperseded(generation) else { return }
        createRetryExpiredEpoch = epoch
        raiseCreateRetryExhausted()
    }

    /// Deliberately NOT the class's `keep_waiting`: on this path there is no
    /// stream to keep waiting on and no detector to re-arm, so offering it
    /// would be a button that does nothing. Try again re-runs the whole open,
    /// which is the honest "one more bounded attempt" here.
    private func raiseCreateRetryExhausted() {
        stopForBlockingSurface()
        raiseOwnerFault(
            source: "owner_exhausted",
            context: .start,
            title: Self.playbackStartFailureTitle,
            detail: Self.createRetryExhaustedMessage,
            actions: [.retry, .close]
        )
    }

    // MARK: - The playback surface

    // The adapter and the one publication site. Everything below turns a raw
    // outcome into a typed fault and hands it to the presenter; the presenter
    // decides what is drawn. Nothing here pauses, resumes, reopens or reports
    // to the control plane on the presenter's behalf — the recovery owner
    // above already did that before it raised
    // (docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3.0).

    // playback-surface-publish:begin
    /// The only place `surface` is written.
    @discardableResult
    private func present(_ event: PlaybackSurfaceModel.Event) -> [PlaybackSurfaceLog] {
        let now = ContinuousClock.now
        let entries = surface.apply(event, now: now)
        recordSurfaceLog(entries, at: now)
        return entries
    }

    /// A new playback, or none at all. The presenter has no lifecycle of its
    /// own, so starting one is starting a fresh model.
    private func resetSurface() {
        surface = PlaybackSurfaceModel()
        surfaceHistory = PlaybackSurfaceHistory()
        surfaceEpoch = ContinuousClock.now
        lastSurfaceSampleMs = nil
        lastSurfaceSampleAtMs = nil
        surfaceHasPresented = false
        streamChangePreparingIntent = nil
        surfaceLogOnlyReasons.removeAll()
        // The last playback's wait reading is not this one's.
        waitDetail = nil
    }

    /// The ledger ring and the four client-log events of the contract's §3.6.
    ///
    /// One POST per entry, not one per step. `/client-log` takes a single
    /// `ClientLog` object and has its own per-user limiter, so batching a step
    /// is a server change and this milestone does not touch the server. The
    /// volume is bounded by the events themselves: a raise or a retirement,
    /// never an evidence sample — a `presenting` sample that retires nothing
    /// returns no entries and this returns immediately.
    private func recordSurfaceLog(_ entries: [PlaybackSurfaceLog], at now: ContinuousClock.Instant) {
        guard !entries.isEmpty else { return }
        let elapsed = surfaceEpoch <= now
            ? Self.milliseconds(surfaceEpoch.duration(to: now))
            : 0
        let snapshot = PlaybackSurfaceHistory.PlayerSnapshot(
            rate: Double(player.rate),
            positionMs: currentMs,
            presenting: surface.presenting,
            sessionId: sessionId,
            attempt: playbackAttemptId
        )
        recordDiagnosticPlayback("state")
        for entry in entries {
            surfaceHistory.record(entry, atMs: elapsed, player: snapshot)
            reportSurfaceEvent(entry, at: elapsed, snapshot: snapshot)
        }
    }
    // playback-surface-publish:end

    /// The media generation a fault is ABOUT: the one the presenter has
    /// attached, not the one an open in flight has already claimed.
    ///
    /// `openGeneration` moves at the top of every `open()`, before the create
    /// has even been sent, so a create that fails leaves it one ahead of the
    /// picture the viewer is still watching. A fault raised against that
    /// number would be about a generation that never existed: evidence for the
    /// attached picture would not match it, the agreement rule could not see
    /// it, and `media_owner_lost_410` could never be promoted by the owner's
    /// own stop. Before anything is attached there is nothing to be about, and
    /// the open's own generation is the honest answer.
    private var surfaceAttachedGeneration: Int { surface.attached ?? openGeneration }

    /// `start` before this playback has ever presented, `change` while a
    /// replacement is pending over a predecessor that is still attached,
    /// `attached` otherwise (contract §3.3).
    private var surfaceContext: PlaybackSurfaceModel.Context {
        guard surfaceHasPresented else { return .start }
        return isChangingStream && player.currentItem != nil ? .change : .attached
    }

    /// Raise a blocking fault the recovery owner has **already** stopped the
    /// player for. `playerStopped: true` is that declaration, and the
    /// presenter refuses to draw the surface without it.
    private func raiseOwnerFault(
        source: String,
        context: PlaybackSurfaceModel.Context? = nil,
        error: Error? = nil,
        title: String?,
        detail: String?,
        actions: [PlaybackFault.Action] = [],
        positionMs: Int? = nil
    ) {
        let resolved = context ?? surfaceContext
        let outcome = Self.surfaceOutcome(
            for: error,
            context: resolved,
            fallback: source,
            fallbackActions: actions
        )
        present(PlaybackSurfaceModel.raise(
            source: outcome.source,
            context: resolved,
            attached: surfaceAttachedGeneration,
            playerStopped: true,
            actions: outcome.actions,
            positionMs: outcome.positionMs ?? positionMs,
            title: title,
            detail: outcome.detail ?? detail
        ))
    }

    /// Raise a fault that leaves the player alone: a notice, a hold, a
    /// recovery step, or a refused change over a predecessor that keeps
    /// playing.
    ///
    /// Structurally incapable of a blocking class. `surfaceOutcome` is told it
    /// may not return one, so a code rewrite cannot turn a banner into a
    /// `stopped` the presenter then refuses for want of a stop — which is not a
    /// surface but the absence of one: a frozen picture with nothing drawn over
    /// it. A blocking `fallback` is a call-site bug rather than a runtime
    /// outcome, and trips in debug rather than shipping silence.
    private func raiseSurfaceNotice(
        source: String,
        context: PlaybackSurfaceModel.Context,
        error: Error? = nil,
        title: String? = nil,
        detail: String?,
        intent: Int? = nil
    ) {
        assert(
            !Self.surfaceSourceIsBlocking(source, context: context),
            "\(source) maps to a blocking class; raise it through raiseOwnerFault after a stop"
        )
        let outcome = Self.surfaceOutcome(
            for: error,
            context: context,
            fallback: source,
            allowsBlocking: false
        )
        present(PlaybackSurfaceModel.raise(
            source: outcome.source,
            context: context,
            attached: surfaceAttachedGeneration,
            intent: intent,
            playerStopped: false,
            actions: outcome.actions,
            positionMs: outcome.positionMs,
            title: title,
            detail: outcome.detail ?? detail
        ))
    }

    /// The adapter: a refusal the server explained becomes the source row its
    /// code names, and anything else stays whatever outcome the owner
    /// produced. A row the context does not declare is never raised — the
    /// presenter would answer `source_context_mismatch` and draw nothing at
    /// all, which is worse than the generic sentence.
    nonisolated static func surfaceOutcome(
        for error: Error?,
        context: PlaybackSurfaceModel.Context,
        fallback: String,
        fallbackActions: [PlaybackFault.Action] = [],
        allowsBlocking: Bool = true
    ) -> PlaybackSurfaceOutcome {
        let generic = PlaybackSurfaceOutcome(source: fallback, actions: fallbackActions)
        guard let error, let classified = refusalSurfaceOutcome(for: error) else { return generic }
        guard case .success = PlaybackSurfaceContract.row(source: classified.source, context: context)
        else { return generic }
        // A caller that has not stopped the player may not be handed a row the
        // presenter will only draw over one.
        guard allowsBlocking || !surfaceSourceIsBlocking(classified.source, context: context)
        else { return generic }
        return classified
    }

    /// Does this row map to a class the presenter draws only over a player its
    /// owner has already stopped?
    nonisolated static func surfaceSourceIsBlocking(
        _ source: String,
        context: PlaybackSurfaceModel.Context
    ) -> Bool {
        guard case .success(let row) = PlaybackSurfaceContract.row(source: source, context: context),
              let cls = row.cls
        else { return false }
        return PlaybackSurfaceContract.rule(for: cls).blocking == .always
    }

    /// The source row a create/decision refusal maps to, or `nil` when the
    /// server said nothing this table can classify.
    nonisolated static func refusalSurfaceOutcome(for error: Error) -> PlaybackSurfaceOutcome? {
        guard let api = error as? APIError else { return nil }
        if let status = api.httpStatus, status == 401 || status == 403 {
            return PlaybackSurfaceOutcome(source: "auth_401_403", actions: [.signIn, .close])
        }
        guard let code = api.refusalCode else { return nil }
        switch code {
        // Still deliberately NOT `create_503_not_yet`, and now for a sharper
        // reason than "M5 has not happened yet". M5's retry owner
        // (`createRetryingNotYet`) raises that row itself, with the server's
        // sentence, while it is retrying. This function feeds `raiseOwnerFault`
        // — which is only ever called AFTER the owner stopped the player — so
        // answering it with a `preparing` would put a spinner over a stopped
        // player and never take it down. A "not yet" that reaches here is one
        // the retry already gave up on, and the owner's own `stopped` is the
        // truthful answer for it.
        case "vod_disabled":
            return PlaybackSurfaceOutcome(source: "vod_disabled")
        case "vod_source_rescan_required":
            return PlaybackSurfaceOutcome(source: "vod_source_rescan_required")
        case "vod_source_unsupported":
            return PlaybackSurfaceOutcome(source: "vod_source_unsupported")
        case "vod_transcode_unavailable":
            return PlaybackSurfaceOutcome(source: "vod_transcode_unavailable")
        case "vod_subtitle_burn_unavailable":
            return PlaybackSurfaceOutcome(source: "vod_subtitle_burn_unavailable")
        case "media_owner_lost":
            guard case .refused(_, _, _, let positionMs) = api else { return nil }
            return PlaybackSurfaceOutcome(source: "media_owner_lost_410", positionMs: positionMs)
        default:
            return nil
        }
    }

    /// A playlist or segment refusal reaches this client as a status code and
    /// nothing else: AVFoundation never surfaces the body (§2.2). Classify on
    /// the number, and never invent a sentence for it.
    nonisolated static func mediaFailureSurfaceSource(
        statusCode: Int?,
        context: PlaybackSurfaceModel.Context
    ) -> String {
        guard let statusCode else { return "owner_stopped" }
        let candidate: String
        switch statusCode {
        case 401, 403: candidate = "auth_401_403"
        case 410: candidate = "media_owner_lost_410"
        default: return "owner_stopped"
        }
        guard case .success = PlaybackSurfaceContract.row(source: candidate, context: context)
        else { return "owner_stopped" }
        // A 410 that reaches here has already exhausted the ladder above, so
        // the owner's stop is what the viewer is looking at; the 410's own
        // `recovering` rung belongs to the play-out that preceded it.
        return candidate == "media_owner_lost_410" ? "owner_stopped" : candidate
    }

    /// The app going away freezes every timer and every evidence sample: a
    /// thirty-second hold a viewer backgrounded for a minute has not been on
    /// screen for thirty seconds.
    private func installSurfaceLifecycleObservation() {
        surfaceLifecycleObservation = [
            (UIApplication.didEnterBackgroundNotification, true),
            (UIApplication.willEnterForegroundNotification, false),
        ].map { name, hidden in
            NotificationCenter.default.publisher(for: name).sink { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.present(.hidden(hidden))
                    if hidden {
                        self.invalidateResumeAttempt(outcome: "backgrounded")
                    }
                }
            }
        }
    }

    /// The staged loading overlay, raised (§3.3 row 10). No title and no
    /// detail: the overlay it replaces was a bare spinner, and §6 says move
    /// the text as it is rather than write new text.
    private func raiseStreamChangePreparing() {
        guard started else { return }
        let intent = viewerActionEpoch
        streamChangePreparingIntent = intent
        raiseSurfaceNotice(
            source: "client_preparing",
            context: surfaceContext,
            detail: nil,
            intent: intent
        )
    }

    /// The open settled — attached, refused, superseded or abandoned; the flag
    /// goes down on all four. `intent_settled` retires only the classes whose
    /// row names it, which is `preparing` and nothing else, so a refusal this
    /// same failure raised keeps its banner and its Try again.
    private func settleStreamChangePreparing() {
        guard let intent = streamChangePreparingIntent else { return }
        streamChangePreparingIntent = nil
        present(.intentSettled(intent))
    }

    /// Contract §3.3 row 12: `waitingToPlayAtSpecifiedRate` after start is a
    /// `media_waiting` fault, which the presenter draws as `buffering` once
    /// the wait has lasted `buffering_min_ms`. The debounce is the reducer's
    /// and there is no second timer here — this says only that the player is
    /// waiting, and says it at most once per wait.
    ///
    /// Before the first frame there is no "after start" to be after: row 12 is
    /// declared for the `attached` context alone, and a wait during a cold
    /// open is the staged start it actually is (`client_preparing`, row 10).
    /// Both carry the sentence the legacy overlay composed, moved as-is.
    private func noteMediaWaiting() {
        let context = surfaceContext
        guard let source = Self.mediaWaitSurfaceSource(
            timeControlStatus: player.timeControlStatus,
            context: context,
            started: started,
            wantsPlayback: wantsPlayback,
            finished: finished,
            blocked: isPlaybackBlocked
        ) else { return }
        // The web's `{once:true}`. AVPlayer flaps in and out of this status at
        // every fMP4 boundary on healthy 4K, exactly as Safari fires `waiting`,
        // and a wait already raised is a fact being restated rather than a new
        // one: the ledger ring is for distinct faults.
        guard !hasLiveSurfaceFault(source: source) else { return }
        let waiting = PlaybackWaitPresentation.make(
            runwaySeconds: bufferedRunwaySeconds(),
            httpWaitCount: sessionStatus?.httpWaitCount,
            source: source
        )
        raiseSurfaceNotice(
            source: source,
            context: context,
            title: waiting.title,
            detail: waiting.detail
        )
    }

    /// Which row a wait belongs to, or `nil` for no raise at all.
    ///
    /// Pure, like `surfaceIsPresenting` and for the same reason: a headless
    /// `AVPlayer` over a playlist URL nothing serves never reaches
    /// `.waitingToPlayAtSpecifiedRate`, so the decision is otherwise provable
    /// only by reading the source — which is how the last round's detector
    /// came to be pinned by spelling and proved by nothing.
    ///
    /// `change` is somebody else's pixel: the change's own `client_preparing`
    /// is already up, and a second progress fault under it would say the same
    /// thing twice. `wantsPlayback` rather than the transport's own rate,
    /// because a buffering player reports rate zero while Play is still the
    /// viewer's intent — the same reason `togglePlayPause` keys off it.
    nonisolated static func mediaWaitSurfaceSource(
        timeControlStatus: AVPlayer.TimeControlStatus,
        context: PlaybackSurfaceModel.Context,
        started: Bool,
        wantsPlayback: Bool,
        finished: Bool,
        blocked: Bool
    ) -> String? {
        guard started, wantsPlayback, !finished, !blocked,
              timeControlStatus == .waitingToPlayAtSpecifiedRate
        else { return nil }
        switch context {
        case .attached: return "media_waiting"
        case .start: return "client_preparing"
        case .change: return nil
        }
    }

    private func hasLiveSurfaceFault(source: String) -> Bool {
        let generation = surfaceAttachedGeneration
        return surface.faults.contains {
            $0.source == source && $0.attached == generation
        }
    }

    /// The presenter's clock, and nothing else.
    ///
    /// Every timing the contract states — the 350 ms buffering debounce, the
    /// 30 s hold, the 5 s degraded notice — is measured by the reducer, and
    /// the reducer only measures when its caller applies an event
    /// (implementation §3.3: "timers are the caller's `tick` events").
    /// AVPlayer's periodic observer cannot be that clock: it stops firing the
    /// moment the film clock stops, which is exactly when a wait needs
    /// drawing. This is the twin of the web's `playbackProgressTick`, on the
    /// same 500 ms. It samples nothing, detects nothing, moves no threshold
    /// and raises nothing the player is not already doing.
    private func startSurfaceClock() {
        surfaceClockTask?.cancel()
        surfaceClockTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(Self.surfaceClockIntervalMs))
                guard !Task.isCancelled, let self, self.started else { return }
                self.surfaceClockTick(nowMs: PlaybackControlSession.monotonicMs())
            }
        }
    }

    /// One beat of the presenter's clock. AVPlayer's periodic observer is
    /// the evidence sampler while the film clock runs, and it stops firing
    /// the moment the clock stops — which is exactly when the model needs to
    /// hear that the picture is no longer moving. Its last sample was taken
    /// over a still-moving clock, so the model kept saying `presenting`, and
    /// every `media_waiting` raised below was retired in the same step by
    /// that stale evidence: twelve seconds of frozen picture, thirty
    /// raise/clear pairs a millisecond apart, and no spinner. Once the
    /// observer has been silent for one clock interval this clock takes the
    /// same sample through the same sampler: the position delta over a
    /// stationary clock is "not presenting", which is the evidence the
    /// contract asks for (§3.4) and no transport status. Evidence first,
    /// then the wait, so the raise is weighed against the stopped clock.
    func surfaceClockTick(nowMs: Int) {
        refreshWaitDetail()
        if Self.surfaceClockSamplesPresentation(lastSampleAtMs: lastSurfaceSampleAtMs, nowMs: nowMs),
           !isChangingStream, player.currentItem != nil {
            sampleSurfacePresentation(at: realPositionMs(), evidence: currentLocalVideoEvidence())
        }
        noteMediaWaiting()
        present(.tick)
    }

    nonisolated static let surfaceClockIntervalMs = 500

    /// Whether the presenter's clock owes the model a presentation sample:
    /// only once the periodic observer has been silent for a whole clock
    /// interval. While the film clock runs the observer samples on its own
    /// cadence and this clock adds nothing, so a healthy picture is never
    /// sampled twice in one turn and never reads as stationary by accident.
    nonisolated static func surfaceClockSamplesPresentation(
        lastSampleAtMs: Int?,
        nowMs: Int,
        intervalMs: Int = surfaceClockIntervalMs
    ) -> Bool {
        guard let lastSampleAtMs else { return true }
        return nowMs - lastSampleAtMs >= intervalMs
    }

    /// Resample the wait sentence. Published only on change, so a steady
    /// reading does not redraw the player twice a second.
    private func refreshWaitDetail() {
        let detail = PlaybackWaitPresentation.make(
            runwaySeconds: bufferedRunwaySeconds(),
            httpWaitCount: sessionStatus?.httpWaitCount
        ).detail
        if waitDetail != detail { waitDetail = detail }
    }

    /// Contract §3.3 row 18: prepared-successor abandonment, `session_gone` on
    /// a successor's first exchange, and telemetry/reporter/stats failures.
    /// The incumbent is untouched, `log_only` maps to no class at all, and the
    /// event is the only trace — which is the point, and which the fence keeps
    /// true.
    ///
    /// One row per distinct reason per attached generation: a control reporter
    /// that fails every cadence, or a status poll that answers nothing for a
    /// minute, is one fact and not thirty `/client-log` posts.
    ///
    /// `postClientLog` is deliberately not a site. This raise posts a client
    /// log, so a failed post that raised here would be a loop.
    @discardableResult
    func noteSurfaceLogOnly(_ reason: String) -> Bool {
        guard started else { return false }
        let generation = surfaceAttachedGeneration
        guard surfaceLogOnlyReasons.insert("\(reason)@\(generation)").inserted else { return false }
        present(PlaybackSurfaceModel.raise(
            source: "log_only",
            context: surfaceContext,
            attached: generation,
            detail: reason
        ))
        return true
    }

    /// PiP and AirPlay report presentation through the platform's own flags,
    /// so the view hands them over rather than this controller guessing.
    func notePictureInPictureActive(_ active: Bool) {
        pictureInPictureIsActive = active
        if active {
            invalidateResumeAttempt(outcome: "external-presentation")
        }
    }

    /// A PiP failure is a notice about the window, not about the picture.
    func notePictureInPictureFailure(_ message: String?) {
        guard let message, !message.isEmpty else { return }
        raiseSurfaceNotice(source: "degraded_notice", context: surfaceContext, detail: message)
    }

    /// One evidence sample from the client's existing presentation proof. No
    /// detector and no timer of its own: the periodic observer supplies the
    /// same current-layer snapshot used by TTFF and the black-frame watchdog.
    private func sampleSurfacePresentation(at observedPosition: Int, evidence: LocalVideoEvidence) {
        // Nothing attached is nothing to prove.
        guard let attached = surface.attached else { return }
        let hasVideoSource = decision?.source?.videoCodec != nil
        // Declared, not guessed: an AirPlay or PiP picture is presenting on a
        // screen this process cannot measure.
        let declared = player.isExternalPlaybackActive || pictureInPictureIsActive
        let picture = (decision?.source != nil && !hasVideoSource)
            || evidence.frameReady || declared
        let presenting = Self.surfaceIsPresenting(
            observedPosition: observedPosition,
            lastSampleMs: lastSurfaceSampleMs,
            isChangingStream: isChangingStream,
            rate: player.rate,
            picture: picture
        )
        lastSurfaceSampleMs = observedPosition
        lastSurfaceSampleAtMs = PlaybackControlSession.monotonicMs()
        if presenting { surfaceHasPresented = true }
        present(.presenting(presenting, attached: attached))
        endSystemHoldIfPlaying()
    }

    #if DEBUG
    /// Test seam, Debug-only: an attached playback with a surface model to
    /// sample into, so the presenter's clock can be driven on a controller
    /// without a server. It sets the three facts the clock reads — started,
    /// an attached generation and a current item — and decides nothing.
    func attachSurfaceForTesting(_ item: AVPlayerItem, generation: Int) {
        started = true
        player.replaceCurrentItem(with: item)
        _ = present(.attach(generation))
    }

    /// Test seam, Debug-only: the periodic observer's sample, taken through
    /// the real sampler at a position and rate the test names, so a test can
    /// stand where AVPlayer's observer last stood before the film clock
    /// stopped. `atMs` is when the observer ran on the controller's clock.
    func sampleObserverPresentationForTesting(positionMs: Int, atMs: Int, evidence: LocalVideoEvidence) {
        sampleSurfacePresentation(at: positionMs, evidence: evidence)
        lastSurfaceSampleAtMs = atMs
    }

    var surfaceLastSampleAtMsForTesting: Int? { lastSurfaceSampleAtMs }
    #endif

    /// Film actually playing is the system's hold over, whether or not iOS
    /// ever says so: retire the banner and give the stall monitor back. Kept
    /// out of the sampler, whose evidence must never name a transport status;
    /// this weighs the presenter's verdict AND the transport, because `rate`
    /// alone is only the requested rate and a seek moves the position with
    /// nothing playing.
    private func endSystemHoldIfPlaying() {
        guard systemPaused, surface.presenting, player.timeControlStatus == .playing,
              seekState.pendingMs == nil else { return }
        systemPaused = false
        systemPauseBeganAt = nil
        present(.systemPaused(false))
        noteSurfaceLogOnly("system_hold_ended:presentation")
    }

    /// §4.3's disagreement detector, as a pure function of the evidence the
    /// sampler collected — so a unit test can drive the decision itself, which
    /// is otherwise reachable only from a periodic observer over a decoding
    /// `AVPlayer` no headless test has.
    ///
    /// The film position must have ADVANCED since the previous sample. A
    /// transport that reports itself as playing over a frozen position is
    /// precisely the disagreement this exists to find, so a rate — or a
    /// `timeControlStatus`, or the published `isPlaying` — is necessary and
    /// can never be sufficient. The first sample of an attachment has no
    /// predecessor to beat and counts as movement.
    nonisolated static func surfaceIsPresenting(
        observedPosition: Int,
        lastSampleMs: Int?,
        isChangingStream: Bool,
        rate: Float,
        picture: Bool
    ) -> Bool {
        let moved = observedPosition > (lastSampleMs ?? observedPosition - 1)
        return !isChangingStream && rate > 0 && moved && picture
    }

    #if DEBUG
    /// Test seam, Debug-only and never in a shipped binary: declare that a
    /// frame has been presented for this playback.
    ///
    /// `surfaceContext` is `.start` until one has, and the only producer is
    /// `sampleSurfacePresentation`, which needs a decoded picture, a moving
    /// position and a settled stream. A headless XCTest has none of the three
    /// — an AVPlayerItem over a playlist URL nothing serves never readies, so
    /// `isChangingStream` never clears and the periodic observer returns
    /// early. Without this the `.change` and `.attached` halves of every
    /// context-sensitive rule are unreachable from a unit test. It sets one
    /// piece of evidence and makes no decision of its own — `surfaceContext`
    /// reads it, and the create-retry ladder turns on what that says.
    func noteFramePresentedForTesting() { surfaceHasPresented = true }
    #endif

    /// A `Duration` as whole milliseconds, for the ledger and the log.
    nonisolated static func milliseconds(_ duration: Duration) -> Int {
        let components = duration.components
        return Int(components.seconds * 1_000)
            + Int(components.attoseconds / 1_000_000_000_000_000)
    }

    private func reportSurfaceEvent(
        _ entry: PlaybackSurfaceLog,
        at elapsed: Int,
        snapshot: PlaybackSurfaceHistory.PlayerSnapshot
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        // A fixture error is a client bug, not a surface: it is worth a log
        // line and nothing else.
        let level = entry.event == .error ? "error" : "info"
        postClientLog(ApplePlaybackSurfaceLog(
            level: level,
            event: entry.event.rawValue,
            surfaceClass: entry.cls?.rawValue,
            source: entry.source,
            detail: entry.detail,
            attached: entry.attached,
            intent: entry.intent,
            by: entry.by?.rawValue,
            action: entry.action?.rawValue,
            error: entry.error?.rawValue,
            positionMs: entry.positionMs ?? snapshot.positionMs,
            actions: entry.actions.map(\.rawValue),
            playerStopped: entry.playerStopped,
            rate: snapshot.rate,
            presenting: snapshot.presenting,
            raisedAtMs: elapsed,
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            sessionId: snapshot.sessionId,
            attempt: snapshot.attempt
        ))
    }

    nonisolated static func needsAAC(audioIndex: Int?, decision: Decision) -> Bool {
        guard let audioIndex,
              let codec = decision.audio?.first(where: { $0.index == audioIndex })?.codec.lowercased()
        else { return decision.delivery?.aac ?? decision.transcodeAudio ?? false }
        return !["aac", "ac3", "eac3", "alac", "mp3"].contains(codec)
    }

    // MARK: - Observation and metadata

    /// Install one passive frame-availability tap per attached item. It is
    /// idle outside an interactive seek. A pixel buffer proves that this
    /// item's output reached the requested timeline, but AVPlayerItemVideoOutput
    /// does not acknowledge physical display by AVPlayerLayer; device playback
    /// qualification must still verify that final rendering boundary.
    private func installSeekVideoOutput(
        on item: AVPlayerItem, retaining preparedOutput: AVPlayerItemVideoOutput? = nil
    ) {
        seekPresentationTask?.cancel()
        seekPresentationTask = nil
        seekVideoOutput?.setDelegate(nil, queue: nil)
        let output = preparedOutput ?? AVPlayerItemVideoOutput(pixelBufferAttributes: nil)
        let wakeup = SeekOutputWakeup()
        output.setDelegate(wakeup, queue: .main)
        if !item.outputs.contains(where: { $0 === output }) { item.add(output) }
        seekVideoOutput = output
        seekOutputWakeup = wakeup
    }

    private func beginSeekPresentationMonitor(generation: Int, targetMs: Int) {
        seekPresentationTask?.cancel()
        guard let item = player.currentItem else { return }
        let output = seekVideoOutput
        let wakeup = seekOutputWakeup
        let hasVideo = decision?.source?.videoCodec != nil
        seekPresentationBackgrounded = UIApplication.shared.applicationState == .background
        seekPresentationLifecycle = [
            (UIApplication.didEnterBackgroundNotification, true),
            (UIApplication.willEnterForegroundNotification, false)
        ].map { name, backgrounded in
            NotificationCenter.default.publisher(for: name).sink { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self else { return }
                    self.seekPresentationBackgrounded = backgrounded
                    self.sampleSeekPresentationClocks()
                }
            }
        }
        sampleSeekPresentationClocks()
        let telemetryOwner = snapshotAttempt()
        seekPresentationTask = Task { [weak self, weak item] in
            defer {
                if let self,
                   !self.attemptStillCurrent(telemetryOwner, fence: .seekTelemetrySupersession) {
                    self.abandonSeekMeasurement(generation: generation)
                }
            }
            while !Task.isCancelled {
                guard let self, let item,
                      self.player.currentItem === item,
                      self.seekState.pendingMs == targetMs,
                      self.seekState.generation == generation
                else { return }

                let deadlineReached = self.sampleSeekPresentationClocks()

                var settled = false
                if hasVideo, let output {
                    let itemTime = output.itemTime(
                        forHostTime: ProcessInfo.processInfo.systemUptime
                    )
                    var displayTime = CMTime.invalid
                    if itemTime.isValid,
                       itemTime.seconds.isFinite,
                       output.hasNewPixelBuffer(forItemTime: itemTime),
                       output.copyPixelBuffer(forItemTime: itemTime, itemTimeForDisplay: &displayTime) != nil,
                       displayTime.isValid, displayTime.seconds.isFinite {
                        // Use the returned frame's own display time rather
                        // than assigning the requested clock to an old image.
                        let presentedMs = self.baseMs + max(0, Int(displayTime.seconds * 1_000))
                        settled = self.seekState.presentedVideo(
                            positionMs: presentedMs,
                            generation: generation
                        )
                    }
                } else if !hasVideo,
                          self.player.timeControlStatus == .playing,
                          self.player.rate > 0 {
                    settled = self.seekState.presentedAudio(
                        positionMs: self.realPositionMs(),
                        generation: generation
                    )
                }

                if settled {
                    if let log = self.seekMeasurement.presented(generation: generation) {
                        self.postClientLog(log)
                        if self.requestedSeekGeneration == generation {
                            self.requestedSeekGeneration = nil
                        }
                    }
                    self.currentMs = self.realPositionMs()
                    self.playbackControlPlayerChanged()
                    self.updateNowPlaying()
                    return
                }
                if deadlineReached {
                    // An attached/seeked item that never presents is a stall,
                    // not an indefinitely pending UI state. Recover from a
                    // separate task: reopen installs a new video output and
                    // cancels this monitor, so awaiting it here would cancel
                    // the recovery halfway through its own open.
                    self.currentMs = targetMs
                    // The three epochs this recovery depends on are named by
                    // `.seekPresentationDeadline`. Its seek scope is this
                    // monitor's own `generation`: the loop guard above already
                    // required `seekState.generation == generation` and
                    // nothing has suspended since, so `recovery.seek` is that
                    // same value.
                    let recovery = self.snapshotAttempt()
                    Task { [weak self] in
                        guard let self,
                              self.attemptStillCurrent(recovery, fence: .seekPresentationDeadline),
                              self.wantsPlayback, !self.isPlaybackBlocked, !self.finished,
                              !(self.seekPresentationBackgrounded && hasVideo),
                              self.seekState.pendingMs == targetMs
                        else { return }
                        await self.retrySameDeliveryAfterStall(
                            PlaybackStallEvent(
                                kind: .buffering,
                                action: .reopen,
                                positionMs: targetMs,
                                durationMs: Int(Self.seekPresentationDeadlineSeconds * 1_000)
                            ),
                            consultControl: false
                        )
                    }
                    return
                }
                if let output, let wakeup {
                    output.requestNotificationOfMediaDataChange(withAdvanceInterval: 0.05)
                    await wakeup.waitOrPoll(afterNanoseconds: 50_000_000)
                } else {
                    try? await Task.sleep(for: .milliseconds(50))
                }
            }
        }
    }

    @discardableResult
    private func sampleSeekPresentationClocks() -> Bool {
        let now = ProcessInfo.processInfo.systemUptime
        let generation = seekState.generation
        seekState.observePlaybackClock(
            at: now, rate: Double(player.rate),
            isPlaying: player.timeControlStatus == .playing,
            generation: generation
        )
        return seekState.observePresentationDemand(
            at: now,
            eligible: started && wantsPlayback && !isPlaybackBlocked && !finished && !isChangingStream
                && !(seekPresentationBackgrounded && decision?.source?.videoCodec != nil),
            generation: generation
        )
    }

    private func addPeriodicObserver() {
        let interval = CMTime(seconds: 1, preferredTimescale: 2)
        let observe = makePeriodicPlaybackObservation()
        timeObserver = player.addPeriodicTimeObserver(forInterval: interval, queue: .main) { _ in
            MainActor.assumeIsolated { observe() }
        }
    }

    /// The actual timer callback owns a title, and only samples an attached
    /// item outside preparation. A no-item clock is not a resume position;
    /// removing an observer does not revoke a callback already queued by AVF.
    func makePeriodicPlaybackObservation() -> @MainActor @Sendable () -> Void {
        let lifecycle = lifecycleGeneration
        let observedPlayer = player
        return { [weak self, weak observedPlayer] in
                guard let self, self.isCurrentLifecycle(lifecycle), self.player === observedPlayer,
                      !self.isChangingStream else { return }
                guard let item = self.player.currentItem else {
                    self.blackFrameWatchdog.bind(nil)
                    return
                }
                // Keep an interactive target on screen while its item is being
                // prepared. Reading the predecessor here was the visible snap
                // back after a progress-bar or skip-button command.
                if self.seekState.pendingMs == nil && !self.isChangingStream {
                    self.currentMs = self.realPositionMs()
                }
                // On the same clock that reveals the marker button, so an
                // automatic skip happens where a viewer would first see the
                // button rather than one tick later.
                self.autoSkipActiveMarkerIfNeeded()
                self.playbackControlPlayerChanged()
                self.pgsOverlayPeriodicTick(currentMs: self.currentMs)
                self.progressiveCaptions.mediaChanged()
                // The last rate the viewer was genuinely playing at, so a
                // pause at 1.5× is restored as 1.5× and not as the 0 the
                // transport reports while paused (P2-5).
                guard self.player === observedPlayer, self.player.currentItem === item else { return }
                let observedPosition = self.realPositionMs()
                let isActuallyPlaying = self.player.timeControlStatus == .playing
                    && self.player.rate > 0
                let evidence = self.currentLocalVideoEvidence()
                let localTargetExpected = self.localVideoTargetExpected(evidence)
                self.reportPlaybackTTFFIfNeeded(
                    at: observedPosition,
                    playing: isActuallyPlaying,
                    evidence: evidence
                )
                // Sampled outside the playing branch so a picture that arrives
                // while paused still retires the watchdog.
                if self.blackFrameWatchdog.observe(
                    item: item,
                    positionMs: observedPosition,
                    frameReady: localTargetExpected && evidence.frameReady,
                    localTargetExpected: localTargetExpected,
                    hasVideoSource: self.decision?.source?.videoCodec != nil,
                    playing: isActuallyPlaying
                ) {
                    self.queueBlackFrameRecovery(for: item, at: observedPosition)
                }
                // The surface's evidence, taken from the samples this
                // observer already has: the position delta, the rate, and the
                // first-frame proof for an item that has never presented one.
                self.sampleSurfacePresentation(at: observedPosition, evidence: evidence)
                // In the same turn as the evidence, never before it: AVPlayer
                // invokes this observer when playback stops as well as while
                // it runs, so a wait raised here is raised against a model
                // that has already been told the picture stopped. Raised the
                // other way round, §3.2 would resolve the disagreement against
                // the fault and retire it in the step that made it.
                self.noteMediaWaiting()
                if isActuallyPlaying {
                    self.preferredRate = self.player.rate
                    self.observeAttachmentPlayback(
                        positionMs: observedPosition,
                        playing: true
                    )
                    if self.attachmentRecovery.establishedPlayback {
                        if self.resumeAttempt == nil {
                            self.sameDeliveryStallRecovery.reset()
                        }
                        // The most recent same-delivery recovery proved itself.
                        // A later, independent interruption may reconnect once
                        // too; an immediate repeated failure remains terminal.
                        self.establishedHDRRetryAttempted = false
                    }
                }
                self.stallObservation.noteRecoveredStagnation(
                    self.playbackRecoveryMonitor.takeRecoveredStagnantDurationMs()
                )
                self.observeProgressPastEarlyEnd(self.currentMs)
                if self.isPlaying && self.currentMs - self.lastReportedMs >= 10_000 {
                    self.lastReportedMs = self.currentMs
                    self.report(self.currentMs)
                    self.reportObservedPlaybackStalls(at: self.currentMs)
                }
        }
    }

    /// Feed the established-playback owner from the periodic observation.
    /// Kept as an internal seam so ownership tests can establish the exact
    /// production state without waiting five seconds of wall time.
    func observeAttachmentPlayback(positionMs: Int, playing: Bool) {
        attachmentRecovery.observe(positionMs: positionMs, playing: playing)
    }

    /// Record the bounded retry before it happens, and clear it on every
    /// terminal route. Keeping this state transition outside the notification
    /// closure makes the actual controller wiring regression-testable.
    func prepareObservedEndAction(
        knownDurationMs: Int,
        itemDurationMs: Int?,
        isGrowingPlaylist: Bool,
        endedAt: Int
    ) -> PlayerItemEndAction {
        let action = Self.endAction(
            knownDurationMs: knownDurationMs,
            itemDurationMs: itemDurationMs,
            isGrowingPlaylist: isGrowingPlaylist,
            endedAt: endedAt,
            previousUncorroboratedEndMs: lastUncorroboratedEndMs
        )
        switch action {
        case .reopen:
            lastUncorroboratedEndMs = endedAt
        case .stop, .finish:
            lastUncorroboratedEndMs = nil
        }
        return action
    }

    /// Advancing one tolerance window past the failed boundary proves the
    /// replacement made progress and earns a fresh bounded retry later.
    func observeProgressPastEarlyEnd(_ positionMs: Int) {
        guard let endedAt = lastUncorroboratedEndMs,
              positionMs >= endedAt,
              positionMs - endedAt >= Self.repeatedEndToleranceMs
        else { return }
        lastUncorroboratedEndMs = nil
    }

    func stopAfterRepeatedEarlyEnd(
        at endedAt: Int,
        expectedDurationMs: Int?,
        isGrowingPlaylist: Bool
    ) {
        lastUncorroboratedEndMs = nil
        currentMs = max(0, endedAt)
        report(currentMs)
        stopForBlockingSurface(revokingPlaybackIntent: true)
        finished = false
        raiseOwnerFault(
            source: "repeated_early_end",
            context: .attached,
            title: Self.earlyEndFailureTitle,
            detail: Self.repeatedEarlyEndMessage
        )
        reportEarlyEndFailure(
            at: currentMs,
            expectedDurationMs: expectedDurationMs,
            isGrowingPlaylist: isGrowingPlaylist
        )
        updateNowPlaying()
    }

    func finishAfterObservedEnd(at durationMs: Int) {
        lastUncorroboratedEndMs = nil
        isPlaying = false
        wantsPlayback = false
        currentMs = max(0, durationMs)
        report(currentMs)
        updateNowPlaying()
        finished = true
    }

    private func retireItemObserver() {
        retireAutoTransferMetrics()
        itemObserver?.cancel()
        itemObserver = nil
        itemEventTask?.cancel()
        itemEventTask = nil
        lastItemFailureDetail = nil
    }

    private func installItemObserver(for item: AVPlayerItem) {
        installAutoTransferMetrics(for: item)
        let observer = AVPlayerItemObserver(item: item, player: player)
        itemObserver = observer
        itemEventTask = Task { @MainActor [weak self, weak observer] in
            guard let observer else { return }
            for await event in observer.events {
                guard let self, self.itemObserver === observer,
                      self.player.currentItem === item else { return }
                switch event {
                case .status(.failed):
                    await self.handleItemFailure(item)
                case .failedToPlayToEnd(let error):
                    await self.handleItemFailure(item, fatalError: error)
                case .playedToEnd:
                    await self.handleObservedEnd(item)
                case .playbackStalled:
                    // The existing recovery evidence poll owns the stall
                    // policy; this notification is never a terminal rung.
                    break
                case .newErrorLogEntry, .timeControl, .interruption, .routeLost, .status:
                    break
                }
            }
        }
    }

    private func handleObservedEnd(_ item: AVPlayerItem) async {
        guard started, !isChangingStream, player.currentItem === item else { return }
        let endedAt = realPositionMs()
        let itemDurationSeconds = item.duration.seconds
        let itemDurationMs = itemDurationSeconds.isFinite && itemDurationSeconds > 0
            ? Int(itemDurationSeconds * 1000)
            : nil
        let isGrowingPlaylist = sessionId != nil && !isVOD
        switch prepareObservedEndAction(
            knownDurationMs: knownDurationMs,
            itemDurationMs: itemDurationMs,
            isGrowingPlaylist: isGrowingPlaylist,
            endedAt: endedAt
        ) {
        case .reopen:
            // A growing playlist may announce its present end before playback
            // is finished. Keep the same rolling reopen budget as before.
            guard recoveryReopenBudget.admit() else {
                let expectedDurationMs = knownDurationMs > 0 ? knownDurationMs : itemDurationMs
                stopAfterRepeatedEarlyEnd(
                    at: endedAt,
                    expectedDurationMs: expectedDurationMs,
                    isGrowingPlaylist: isGrowingPlaylist
                )
                return
            }
            await reopen(at: endedAt)
        case .stop:
            let expectedDurationMs = knownDurationMs > 0 ? knownDurationMs : itemDurationMs
            stopAfterRepeatedEarlyEnd(
                at: endedAt,
                expectedDurationMs: expectedDurationMs,
                isGrowingPlaylist: isGrowingPlaylist
            )
        case .finish(let durationMs):
            finishAfterObservedEnd(at: durationMs)
        }
    }

    /// A failed item advances the recovery ladder — unless a stream change is
    /// already replacing it. The predecessor's failure during a seek or track
    /// change is expected noise: server-side supersession deletes its playlist
    /// the moment the successor's create begins, so its outstanding fetches
    /// 404 while the new session spins up. Reacting to that raced a second
    /// open against the one in flight — flipping delivery down the SDR or
    /// transcode ladder, or stopping playback outright, mid-seek. `open()`
    /// re-checks the successor's own status once the change lands, so a
    /// genuinely failed replacement is still handled.
    private func handleItemFailure(_ item: AVPlayerItem, fatalError: NSError? = nil) async {
        guard player.currentItem === item, !isChangingStream else { return }
        let itemError = item.error as NSError?
        let detail = PlayerItemFailure.classify(
            fatal: fatalError,
            item: itemError,
            log: item.errorLog()?.events.map(PlayerItemFailure.LogEntry.init) ?? [],
            failedURI: Self.failedResourceURI(item: item, error: fatalError ?? itemError)
        )
        lastItemFailureDetail = detail.map { (item, $0) }
        let isCompatibilityFailure = Self.isCompatibilityPlaybackFailure(
            error: detail?.error,
            eventDomain: detail?.eventDomain,
            eventStatus: detail?.eventStatus,
            eventComment: detail?.eventComment
        )
        let decoderAttempt = snapshotAttempt()
        let autoDecodeRecovery = isCompatibilityFailure ? await noteAutoDecodeFailure() : false
        guard player.currentItem === item,
              attemptStillCurrent(decoderAttempt, fence: .itemDecoderAcknowledgement) else { return }
        // Only a transport failure can be answered by another node; see
        // `isTransportPlaybackFailure`. `!isCompatibilityFailure` is not the
        // same question and would walk the whole list for a terminal 404.
        let isTransportFailure = Self.isTransportPlaybackFailure(
            error: detail?.error,
            eventDomain: detail?.eventDomain,
            eventStatus: detail?.eventStatus
        )
        if let retiredSession = sessionId, Self.parksPausedItemFailure(
            started: started,
            wantsPlayback: wantsPlayback,
            isRollingSession: !isVOD && !isDirectPlayback,
            isCompatibilityFailure: isCompatibilityFailure,
            resumeInFlight: resumeAttempt != nil
        ) {
            // Paused, and the item died under a viewer who is not watching —
            // on a rolling session this is the pause grace retiring the
            // presentation (§9.5). Walking the node list or raising
            // "Playback stopped." here is what this latch replaces: every
            // node answers the same 404/410, and the error is for nobody.
            // The dead item's clock is unreliable, so the position is the
            // last one the periodic observer saw.
            pausedRetirement = PausedRetirement(
                sessionId: retiredSession,
                positionMs: Self.compatibilityRetryPositionMs(lastObservedMs: currentMs)
            )
            noteSurfaceLogOnly("paused_item_failure:\(detail?.eventStatus.map(String.init) ?? "none")")
            return
        }
        if let attempt = resumeAttempt {
            guard attempt.publicationCompleted else {
                // Preserve the failed item until the urgent Resume publication
                // has established its sequence floor. Classification above is
                // repeated against this same identity when publication lands.
                resumePendingFailedItem = item
                resumeRepairPendingReason = nil
                return
            }
            let reason = isTransportFailure && !isCompatibilityFailure
                ? "item-failure-transport"
                : isCompatibilityFailure
                    ? "item-failure-compatibility"
                    : "item-failure-same-delivery"
            guard claimResumeRepair(at: resumeNow(), reason: reason) != nil else {
                return
            }
        }
        // Publish this failure and wait briefly for the verdict it earns. The
        // ask goes here, ahead of every rung, because the answer governs one
        // of them and the exchange has to be out before any reopen replaces
        // the item it describes.
        //
        // What the answer does NOT do is end the ladder, which is what this
        // originally shipped doing and what #722's review corrected. `terminal`
        // is emitted only for `unsupported` and `invalid_configuration`, and
        // the server documents `is_permanent` as "whether retrying this
        // source, *unchanged*, can ever succeed" — `unsupported`'s own
        // sentence is "this source cannot be carried by *this delivery
        // pipeline*". It is a verdict about the recipe, not the source: the
        // compatibility fallback asks for a different pipeline, which is
        // exactly what the verdict leaves open, and the server admits its own
        // `execute_prepublication_copy_retry` for the same reason. Ending the
        // ladder on it deleted the rung most likely to still produce a picture
        // and captioned the failure with a sentence about a pipeline nobody
        // was proposing any more.
        //
        // So it governs one thing: whether the established-HDR rung spends its
        // reconnect. It does not govern whether that rung *runs* — that rung
        // also carries a stop, and a stop is not a retry. And it does not
        // govern the compatibility fallback below it, which changes the
        // recipe. A `hold` or a `retry_resource` governs nothing at all: on a
        // dead item they would leave a player with no path forward.
        let failureAttempt = snapshotAttempt()
        _ = await controlVerdictForItemFailure(item)
        guard attemptStillCurrent(failureAttempt, fence: .itemFailureLadder),
              player.currentItem === item,
              !isChangingStream else { return }
        var reportedFailure = false
        if autoDecodeRecovery, started {
            await reopen(at: Self.compatibilityRetryPositionMs(lastObservedMs: currentMs), intent: .sameDeliveryRepair)
            return
        }
        if started, !isCompatibilityFailure, isTransportFailure {
            // The log goes out before the retry, not after it: a successful
            // node failover replaces this item and returns, so reporting
            // afterwards lost the telemetry for exactly the case the failover
            // exists to handle. Moving to another node buys no compatibility
            // rung, so the step names none — and `isCompatibilityFailure` is
            // false here, which is the same fallback the block below computes.
            reportPlaybackFailure(
                item,
                step: PlaybackCompatibilityLadderStep(
                    cause: .itemFailure,
                    fallback: PlaybackCompatibilityFallback.none
                )
            )
            reportedFailure = true
            if await retryMediaOnNextNode(item) { return }
        }
        // Evaluated before anything recovers, because the log has to go out
        // before the ladder's own reopen replaces this item. An established
        // HDR delivery reconnects itself instead of descending, so it buys no
        // rung — say so rather than naming one that never ran.
        let reconnectsEstablishedHDR = Self.shouldPreserveEstablishedHDRDelivery(
            deliveredRange: deliveredRange,
            establishedPlayback: attachmentRecovery.establishedPlayback
        )
        let fallback: PlaybackCompatibilityFallback =
            started && isCompatibilityFailure && !reconnectsEstablishedHDR
                ? plannedCompatibilityFallback
                : PlaybackCompatibilityFallback.none
        if !reportedFailure {
            reportPlaybackFailure(
                item,
                step: PlaybackCompatibilityLadderStep(cause: .itemFailure, fallback: fallback)
            )
        }
        if started {
            // P2-6: this item is already dead, so its `currentTime()`
            // is 0 or invalid and a VOD/direct retry would silently
            // restart the film at 0:00. The last position the periodic
            // observer saw is the truthful retry point. The transport
            // intent survives in `wantsPlayback`, so a viewer who was
            // paused when the item failed stays paused.
            let position = Self.compatibilityRetryPositionMs(lastObservedMs: currentMs)
            if await retryEstablishedHDRDelivery(
                at: position,
                unchangedRetryRuledOut: unchangedRetryRuledOut(
                    isTransportFailure: isTransportFailure
                )
            ) { return }
            if isCompatibilityFailure,
               await retryWithNextCompatibilityFallback(at: position) { return }
        }
        stopForBlockingSurface(revokingPlaybackIntent: true)
        isChangingStream = false
        // AVPlayer's own error object is a generic fallback and its message
        // can carry a media URL, so an armed server verdict is both more
        // accurate and safer to show.
        //
        // Except for a transport failure, which is the one case the verdict
        // demonstrably does not explain: a verdict says production stopped
        // for a reason retrying cannot change, and a link that dropped is a
        // different cause with a different answer. Showing it there would be
        // the failure mode of arming a verdict at all — a confident sentence
        // about the wrong thing.
        let verdict = isTransportFailure ? nil : playbackControl.terminalVerdict?.message
        // A media refusal reaches AVFoundation as a status code and never as a
        // body (§2.2), so the adapter classifies on the code it has and never
        // claims a sentence it cannot have.
        raiseOwnerFault(
            source: Self.mediaFailureSurfaceSource(
                statusCode: detail?.eventStatus,
                context: surfaceContext
            ),
            title: currentMs > 0
                ? Self.playbackStoppedFailureTitle
                : Self.playbackStartFailureTitle,
            detail: verdict
                ?? detail?.error?.localizedDescription
                ?? PlaybackPreparationError.failed.localizedDescription,
            positionMs: realPositionMs()
        )
    }

    /// Reattach the exact media path through another ingress. The existing
    /// session capability and delivery mode stay intact, so this path never
    /// consumes the HDR/codec compatibility ladder.
    ///
    /// It attaches an item, so it takes the same generation fence `open()`
    /// takes. Without it a `stop()` landing during one of the awaits below
    /// resurrects a player nobody is watching — audible, with background audio
    /// on — and a status callback in `open()`'s own post-`isChangingStream`
    /// window can clobber the item that open is still configuring.
    func retryMediaOnNextNode(_ failedItem: AVPlayerItem) async -> Bool {
        #if os(iOS)
        if offlineId != nil { return false }
        #endif
        guard player.currentItem === failedItem,
              let path = activeMediaPath,
              let url = Session.shared.nextMediaFailoverURL(
                path,
                authenticated: activeMediaAuthenticated
              ) else { return false }
        openGeneration &+= 1
        let generation = openGeneration
        // A growing session's playlist cannot be resumed by seeking to a film
        // position — the successor renumbers — but an item-local position is
        // exactly what a replacement generation understands, and dropping it
        // would throw a viewer twenty minutes in back to the session origin.
        let resume = usesDirectTimeline ? currentMs : max(currentMs - baseMs, 0)
        let seekOwner = seekPreparationOwner(at: currentMs)
        let executedSeekGeneration = seekState.generation
        let executedSeekTarget = seekState.pendingMs
        isChangingStream = true
        let item = AVPlayerItem(url: url)
        Self.configureBuffering(item, growingHLS: sessionId != nil && !isVOD)
        installSeekVideoOutput(on: item)
        #if os(iOS)
        item.externalMetadata = [titleMetadata(title)]
        #endif
        retireItemObserver()
        pgsOverlayItemGeneration &+= 1
        pgsOverlayWindowTask?.cancel()
        pgsOverlayLoadingRange = nil
        pgsOverlayWindowFailures = 0
        pgsOverlayWindow = nil
        player.replaceCurrentItem(with: item)
        installItemObserver(for: item)
        // Failover replaces the native item just as open() does. Faults from
        // its predecessor cannot remain over the successor's recovered video.
        present(.attach(generation))
        if resumeAttempt?.repairAdmitted == true {
            bindResumeRepairSuccessor(item, generation: generation)
        }
        // Failover advanced the open fence. Rebind its one poll before waiting;
        // a late predecessor response must not supply preparation evidence.
        startRecoveryEvidencePoll()
        if wantsPlayback { player.play() } else { player.pause() }
        // The overlay is per-item and was just torn down; `open()` rebuilds it
        // at this point and so must this, or a PGS-subtitled film loses its
        // subtitles at the first failover and never gets them back. It wants
        // SOURCE time — `resume` is deliberately item-local for a growing
        // session, and passing it would load the window for the wrong part of
        // the film whenever `baseMs` is nonzero.
        refreshPGSOverlayWindow(at: currentMs, reason: .force)
        do { try await seekWhenReady(item, ms: resume, owner: seekOwner) }
        catch {
            // The status observer will drive the next node or terminal
            // surface after this transition leaves its suppression window.
        }
        // A newer `open()` owns the transition now, and with it
        // `isChangingStream` — clearing it here would un-suppress the status
        // observer while that open is still configuring its item, which is
        // the race this fence exists to prevent.
        guard !isSuperseded(generation) else { return true }
        guard started else {
            isChangingStream = false
            return true
        }
        await reconcileNativeMediaSelections(to: item)
        guard !isSuperseded(generation) else { return true }
        guard started else {
            isChangingStream = false
            return true
        }
        applyDisplayCriteria(for: item, generation: generation)
        if wantsPlayback { player.play() } else { player.pause() }
        isPlaying = wantsPlayback
        isChangingStream = false
        if let target = seekState.pendingMs {
            let seekGeneration = seekState.generation
            if recipeRevision.needsReopen || seekGeneration != executedSeekGeneration
                || target != executedSeekTarget {
                issueSeek(to: target, generation: seekGeneration)
            } else if seekState.markExecuted(generation: seekGeneration, targetMs: target) {
                beginSeekPresentationMonitor(generation: seekGeneration, targetMs: target)
            }
        }
        if item.status == .failed {
            await handleItemFailure(item)
        }
        return true
    }

    /// The server-relative form of a delivery URL, or nil when it does not
    /// belong to this server.
    ///
    /// Compared on the canonical origin, so `https://h` and `https://h:443`
    /// do not read as two servers — that mismatch silently disabled failover
    /// for the whole playback, with nothing in the log to say why.
    private func clusterRelativeMediaPath(_ value: String) -> String? {
        if value.hasPrefix("/"), !value.hasPrefix("//") { return value }
        guard let candidate = URLComponents(string: value),
              let scheme = candidate.scheme,
              let host = candidate.host,
              let current = Session.shared.canonicalPrimaryOrigin else { return nil }
        var authority = "\(scheme)://\(host)"
        if let port = candidate.port { authority += ":\(port)" }
        guard Session.canonicalOrigin(authority) == current else { return nil }
        var path = candidate.percentEncodedPath
        if path.isEmpty { path = "/" }
        if let query = candidate.percentEncodedQuery { path += "?\(query)" }
        return path
    }

    private static func failedResourceURI(item: AVPlayerItem, error: NSError?) -> String? {
        if let uri = error?.userInfo[NSURLErrorFailingURLStringErrorKey] as? String {
            return uri
        }
        if let url = error?.userInfo[NSURLErrorFailingURLErrorKey] as? URL {
            return url.absoluteString
        }
        return (item.asset as? AVURLAsset)?.url.absoluteString
    }

    private func reportPlaybackFailure(
        _ item: AVPlayerItem,
        step: PlaybackCompatibilityLadderStep
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        #if os(tvOS)
        let audio = AVAudioSession.sharedInstance()
        let rate = audio.sampleRate.isFinite && (0...384_000).contains(audio.sampleRate) ? Int(audio.sampleRate) : 0
        noteSurfaceLogOnly("audio_route_at_item_failure:outputs=\(audio.currentRoute.outputs.count):sample_rate=\(rate)")
        #endif
        let itemError = item.error as NSError?
        let detail = lastItemFailureDetail?.item === item
            ? lastItemFailureDetail?.detail
            : PlayerItemFailure.classify(
                fatal: nil,
                item: itemError,
                log: item.errorLog()?.events.map(PlayerItemFailure.LogEntry.init) ?? [],
                failedURI: Self.failedResourceURI(item: item, error: itemError)
            )
        let failure = detail?.error
        let payload = ApplePlaybackFailureLog(
            message: failure?.localizedDescription
                ?? PlaybackPreparationError.failed.localizedDescription,
            method: clientLogMethod,
            code: failure?.code,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            detail: Self.playbackFailureDetail(
                error: failure,
                eventDomain: detail?.eventDomain,
                eventStatus: detail?.eventStatus,
                eventComment: detail?.eventComment,
                ladderStep: step
            )
        )
        postClientLog(payload)
    }

    /// A ladder entry the client decided on its own: no AVFoundation error
    /// exists to describe it, so the log carries only the cause and the rung.
    private func reportCompatibilityLadderFailure(
        message: String,
        step: PlaybackCompatibilityLadderStep
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        postClientLog(ApplePlaybackFailureLog(
            message: message,
            method: clientLogMethod,
            code: nil,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            detail: Self.playbackFailureDetail(
                error: nil,
                eventDomain: nil,
                eventStatus: nil,
                eventComment: nil,
                ladderStep: step
            )
        ))
    }

    private func reportEarlyEndFailure(
        at positionMs: Int,
        expectedDurationMs: Int?,
        isGrowingPlaylist: Bool
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        guard model != nil else { return }
        postClientLog(ApplePlaybackEarlyEndLog(
            positionMs: positionMs,
            expectedDurationMs: expectedDurationMs,
            isGrowingPlaylist: isGrowingPlaylist,
            message: Self.repeatedEarlyEndMessage,
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec
        ))
    }

    private func abandonSeekMeasurement(generation: Int? = nil) {
        if let log = seekMeasurement.abandoned(generation: generation) { postClientLog(log) }
        if generation == nil || requestedSeekGeneration == generation {
            requestedSeekGeneration = nil
        }
    }

    @discardableResult
    private func queueBlackFrameRecovery(for item: AVPlayerItem, at position: Int) -> Task<Void, Never> {
        let attempt = snapshotAttempt()
        return Task { @MainActor [weak self] in
            guard let self else { return }
            guard self.started,
                  self.attemptStillCurrent(attempt, fence: .blackFrameDecodeFailure),
                  self.player.currentItem === item,
                  self.blackFrameRecoveryIsEligible(for: item) else {
                // Epoch/item refusals can short-circuit before eligibility is
                // sampled. They must still release this attachment's trigger.
                self.blackFrameWatchdog.cancelRecovery(for: item)
                return
            }
            await self.handleBlackFrameDecodeFailure(at: position)
        }
    }

    private func localVideoTargetExpected(_ evidence: LocalVideoEvidence) -> Bool {
        evidence.bindingMatches && evidence.targetVisible
            && !player.isExternalPlaybackActive && !pictureInPictureIsActive
            && seekState.pendingMs == nil && !isChangingStream
    }

    func blackFrameRecoveryIsEligible(for item: AVPlayerItem) -> Bool {
        guard player.currentItem === item else { return false }
        let evidence = currentLocalVideoEvidence()
        let local = localVideoTargetExpected(evidence)
        return blackFrameWatchdog.recoveryIsEligible(
            for: item, frameReady: local && evidence.frameReady,
            eligible: started && local && decision?.source?.videoCodec != nil
                && player.timeControlStatus == .playing && player.rate > 0)
    }

    private func reportPlaybackTTFFIfNeeded(at positionMs: Int, playing: Bool, evidence: LocalVideoEvidence) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        let firstOutput = PlaybackFirstOutputEvidence.observed(
            hasVideoSource: decision?.source.map { $0.videoCodec != nil },
            localTargetExpected: localVideoTargetExpected(evidence), evidence: evidence)
        guard let ms = ttffMeasurement.observe(positionMs: positionMs, playing: playing,
            evidence: firstOutput, seekPending: seekState.pendingMs != nil) else {
            return
        }
        lastTTFFMs = ms
        let method = clientLogMethod
        let height = sessionStatus?.targetHeight
            ?? selectedHeight
            ?? (method == "transcode" ? nil : decision?.source?.height)
        postClientLog(ApplePlaybackTTFFLog(
            ms: ms,
            method: method,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            height: height,
            encoder: encoder,
            sessionId: sessionId,
            attempt: playbackAttemptId,
            reason: ttffReason,
            evidence: firstOutput.label
        ))
    }

    private func reportPlaybackProbe(at positionMs: Int) {
        recordDiagnosticPlayback()
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        let method = clientLogMethod
        let height = sessionStatus?.targetHeight
            ?? selectedHeight
            ?? (method == "transcode" ? nil : decision?.source?.height)
        postClientLog(ApplePlaybackProbeLog(
            method: method,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            height: height,
            encoder: encoder,
            sessionId: sessionId,
            attempt: playbackAttemptId,
            snapshot: playbackDiagnosticSnapshot(at: positionMs),
            detail: localVideoDiagnosticDetail()
        ))
    }

    private func localVideoDiagnosticDetail() -> String {
        let evidence = currentLocalVideoEvidence()
        let size = presentationSize
        let source = decision?.source.map { $0.videoCodec == nil ? "audio" : "video" } ?? "unknown"
        return "source=\(source) binding=\(evidence.bindingMatches) visible=\(evidence.targetVisible)"
            + " ready=\(evidence.readyForDisplay) item=\(player.currentItem?.status.rawValue ?? -1)"
            + " dimensions=\(size.width)x\(size.height) playing=\(player.timeControlStatus == .playing && player.rate > 0)"
            + " external=\(player.isExternalPlaybackActive) pip=\(pictureInPictureIsActive) black_ms=\(blackFrameWatchdog.blackMs)"
    }

    private func reportPlaybackStall(
        _ event: PlaybackStallEvent,
        outcome: SameDeliveryStallRecoveryOutcome
    ) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        let payload = ApplePlaybackStallLog(
            kind: event.kind,
            outcome: outcome,
            positionMs: event.positionMs,
            durationMs: event.durationMs,
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            encoder: encoder,
            sessionId: sessionId,
            attempt: playbackAttemptId,
            snapshot: playbackDiagnosticSnapshot(at: event.positionMs)
        )
        postClientLog(payload)
    }

    private func reportObservedPlaybackStalls(at positionMs: Int) {
        #if os(iOS)
        if offlineId != nil { return }
        #endif
        guard let observed = stallObservation.take(numberOfStalls: stalls) else { return }
        postClientLog(ApplePlaybackObservedStallLog(
            delta: observed.delta,
            positionMs: positionMs,
            stagnantDurationMs: observed.stagnantDurationMs,
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            vcodec: decision?.source?.videoCodec,
            encoder: encoder,
            sessionId: sessionId,
            attempt: playbackAttemptId,
            snapshot: playbackDiagnosticSnapshot(at: positionMs)
        ))
    }

    private func postClientLog<Payload: Encodable>(_ payload: Payload) {
        guard let url = Session.shared.url("/api/v1/client-log"),
              let body = try? JSONEncoder().encode(payload)
        else { return }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.httpBody = body
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        Session.shared.authorize(&request)
        Task {
            _ = try? await URLSession.shared.data(for: request)
        }
    }

    private func reportMarkerEvent(_ event: String, detail: String, message: String) {
        postClientLog(AppleMarkerPlaybackLog(
            event: event,
            message: message,
            method: clientLogMethod,
            title: title,
            fileId: fileId,
            detail: detail
        ))
    }

    /// `ladderStep` names the recovery this failure bought, so a device log
    /// shows which rung was chosen and what chose it — including for the two
    /// client-observed verdicts (a readiness timeout, a black picture) that
    /// carry no AVFoundation error at all.
    static func playbackFailureDetail(
        error: NSError?,
        eventDomain: String?,
        eventStatus: Int?,
        eventComment: String?,
        ladderStep: PlaybackCompatibilityLadderStep? = nil
    ) -> String {
        var fields: [String] = []
        if let error {
            fields.append("error=\(error.domain):\(error.code)")
            if let underlying = error.userInfo[NSUnderlyingErrorKey] as? NSError {
                fields.append(
                    "underlying=\(underlying.domain):\(underlying.code) \(underlying.localizedDescription)"
                )
            }
        }
        if let eventDomain, let eventStatus {
            fields.append("event=\(eventDomain):\(eventStatus)")
        } else if let eventDomain {
            fields.append("event=\(eventDomain)")
        } else if let eventStatus {
            fields.append("event_status=\(eventStatus)")
        }
        if let eventComment, !eventComment.isEmpty {
            fields.append("comment=\(eventComment)")
        }
        if let ladderStep {
            fields.append("cause=\(ladderStep.cause.rawValue)")
            fields.append("ladder=\(ladderStep.fallback.telemetryName)")
        }
        return fields.joined(separator: " · ")
    }

    static func shouldRetryWithCompatibilityTranscode(
        canRetry: Bool,
        alreadyAttempted: Bool
    ) -> Bool {
        canRetry && !alreadyAttempted
    }

    /// Route an interactive seek. A growing HLS session's playlist advertises
    /// a real seekable window — everything published and not yet pruned — and
    /// AVPlayer seeks inside it instantly. Replacing the server session is
    /// only necessary when the target lies outside that window (not yet
    /// transcoded, or already pruned by retention). Before this routing every
    /// non-VOD seek was a full session teardown and create: multi-second
    /// spinner per scrub, one server round trip per ±10 s press.
    ///
    /// `seekableRangesMs` is in the item's local clock; `targetMs`/`baseMs`
    /// are film time, with `baseMs` the film position of item-local zero.
    /// The holdback keeps a native landing short of the live edge, where no
    /// future media exists yet; a target just past the edge snaps to the
    /// holdback instead of paying a whole reopen for a couple of seconds.
    nonisolated static func seekRoute(
        targetMs: Int,
        baseMs: Int,
        usesDirectTimeline: Bool,
        isVOD: Bool,
        isChangingStream: Bool,
        seekableRangesMs: [ClosedRange<Int>],
        liveEdgeHoldbackMs: Int = 1_500,
        liveEdgeSnapWindowMs: Int = 2_500
    ) -> PlayerSeekRoute {
        // A change in flight owns the player; the reopen queue serializes
        // behind it. This branch also keeps the old invariant that the
        // native leg below never runs with a nonzero base on VOD/direct.
        if isChangingStream { return .reopen }
        if usesDirectTimeline || isVOD {
            // Fully seekable timelines whose local clock is film time.
            return .native(itemMs: targetMs)
        }
        let local = targetMs - baseMs
        // Exact containment first, across every advertised range: a target
        // inside a later range must seek there, not snap to an earlier
        // range's edge (PR #122 review: [0…10 s, 11 s…90 s] with a 12 s
        // target belongs at 12 s, not at the first range's 8.5 s holdback).
        for range in seekableRangesMs {
            let safeUpper = range.upperBound - liveEdgeHoldbackMs
            guard safeUpper >= range.lowerBound else { continue }
            if local >= range.lowerBound && local <= safeUpper {
                return .native(itemMs: local)
            }
        }
        // Snapping applies only at the live edge — the range with the
        // greatest upper bound — never across an interior gap, whose media
        // genuinely is not in the playlist.
        if let latest = seekableRangesMs.max(by: { $0.upperBound < $1.upperBound }) {
            let safeUpper = latest.upperBound - liveEdgeHoldbackMs
            if safeUpper >= latest.lowerBound,
               local > safeUpper,
               local <= latest.upperBound + liveEdgeSnapWindowMs {
                return .native(itemMs: safeUpper)
            }
        }
        return .reopen
    }

    /// Whether this sample was taken during an explicit network wait. The
    /// shared stall clock counts regardless — crossing regimes must never
    /// restart it — but the tally of waiting samples decides the fired
    /// event's kind without classifying its cause. This applies equally to
    /// iPhone, iPad, and Apple TV.
    static func shouldMonitorBufferingStall(
        timeControlStatus: AVPlayer.TimeControlStatus
    ) -> Bool {
        timeControlStatus == .waitingToPlayAtSpecifiedRate
    }

    /// Failures a *different node* could plausibly answer differently.
    ///
    /// Deliberately an allowlist, because "not a codec failure" is a much
    /// larger set: an ended session's 404, a refused credential, and a DRM
    /// error are all reproducible on every ingress, and walking the node list
    /// for one of those costs a full item attach per node before the viewer
    /// sees the error they were always going to see. Only a transport
    /// failure — the connection, not the answer — moves.
    static func isTransportPlaybackFailure(
        error: NSError?,
        eventDomain: String?,
        eventStatus: Int?
    ) -> Bool {
        var chain: [NSError] = []
        var next = error
        while let current = next, chain.count < 8 {
            chain.append(current)
            next = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }
        let transportURLErrors: Set<Int> = [
            NSURLErrorTimedOut,
            NSURLErrorCannotFindHost,
            NSURLErrorCannotConnectToHost,
            NSURLErrorNetworkConnectionLost,
            NSURLErrorDNSLookupFailed,
            NSURLErrorResourceUnavailable,
            NSURLErrorNotConnectedToInternet,
            NSURLErrorBadServerResponse,
            NSURLErrorSecureConnectionFailed,
        ]
        if chain.contains(where: {
            $0.domain == NSURLErrorDomain && transportURLErrors.contains($0.code)
        }) {
            return true
        }
        // Deliberately no `AVFoundationErrorDomain` case here. Every AVError
        // that reaches this path is a verdict about the media, which the next
        // node gives too; the transport failures AVFoundation sees arrive as
        // the `NSURLErrorDomain` chain above or as an access-log status below.
        //
        // An access-log event carries the HTTP status the media stack saw. A
        // 5xx is the node; a 4xx is the answer, and every node gives it.
        //
        // The status is read without regard to `eventDomain`, exactly as
        // `isCompatibilityPlaybackFailure` reads its decoder codes below. An
        // `AVPlayerItemErrorLogEvent` reports the transfer's HTTP status while
        // naming `CoreMediaErrorDomain`, so requiring `NSURLErrorDomain` here
        // made this branch unreachable outside its own fixture and a 5xx on a
        // segment never moved off the failing node. HTTP statuses are positive
        // and three-digit; the CoreMedia codes the sibling matches are
        // negative, so the two ranges cannot collide.
        if let status = eventStatus, (500..<600).contains(status) {
            return true
        }
        return false
    }

    /// Only a media/container/decoder rejection may advance the compatibility
    /// ladder. Timeouts, HTTP failures, and other transport errors must never
    /// spend the HDR fallbacks merely because they happened before first frame.
    static func isCompatibilityPlaybackFailure(
        error: NSError?,
        eventDomain: String?,
        eventStatus: Int?,
        eventComment: String?
    ) -> Bool {
        var chain: [NSError] = []
        var next = error
        while let current = next, chain.count < 8 {
            chain.append(current)
            next = current.userInfo[NSUnderlyingErrorKey] as? NSError
        }

        let avMediaCodes: Set<AVError.Code> = [
            .decodeFailed,
            .invalidSourceMedia,
            .fileFormatNotRecognized,
            .fileFailedToParse,
            .decoderNotFound,
            .incompatibleAsset,
            .failedToParse,
            .undecodableMediaData,
            .formatUnsupported,
        ]
        if chain.contains(where: { candidate in
            guard candidate.domain == AVFoundationErrorDomain,
                  let code = AVError.Code(rawValue: candidate.code)
            else { return false }
            return avMediaCodes.contains(code)
        }) {
            return true
        }

        // VideoToolbox decoder failures commonly surface as an underlying
        // CoreMedia/OSStatus error instead of an AVError.Code. The two
        // CoreMedia codes below are media verdicts too, not transport faults:
        // a device with no Dolby Vision Profile 5 decoder rejects the asset
        // outright (-12927), or fails to build a decoder session from the
        // empty `hvcC` such a track carries (-15517). Leaving them out
        // classified the most common Dolby Vision failure as terminal, so the
        // ladder never ran and the viewer kept a black screen.
        let decoderStatusCodes: Set<Int> = [
            -12906, // kVTCouldNotFindVideoDecoderErr
            -12909, // kVTVideoDecoderBadDataErr
            -12910, // kVTVideoDecoderUnsupportedDataFormatErr
            -12911, // kVTVideoDecoderMalfunctionErr
            -12927, // kFigPlayerError_IncompatibleAsset
            -15517, // decoder initialization failed (empty hvcC)
            -17694, // kVTVideoDecoderReferenceMissingErr
        ]
        if chain.contains(where: { decoderStatusCodes.contains($0.code) })
            || eventStatus.map(decoderStatusCodes.contains) == true {
            return true
        }

        let diagnostic = ([eventDomain, eventComment].compactMap { $0 }
            + chain.flatMap { [$0.domain, $0.localizedDescription] })
            .joined(separator: " ")
            .lowercased()
        let mediaTerms = [
            "decoder failed",
            "decoder rejected",
            "could not decode",
            "codec is not supported",
            "codec not supported",
            "format is not supported",
            "format not supported",
            "undecodable media",
        ]
        return mediaTerms.contains(where: diagnostic.contains)
    }

    static func nextCompatibilityFallback(
        canRetryWithHDRBase: Bool,
        hdrBaseAlreadyAttempted: Bool,
        canRetryWithTranscode: Bool,
        transcodeAlreadyAttempted: Bool
    ) -> PlaybackCompatibilityFallback {
        if canRetryWithHDRBase && !hdrBaseAlreadyAttempted { return .hdrBase }
        if shouldRetryWithCompatibilityTranscode(
            canRetry: canRetryWithTranscode,
            alreadyAttempted: transcodeAlreadyAttempted
        ) {
            return .transcode
        }
        return .none
    }

    /// Profile 8.1/8.4 sources carry a complete HDR base picture beneath
    /// their Dolby Vision metadata. If VideoToolbox silently rejects that
    /// title's RPU/configuration, the server can strip only the DV layer and
    /// keep the same 10-bit HEVC picture instead of tone-mapping to SDR.
    static func hasCompatibleDolbyVisionBase(_ hdrFormat: String?) -> Bool {
        let format = hdrFormat?.lowercased() ?? ""
        return format.contains("hdr10-compatible") || format.contains("hlg-compatible")
    }

    static let hdrSubtitleNotice =
        "That subtitle requires an SDR burn-in. HDR playback was kept unchanged."
    nonisolated static let pgsOverlayExternalPlaybackNotice =
        "PGS overlays stay in the app and are not available in Picture in Picture, AirPlay, or external playback. HDR playback was kept unchanged."

    static func isHDRDelivery(_ deliveredRange: String?) -> Bool {
        guard let range = deliveredRange?.lowercased() else { return false }
        return ["dolby_vision", "hdr10", "hlg"].contains(range)
    }

    /// Burn-only bitmap and styled subtitles can only be drawn by the server's
    /// H.264 SDR pipeline. A recognized PGS application overlay is not a burn;
    /// refuse only the selections that would actually replace HDR video.
    static func subtitleBurnWouldDiscardHDR(
        _ index: Int?,
        tracks: [SubtitleTrack],
        deliveredRange: String?
    ) -> Bool {
        guard let index, isHDRDelivery(deliveredRange) else { return false }
        return subtitleRequiresBurn(index, in: tracks)
    }


    /// Whether the subtitle a starting playback wants must be dropped because
    /// drawing it would have traded the HDR picture away.
    ///
    /// Two vetoes, and the server's counts **only when a subtitle choice
    /// actually traveled**. `/decision` emits its `selection` block whenever
    /// *either* parameter was sent (`selection_requested` is
    /// `audio.is_some() || subtitle.is_some()`, crates/plurxd/src/http/stream.rs),
    /// and the `subtitle_burn_in_blocked_by_hdr` it carries is computed from
    /// the *policy* subtitle against the *source* range. On an audio-only
    /// request that describes a burn the server never even considered applying
    /// — `apply_selected_subtitle` is gated on `subtitle=`. Reading the flag
    /// there would silently drop a forced track the no-choice path starts
    /// happily, e.g. an HDR source tone-mapped to SDR for an SDR-only device
    /// whose policy pick is forced PGS signs: picking only a Japanese audio
    /// track would turn the signs off.
    ///
    /// The client's own predicate reads the *delivered* range, so it answers
    /// for whatever is actually about to play and applies to both arms.
    static func startingSubtitleBlockedByHDR(
        prePlaySubtitle: Int?,
        wanted: Int?,
        serverSelection: DecisionSelection?,
        tracks: [SubtitleTrack],
        deliveredRange: String?
    ) -> Bool {
        let serverRefused = prePlaySubtitle != nil
            && serverSelection?.subtitleBurnInBlockedByHdr == true
        return serverRefused
            || subtitleBurnWouldDiscardHDR(wanted, tracks: tracks, deliveredRange: deliveredRange)
    }

    static func shouldPreserveEstablishedHDRDelivery(
        deliveredRange: String?,
        establishedPlayback: Bool
    ) -> Bool {
        establishedPlayback && isHDRDelivery(deliveredRange)
    }

    /// Has the server ruled out retrying this recipe unchanged?
    ///
    /// Reads the *retained* verdict rather than any one ask's answer, so that
    /// it agrees with the failure sentence `handleItemFailure` shows. Those
    /// two must not disagree: a `terminal` response stops the reporter the
    /// instant it arrives, so a second item failure in the same control
    /// session gets `nil` from a fresh ask while the retained slot — which
    /// deliberately outlives the reporter — still has the verdict. That second
    /// failure is reachable, because a node failover replaces the item without
    /// starting a new control session.
    ///
    /// The transport carve-out is the failure sentence's, for its reason: a
    /// verdict outlives the session that earned it, and a dropped link is a
    /// different cause with a different answer.
    private func unchangedRetryRuledOut(isTransportFailure: Bool) -> Bool {
        guard !isTransportFailure else { return false }
        return playbackControl.terminalVerdict?.type == "terminal"
    }

    /// A stream that has already rendered real HDR did not fail capability
    /// negotiation. Reconnect the same recipe once; if it immediately fails
    /// again, stop visibly instead of hiding the transport fault behind SDR.
    ///
    /// [unchangedRetryRuledOut] skips the reconnect and nothing else. The
    /// server's `is_permanent` is scoped to "retrying this source, *unchanged*"
    /// and that reconnect is the only unchanged retry there is — but the stop
    /// below is this rung's real job, and it is the reason an established HDR
    /// delivery never descends to SDR. Skipping the whole rung would hand the
    /// compatibility ladder a stream it has always been vetoed from, and
    /// tone-map a picture that was rendering real HDR a second ago.
    private func retryEstablishedHDRDelivery(
        at position: Int,
        unchangedRetryRuledOut: Bool = false
    ) async -> Bool {
        guard Self.shouldPreserveEstablishedHDRDelivery(
            deliveredRange: deliveredRange,
            establishedPlayback: attachmentRecovery.establishedPlayback
        ) else { return false }
        guard !establishedHDRRetryAttempted, !unchangedRetryRuledOut else {
            stopForBlockingSurface(revokingPlaybackIntent: true)
            isChangingStream = false
            // A silent stall reaches this rung before the stall funnel's own
            // stop, so leaving it out would hide the verdict on the path most
            // likely to have earned one.
            raiseOwnerFault(
                source: "owner_stopped",
                title: Self.playbackStoppedFailureTitle,
                detail: playbackControl.terminalVerdict?.message
                    ?? "The HDR stream stopped responding. Playback was stopped instead of switching to SDR."
            )
            return true
        }
        establishedHDRRetryAttempted = true
        isChangingStream = false
        await reopen(
            at: position,
            intent: resumeAttempt?.repairAdmitted == true ? .resumeRepair : .normal
        )
        return true
    }

    /// Advance through the least-destructive recovery ladder. A compatible
    /// DV source first becomes HDR10/HLG without a video encode; only a second
    /// failure falls back to the existing universal transcode.
    ///
    /// `source` is the contract row the rung belongs to. The ladder's own
    /// steps are row 13 (`owner_recovery_step`); the readiness deadline that
    /// spends one is row 14 (`readiness_deadline_rungs_left`), which the
    /// contract names for Apple outright. Same class, same pixel, and the
    /// ledger stops calling a readiness timeout an ordinary fallback.
    private func retryWithNextCompatibilityFallback(
        at position: Int,
        source: String = "owner_recovery_step"
    ) async -> Bool {
        switch Self.nextCompatibilityFallback(
            canRetryWithHDRBase: canRetryCurrentItemWithHDRBase,
            hdrBaseAlreadyAttempted: dolbyVisionFallbackAttempted,
            canRetryWithTranscode: canRetryCurrentItemWithTranscode,
            transcodeAlreadyAttempted: compatibilityFallbackAttempted
        ) {
        case .none:
            return false
        case .hdrBase:
            dolbyVisionFallbackAttempted = true
            requireDeliveryFallback(.hdrBase)
            canRetryCurrentItemWithHDRBase = false
            isChangingStream = false
            // A rung the owner is about to spend is a recovery step, not a
            // failure: contract §3.3 row 13 names this sentence outright.
            raiseSurfaceNotice(
                source: source,
                context: surfaceContext,
                detail: "Dolby Vision did not start. Retrying the HDR10-compatible picture…"
            )
        case .transcode:
            compatibilityFallbackAttempted = true
            requireDeliveryFallback(.transcode)
            canRetryCurrentItemWithTranscode = false
            isChangingStream = false
            raiseSurfaceNotice(
                source: source,
                context: surfaceContext,
                detail: "The compatible stream did not start. Retrying a universal stream…"
            )
        }
        await reopen(
            at: position,
            intent: resumeAttempt?.repairAdmitted == true ? .resumeRepair : .normal
        )
        return true
    }

    /// The rung the ladder would choose right now. Read before the ladder runs
    /// so the failure log can name the step the viewer actually got, and so a
    /// client-observed verdict with no rung left changes nothing at all.
    private var plannedCompatibilityFallback: PlaybackCompatibilityFallback {
        Self.nextCompatibilityFallback(
            canRetryWithHDRBase: canRetryCurrentItemWithHDRBase,
            hdrBaseAlreadyAttempted: dolbyVisionFallbackAttempted,
            canRetryWithTranscode: canRetryCurrentItemWithTranscode,
            transcodeAlreadyAttempted: compatibilityFallbackAttempted
        )
    }

    /// A fresh start that never reached `.readyToPlay` is a media verdict, not
    /// a transport fault: AVFoundation neither failed the item nor readied it,
    /// which is what a Dolby Vision Profile 5 track with an empty `hvcC` does
    /// on a device that cannot decode it. Spend one rung instead of stopping.
    /// Returning false leaves the caller's original timeout to surface.
    private func retryAfterReadinessTimeout(at position: Int) async -> Bool {
        let fallback = plannedCompatibilityFallback
        guard fallback != .none else { return false }
        reportCompatibilityLadderFailure(
            message: Self.readinessTimeoutMessage,
            step: PlaybackCompatibilityLadderStep(
                cause: .readinessTimeout,
                fallback: fallback
            )
        )
        // Row 14, and never a prompt: the guard above returned already if no
        // rung was left, so reaching here IS "the deadline fired with rungs
        // left". The class is `recovering` either way — what changes is that
        // the ledger names the deadline instead of borrowing the ladder's row.
        return await retryWithNextCompatibilityFallback(
            at: position,
            source: "readiness_deadline_rungs_left"
        )
    }

    /// Six seconds of film clock with nothing decoded. Nothing was ever
    /// established here — no frame has been presented for this item — so the
    /// established-HDR policy does not apply and the pre-start ladder does.
    ///
    /// With no rung left this used to do nothing at all: audio kept playing
    /// over a black picture with no surface and no explanation, which is the
    /// silent exhaustion §2.2 records. The ladder is the recovery owner here,
    /// so it now does what every other spent owner does — stop the player,
    /// then say so.
    private func handleBlackFrameDecodeFailure(at position: Int) async {
        guard let decoderItem = player.currentItem,
              blackFrameRecoveryIsEligible(for: decoderItem) else { return }
        let decoderAttempt = snapshotAttempt()
        _ = await noteAutoDecodeFailure()
        guard player.currentItem === decoderItem,
              attemptStillCurrent(decoderAttempt, fence: .blackFrameDecoderAcknowledgement),
              blackFrameRecoveryIsEligible(for: decoderItem) else {
            blackFrameWatchdog.cancelRecovery(for: decoderItem)
            return
        }
        let fallback = plannedCompatibilityFallback
        guard fallback != .none else {
            stopForBlockingSurface()
            raiseOwnerFault(
                source: "black_frame_ladder_spent",
                context: .start,
                title: Self.playbackStoppedFailureTitle,
                detail: Self.blackFrameFailureMessage,
                // Left to the source row, which offers Close and Try again:
                // the client and the fixture must not each keep their own copy
                // of a fault's actions.
                positionMs: position
            )
            return
        }
        reportCompatibilityLadderFailure(
            message: Self.blackFrameFailureMessage,
            step: PlaybackCompatibilityLadderStep(
                cause: .blackFrames,
                fallback: fallback
            )
        )
        _ = await retryWithNextCompatibilityFallback(
            at: Self.compatibilityRetryPositionMs(lastObservedMs: position)
        )
    }

    private func report(_ position: Int) {
        guard position > 0 else { return }
        let globalPosition = AudiobookTimeline.globalPosition(
            localPositionMs: position,
            partOffsetMs: progressOffsetMs
        )
        let duration = itemDurationMs ?? (knownDurationMs > 0 ? knownDurationMs : nil)
        #if os(iOS)
        if let offlineId {
            Task {
                await OfflineDownloadManager.shared.recordProgress(
                    id: offlineId,
                    positionMs: globalPosition,
                    durationMs: duration
                )
            }
            return
        }
        #endif
        guard let fileContext, (try? fileContext.localID(expected: fileId)) != nil else { return }
        let itemId = itemId
        let model = model
        let method = clientLogMethod
        Task {
            await model?.reportProgress(itemId: itemId, positionMs: globalPosition,
                                        durationMs: duration, method: method)
        }
    }

    /// Bind evidence to this attachment's lifecycle/open/session, including
    /// poll responses which arrived before the initial item was attached.
    func readinessContext(for item: AVPlayerItem) -> PlayerItemReadiness.Context {
        let generation = openGeneration
        let lifecycle = lifecycleGeneration
        let expectedSession = sessionId
        return PlayerItemReadiness.Context(
            growingSessionID: isVOD ? nil : expectedSession,
            ownsItem: { [weak self, weak item] in
                guard let self, let item else { return false }
                return self.isCurrentLifecycle(lifecycle) && !self.isSuperseded(generation)
                    && self.player.currentItem === item && self.sessionId == expectedSession
            },
            sample: { [weak self] in self?.preparationSample }
        )
    }

    /// Subscribe before inspecting so no native status transition is lost.
    private static func awaitItemReady(_ item: AVPlayerItem, context: PlayerItemReadiness.Context) async throws {
        let observer = AVPlayerItemObserver(item: item)
        defer { observer.cancel() }
        try await PlayerItemReadiness.wait(
            events: observer.events, status: { item.status }, failure: { item.error }, context: context
        )
    }

    func seekPreparationOwner(at targetMs: Int) -> SeekPreparationOwner {
        SeekPreparationOwner(actionEpoch: viewerActionEpoch, seekGeneration: seekState.generation, targetMs: targetMs)
    }

    func seekWhenReady(_ item: AVPlayerItem, ms: Int, owner: SeekPreparationOwner) async throws {
        let generation = openGeneration
        let lifecycle = lifecycleGeneration
        let ownsSeek = { [weak self] in
            guard let self else { return false }
            return self.isCurrentLifecycle(lifecycle) && !self.isSuperseded(generation)
                && self.player.currentItem === item
                // Pause changes transport authority, not the resume point.
                // A newer same-position selection also still needs this seek.
                && (self.viewerActionEpoch == owner.actionEpoch
                    || self.seekState.generation == owner.seekGeneration
                    || self.seekState.pendingMs == owner.targetMs)
                && !Task.isCancelled
        }
        guard ownsSeek() else { return }
        do { try await itemPreparation.ready(item, readinessContext(for: item)) }
        catch {
            guard ownsSeek() else { return }
            throw error
        }
        // The check must precede the mutation, not merely guard the caller's
        // eventual completion: `player` may now contain an unrelated item.
        guard ownsSeek() else { return }
        let outcome = await PlayerSeekCompletion.run(
            operation: { [player, prepare = itemPreparation.seek] in
                // A native interruption need not replace the stream. Retry
                // once while this same destination still owns the item; both
                // attempts share the existing preparation deadline.
                for _ in 0..<2 {
                    guard ownsSeek() else { return false }
                    if await prepare(player, ms) { return true }
                }
                return false
            },
            waitForDeadline: waitNativeSeekDeadline
        )
        guard ownsSeek() else { return }
        switch outcome {
        case .finished(true), .cancelled:
            return
        case .finished(false), .timedOut:
            if outcome == .timedOut { item.cancelPendingSeeks() }
            // The item's clock may still be at zero. Keep the requested film
            // position for Retry and progress reporting, and stop before the
            // caller surfaces the preparation failure.
            let destination = seekState.absolute(owner.targetMs, durationMs: knownDurationMs)
            currentMs = destination.target
            stopForBlockingSurface()
            throw outcome == .timedOut ? PlaybackPreparationError.timedOut : PlaybackPreparationError.failed
        }
    }

    /// Position in the HLS master rendition order. The server advertises only
    /// native tracks and preserves source order, so this remains stable even
    /// when bitmap and styled tracks are interleaved in the menu.
    static func nativeSubtitleOrdinal(_ index: Int, in tracks: [SubtitleTrack]) -> Int? {
        tracks.filter(\.isNativeHLS).firstIndex(where: { $0.index == index })
    }

    static func subtitleRequiresBurn(_ index: Int, in tracks: [SubtitleTrack]) -> Bool {
        tracks.first(where: { $0.index == index }).map {
            !$0.isNativeHLS && !$0.isPGSOverlay
        } ?? true
    }

    static func subtitleUsesOverlay(_ index: Int?, in tracks: [SubtitleTrack]) -> Bool {
        guard let index else { return false }
        return tracks.first(where: { $0.index == index })?.isPGSOverlay == true
    }

    /// Pure media-selection step used by the AVPlayer adapter and XCTest. A
    /// successful result means the video item stays in place; the closure gets
    /// nil for Off or the native rendition ordinal for a selected track.
    @discardableResult
    static func applyNativeSubtitleSelection(
        _ index: Int?,
        tracks: [SubtitleTrack],
        select: (Int?) -> Void
    ) -> Bool {
        guard let index else {
            select(nil)
            return true
        }
        guard let ordinal = nativeSubtitleOrdinal(index, in: tracks) else { return false }
        select(ordinal)
        return true
    }

    /// Which position in the legible group's subtitle options carries native
    /// rendition `ordinal`. Positional mapping is a bet with no contractual
    /// basis: AVFoundation may merge same-`NAME` entries and may synthesize a
    /// closed-caption option, and either event shifts every ordinal. Match the
    /// `LANGUAGE`/`NAME` pair the server actually authored first, and keep the
    /// ordinal only as the last resort (P1-1).
    static func nativeSubtitleOptionIndex(
        ordinal: Int,
        tracks: [SubtitleTrack],
        options: [SubtitleRenditionOption]
    ) -> Int? {
        let natives = tracks.filter(\.isNativeHLS)
        guard natives.indices.contains(ordinal) else { return nil }
        let track = natives[ordinal]
        let names = subtitleRenditionNames(tracks)
        guard names.indices.contains(ordinal) else { return nil }
        let name = quotedAttributeValue(names[ordinal])
        let tag = languageCode(subtitleLanguageTag(track.language))
        if let match = options.firstIndex(where: {
            $0.displayName == name && languageCode($0.languageTag) == tag
        }) {
            return match
        }
        if let match = options.firstIndex(where: { $0.displayName == name }) { return match }
        let sameLanguage = options.indices.filter { languageCode(options[$0].languageTag) == tag }
        if sameLanguage.count == 1 { return sameLanguage[0] }
        return options.indices.contains(ordinal) ? ordinal : nil
    }

    /// Every rendition `NAME` the master advertises, in master order — the
    /// replica of `unique_subtitle_names` in crates/plurxd/src/http/hls.rs.
    ///
    /// RFC 8216 makes NAME unique within a group, so the server disambiguates
    /// repeats by occurrence ("English", "English (2)"). Replicating only the
    /// base name would make two untitled same-language tracks collide and the
    /// second one resolve onto the first — worse than the ordinal guess this
    /// matching replaced.
    static func subtitleRenditionNames(_ tracks: [SubtitleTrack]) -> [String] {
        var seen: [String: Int] = [:]
        var names: [String] = []
        for (position, track) in tracks.enumerated() where track.isNativeHLS {
            let base = subtitleRenditionName(track, position: position)
            let count = (seen[base] ?? 0) + 1
            seen[base] = count
            names.append(count == 1 ? base : "\(base) (\(count))")
        }
        return names
    }

    /// Replica of the server's `quoted`: `NAME` is an HLS quoted-string, so
    /// characters that cannot appear in one are rewritten before emission and
    /// a title containing them reaches AVFoundation already rewritten.
    static func quotedAttributeValue(_ value: String) -> String {
        String(value.map { character -> Character in
            switch character {
            case "\"": return "'"
            case "\r", "\n": return " "
            default: return character
            }
        })
    }

    /// Replica of the server's `subtitle_name` (crates/plurxd/src/http/hls.rs)
    /// — the base `NAME` for the track at `position` in the decision's
    /// subtitle list, before de-duplication. The separator is U+00B7 MIDDLE
    /// DOT with one space on either side, exactly as the server emits it.
    static func subtitleRenditionName(_ track: SubtitleTrack, position: Int) -> String {
        let language = subtitleLanguageName(track.language)
        let title = track.title?.trimmingCharacters(in: .whitespacesAndNewlines)
        if let title, !title.isEmpty {
            return language == "und" ? title : "\(language) · \(title)"
        }
        return language == "und" ? "Subtitle \(position + 1)" : language
    }

    /// Replica of the server's `language_tag`, which is one line delegating to
    /// `plurx_core::tracks::bcp47_tag`: absent or blank is "und", any spelling
    /// the alias table knows becomes its two-letter member (taken by length,
    /// because the groups are ordered by settings canonicality), and an
    /// unknown code passes through unchanged.
    static func subtitleLanguageTag(_ raw: String?) -> String {
        let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        guard !trimmed.isEmpty else { return "und" }
        let lower = trimmed.lowercased()
        return languageAliases.first(where: { $0.contains(lower) })?
            .first(where: { $0.count == 2 }) ?? trimmed
    }

    /// Replica of the server's `language_name`: the display half of `NAME`,
    /// kept in step with the alias table so a language that matches never
    /// renders as a bare three-letter code.
    static func subtitleLanguageName(_ raw: String?) -> String {
        switch subtitleLanguageTag(raw) {
        case "en": return "English"
        case "it": return "Italian"
        case "ja": return "Japanese"
        case "es": return "Spanish"
        case "fr": return "French"
        case "de": return "German"
        case "pt": return "Portuguese"
        case "ko": return "Korean"
        case "zh": return "Chinese"
        case "ru": return "Russian"
        case "hi": return "Hindi"
        case "ar": return "Arabic"
        case "nl": return "Dutch"
        case "sv": return "Swedish"
        case "pl": return "Polish"
        case "no": return "Norwegian"
        case "da": return "Danish"
        case "fi": return "Finnish"
        case "tr": return "Turkish"
        case "th": return "Thai"
        case "vi": return "Vietnamese"
        case "uk": return "Ukrainian"
        case "cs": return "Czech"
        case "el": return "Greek"
        case "he": return "Hebrew"
        case "hu": return "Hungarian"
        case "ro": return "Romanian"
        case let other: return other
        }
    }


    /// Resolve player-local time zero onto the source timeline.
    ///
    /// Copy sessions can start at the keyframe before the requested seek, so
    /// the server's integer origin wins. The older floating-point request
    /// echo remains the compatibility fallback. Cached whole-title sessions
    /// use direct timeline semantics and always begin at zero.
    nonisolated static func sessionMediaOriginMs(_ hls: HlsStart, requestedStartMs: Int) -> Int {
        sessionMediaOriginMs(
            vod: hls.vod == true,
            mediaOriginMs: hls.mediaOriginMs,
            startSeconds: hls.startSeconds,
            requestedStartMs: requestedStartMs
        )
    }

    /// The same rule over a raw origin rather than an `HlsStart`.
    ///
    /// A prepared successor arrives as four fields on a control action, not as
    /// a create response, and it still has to be aligned by exactly the
    /// arithmetic a created session is. Generalising rather than writing a
    /// second copy is the whole point: two alignment implementations that
    /// disagree produce a handoff that lands a few hundred milliseconds off
    /// and looks like a stutter nobody can attribute.
    nonisolated static func sessionMediaOriginMs(
        vod: Bool,
        mediaOriginMs: Int?,
        startSeconds: Double?,
        requestedStartMs: Int
    ) -> Int {
        if vod { return 0 }
        if let mediaOriginMs { return max(0, mediaOriginMs) }
        return Int((startSeconds ?? Double(requestedStartMs) / 1000.0) * 1000)
    }

    /// Item-local seek needed after a live copy session opens on a keyframe
    /// before the requested film position. Tiny timestamp rounding differences
    /// stay at zero; only a real keyframe lead-in pays for an exact seek.
    nonisolated static func sessionAttachSeekMs(
        requestedStartMs: Int,
        mediaOriginMs: Int,
        minimumCorrectionMs: Int = 100
    ) -> Int? {
        let correction = max(0, requestedStartMs) - max(0, mediaOriginMs)
        return correction >= minimumCorrectionMs ? correction : nil
    }

    @discardableResult
    private func applyNativeSubtitleSelection(
        _ index: Int?,
        to item: AVPlayerItem?,
        expectedActionEpoch: Int
    ) async -> Bool {
        guard let item, started, player.currentItem === item,
              expectedActionEpoch == viewerActionEpoch else { return false }
        let selection = await mediaSelectionPreparation.native(index, subtitles, item)
        guard !Task.isCancelled, started,
              player.currentItem === item,
              Self.subtitleMutationIsCurrent(
                expectedActionEpoch: expectedActionEpoch,
                currentActionEpoch: viewerActionEpoch
              )
        else { return false }
        if selection.apply() { return true }
        // Off never fails: without a legible group nothing is being rendered.
        guard let index else { return true }
        // "Has a legible group" is not "advertises subtitle renditions".
        // AVFoundation may synthesise a closed-caption option into `.legible`
        // for a variant that carries no CLOSED-CAPTIONS attribute, so a legacy
        // master can hand back a non-empty group with no subtitles in it.
        return await recoverFromFailedNativeSelection(
            index,
            item: item,
            hasSubtitleOptions: selection.hasSubtitleOptions
        )
    }

    /// False when the selection could not be resolved onto a real option.
    static func selectNativeSubtitle(
        _ index: Int?,
        tracks: [SubtitleTrack],
        in group: AVMediaSelectionGroup,
        of item: AVPlayerItem
    ) -> Bool {
        // Only subtitle options are ours: AVFoundation can also expose a
        // closed-caption option that no rendition in the master authored.
        let options = group.options.filter { $0.mediaType == .subtitle }
        let descriptors = options.map {
            SubtitleRenditionOption(
                languageTag: $0.extendedLanguageTag,
                displayName: $0.displayName
            )
        }
        var resolved = false
        let eligible = Self.applyNativeSubtitleSelection(index, tracks: tracks) { ordinal in
            guard let ordinal else {
                item.select(nil, in: group)
                resolved = true
                return
            }
            guard let position = Self.nativeSubtitleOptionIndex(
                ordinal: ordinal,
                tracks: tracks,
                options: descriptors
            ), options.indices.contains(position) else { return }
            item.select(options[position], in: group)
            resolved = true
        }
        return eligible && resolved
    }

    /// P1-3: the selection did not land. Never leave state claiming a track
    /// the player is not rendering — either burn it (legacy servers only) or
    /// say so and drop the checkmark.
    private func recoverFromFailedNativeSelection(
        _ index: Int,
        item: AVPlayerItem,
        hasSubtitleOptions: Bool
    ) async -> Bool {
        // The item itself failing is the status observer's story, not ours.
        guard item.status != .failed, selectedSubtitle == index else { return false }
        // The legacy-server fallback used to stand here: a failed native
        // selection on a server that advertised no renditions was reopened as
        // a burn. It is gone, and the evidence it rested on with it.
        //
        // The evidence was "this create response carried no `native=` query
        // and this asset advertised no subtitle group". There are no
        // pre-native servers left in the fleet, so that combination can no
        // longer mean "old server" — but it can still occur, on a master this
        // build failed to parse or an asset that had not loaded its groups
        // yet. When it did, a text track the viewer asked for was answered
        // with a full SDR re-encode nobody requested: exactly the outcome this
        // whole arc exists to stop, reached by a path meant to be a kindness.
        //
        // A selection that fails is now simply a selection that failed, and
        // the viewer is told so.
        _ = hasSubtitleOptions
        selectedSubtitle = nil
        showPlaybackNotice("That subtitle track could not be turned on.")
        // The selection resolved to Off without changing the item. The
        // caller still owes any pending media destination to the viewer.
        return true
    }

    /// The audible half of owning media selection (P2-8). An HLS session
    /// carries one muxed audio track — the server already chose it, and
    /// `selectAudio` reopens to change it — but a direct-play file can carry
    /// several, and with automatic criteria off nothing else would honor the
    /// viewer's language there. Best effort: no match leaves AVPlayer's own
    /// default in place, which is what mismatched criteria used to produce.
    private func applyPreferredAudioSelection(to item: AVPlayerItem, expectedActionEpoch: Int) async {
        guard audioOverride == nil, prePlaySelection.audioIndex == nil,
              started, player.currentItem === item,
              expectedActionEpoch == viewerActionEpoch else { return }
        let apply = await mediaSelectionPreparation.audio(audioLanguage, item)
        guard !Task.isCancelled, started, player.currentItem === item, audioOverride == nil,
              prePlaySelection.audioIndex == nil,
              expectedActionEpoch == viewerActionEpoch else { return }
        apply?()
    }

    /// Open, failover, and offline attachment share the same selection owner.
    /// Capture the choice and epoch together, never an old choice with a new
    /// epoch. An intervening command requires a fresh preparation; item
    /// identity alone cannot fence two operations against the same item.
    func reconcileNativeMediaSelections(to item: AVPlayerItem) async {
        while !Task.isCancelled, started, player.currentItem === item {
            let actionEpoch = viewerActionEpoch
            let fields = Self.sessionSubtitleFields(
                selected: selectedSubtitle, tracks: subtitles
            )
            let nativeSubtitle = pgsOverlayIsActive || activeBurnedSubtitle != nil ? nil : fields.native
            await applyPreferredAudioSelection(to: item, expectedActionEpoch: actionEpoch)
            guard !Task.isCancelled, started, player.currentItem === item else { return }
            if actionEpoch != viewerActionEpoch { continue }
            await applyNativeSubtitleSelection(nativeSubtitle, to: item, expectedActionEpoch: actionEpoch)
            guard !Task.isCancelled, started, player.currentItem === item else { return }
            if actionEpoch == viewerActionEpoch { return }
        }
    }

    #if os(iOS)
    private func titleMetadata(_ title: String) -> AVMetadataItem {
        let metadata = AVMutableMetadataItem()
        metadata.identifier = .commonIdentifierTitle
        metadata.value = title as NSString
        metadata.extendedLanguageTag = "und"
        return metadata
    }
    #endif

    /// The audio index an HLS session must carry, in falling order of
    /// authority: `explicit` is a later in-player choice, `plan` is what the
    /// server put in the delivery plan for a selection-aware `/decision`, and
    /// `selected` is the server's automatic answer.
    ///
    /// `plan` outranks `selected` because the contract says to execute the plan
    /// as given (docs/CLIENTS.md §1); it sits *below* `explicit` because the
    /// plan was decided before the viewer changed their mind mid-playback, and
    /// a reopen replays that stale plan otherwise.
    nonisolated static func needsOrdinalBoundAudioSession(audioTrackCount: Int, explicitAudio: Int?) -> Bool {
        explicitAudio != nil || audioTrackCount > 1
    }

    nonisolated static func sessionAudioIndex(
        explicit: Int?,
        plan: Int?,
        selected: Int?
    ) -> Int? {
        explicit ?? plan ?? selected
    }

    /// The subtitle a starting playback shows.
    ///
    /// A pre-play choice is the viewer speaking, so it outranks both this
    /// device's "subtitles off" setting and the never-start-a-burn veto that
    /// governs *automatic* selection — picking a bitmap track on the detail
    /// screen is exactly the viewer asking for it. Off (`-1`) is a choice too,
    /// and the server echoes it as no effective subtitle.
    ///
    /// With no pre-play choice this is unchanged: the device's Off setting is
    /// honored first, then the server's own pick via `automaticSubtitleIndex`.
    static func initialSubtitleIndex(
        prePlay: Int?,
        serverSelection: DecisionSelection?,
        tracks: [SubtitleTrack],
        deviceSubtitlesOff: Bool
    ) -> Int? {
        if let prePlay {
            // The echo is authoritative when there is one — `subtitle_index`
            // null then means Off. A server predating the selection contract
            // sends no `selection` block at all, so fall back to what was asked
            // for, minus the Off sentinel.
            guard let serverSelection else {
                return prePlay == PrePlaySelection.subtitleOff ? nil : prePlay
            }
            return serverSelection.subtitleIndex
        }
        // "Off" in this device's settings is the viewer saying never, so it is
        // honored before the server's pick is even looked at. It is the
        // viewer's instruction rather than a second copy of the selection rule,
        // which lives on the server alone.
        return deviceSubtitlesOff ? nil : automaticSubtitleIndex(tracks)
    }

    /// Every spelling of a language preference AVFoundation might have to
    /// match, BCP-47 tag first. Derived from the shared alias table rather
    /// than a third private copy of it — the twelve-language copy this
    /// replaced could not match a Dutch, Czech, Greek, or Romanian asset.
    static func languageSpellings(_ code: String) -> [String] {
        let lower = code.lowercased()
        guard let group = languageAliases.first(where: { $0.contains(lower) }) else {
            return [code]
        }
        let tag = group.first(where: { $0.count == 2 }) ?? lower
        return [tag] + group.filter { $0 != tag }
    }

    /// The subtitle the player may enable automatically: **the one the server
    /// already chose**, subject to one veto.
    ///
    /// Choosing is the server's job (decided by Paul on 2026-08-03).
    /// `/decision` runs the shared `select_tracks` — anime dual-audio, the
    /// language preferences, and the subtitle mode Auto/Always/Off — and then
    /// stamps `default: true` on exactly the track it picked
    /// (crates/plurxd/src/http/stream.rs, decision handler). `default` on a
    /// decision track is therefore the server's answer and not the muxer's
    /// flag. This client used to re-derive the same rule from its own
    /// language settings, which is how it came to behave as `Always` against a
    /// server set to `Auto`: the rule lived in three languages and drifted.
    ///
    /// What stays here is a veto, not a policy — a statement about what this
    /// client can do rather than about which subtitle the viewer wants:
    /// **automatic selection must never start a burn, except for a forced
    /// track, which may, always at source height** (owner policy, plan §3.3).
    /// A server that picks a non-forced PGS/VobSub/ASS track gets nothing
    /// selected rather than a video encoder on every play (P0-1); the viewer
    /// can still choose it by hand, which is the viewer asking.
    static func automaticSubtitleIndex(_ tracks: [SubtitleTrack]) -> Int? {
        guard let picked = tracks.first(where: { $0.default }) else { return nil }
        guard picked.isNativeHLS || isForcedSubtitle(picked) else { return nil }
        return picked.index
    }

    /// Both signals are meaningful: file 5615's English forced track carries
    /// `forced=false` and only the title says so.
    static func isForcedSubtitle(_ track: SubtitleTrack) -> Bool {
        track.forced || (track.title.map { titleMarksForced($0) } ?? false)
    }

    /// Replica of the server's `title_marks_forced` (crates/plurxd/src/http/
    /// hls.rs). A substring test is too eager: "Non-Forced" and "Unforced" are
    /// real titles, and here the forced arm is the *only* path by which
    /// automatic selection may start a burn, so an over-eager test burns an
    /// ordinary PGS track on every play. Match "forced" on word boundaries and
    /// reject it when the preceding word negates it; "Unforced" and
    /// "Reinforced" fall out for free because the letter in front of them is
    /// not a boundary. Plain "Forced" keeps working — that is 5615's contract.
    nonisolated static func titleMarksForced(_ title: String) -> Bool {
        let lower = title.lowercased()
        var searchStart = lower.startIndex
        while let found = lower.range(of: "forced", range: searchStart..<lower.endIndex) {
            let boundedBefore = found.lowerBound == lower.startIndex
                || !isAlphanumeric(lower[lower.index(before: found.lowerBound)])
            let boundedAfter = found.upperBound == lower.endIndex
                || !isAlphanumeric(lower[found.upperBound])
            if boundedBefore, boundedAfter,
               !negatedBefore(String(lower[lower.startIndex..<found.lowerBound])) {
                return true
            }
            searchStart = found.upperBound
        }
        return false
    }

    /// Replica of the server's `negated_before`: the word immediately in front
    /// of a "forced" occurrence, when it turns the claim around. Separators are
    /// skipped, so "non-forced", "non forced", and "not forced" share a rule.
    nonisolated private static func negatedBefore(_ prefix: String) -> Bool {
        let word = prefix
            .split(whereSeparator: { !isAlphanumeric($0) })
            .last
            .map(String.init) ?? ""
        return ["non", "not", "no", "never"].contains(word)
    }

    nonisolated private static func isAlphanumeric(_ character: Character) -> Bool {
        character.isLetter || character.isNumber
    }

    /// A subtitle burn on a file the device could otherwise take keeps source
    /// resolution — that is the carve-out which lets an automatically selected
    /// forced bitmap track burn without also downgrading the picture. A
    /// genuine transcode still lets server Auto choose its rung unless the
    /// viewer selected one explicitly.
    static func burnSessionHeight(
        burnSubtitle: Int?,
        mode: String,
        selectedHeight: Int?,
        sourceHeight: Int?
    ) -> Int? {
        if let selectedHeight { return selectedHeight }
        guard burnSubtitle != nil, mode != "transcode" else { return nil }
        return sourceHeight
    }

    /// The `selectedHeight` a title starts on for a standing Quality
    /// preference, given the ladder `/decision` advertised for this source.
    ///
    /// Auto and Original name no rung: Original is a `force=original` verdict
    /// (never re-encode), and a rung here would turn it into the transcode
    /// it exists to refuse. A rung is snapped to the tallest advertised one
    /// that does not exceed it — the ladder never contains an upscale, so a
    /// 1080p title under "4K" starts at 1080p — and a source with no ladder
    /// at all falls back to the server's Auto rung.
    nonisolated static func startingHeight(
        for quality: PlaybackQuality,
        ladder: [QualityRung]
    ) -> Int? {
        guard let wanted = quality.rungHeight else { return nil }
        return ladder.map(\.height).filter { $0 <= wanted }.max()
    }

    /// The `subtitle_burn` / `subtitle` pair one selection puts on the wire.
    /// A native text track never becomes a burn — including on the
    /// compatibility-transcode retry, which keeps the selection it had — and
    /// the two fields are never sent together.
    static func sessionSubtitleFields(
        selected: Int?,
        tracks: [SubtitleTrack]
    ) -> (burn: Int?, native: Int?) {
        guard let selected else { return (nil, nil) }
        if subtitleUsesOverlay(selected, in: tracks) { return (nil, nil) }
        if subtitleRequiresBurn(selected, in: tracks) {
            return (selected, nil)
        }
        return (nil, nativeSubtitleOrdinal(selected, in: tracks) == nil ? nil : selected)
    }

    /// Whether a reopen comes back playing. The first attach has no item yet
    /// and always starts; otherwise the viewer's own intent decides, which is
    /// deliberately not the player's observed state — a buffering player
    /// reports `.waitingToPlay` and a failed item has already dropped its rate
    /// to 0, so both would read as "paused" during exactly the reopens P2-5
    /// and P2-6 are about.
    static func reopenResumesPlayback(wantsPlayback: Bool, hasCurrentItem: Bool) -> Bool {
        !hasCurrentItem || wantsPlayback
    }

    /// The film position a compatibility retry resumes from. The failed item's
    /// own clock reads 0 or invalid, so the last position the periodic
    /// observer saw is the only truthful one (P2-6).
    static func compatibilityRetryPositionMs(lastObservedMs: Int) -> Int {
        max(lastObservedMs, 0)
    }

    /// Whether an item failure is parked for resume instead of surfaced.
    ///
    /// Only for a paused viewer on a rolling session — the one kind the
    /// server retires on a pause timer. VOD and direct play are kept alive by
    /// a paused client and keep their ordinary ladder and surface. A
    /// compatibility rejection is still a verdict about the media; an explicit
    /// resume already in flight owns its own repair.
    ///
    /// Not gated on established playback: a replacement opened while paused
    /// (a paused seek) never establishes, and it is retired the same way.
    nonisolated static func parksPausedItemFailure(
        started: Bool,
        wantsPlayback: Bool,
        isRollingSession: Bool,
        isCompatibilityFailure: Bool,
        resumeInFlight: Bool
    ) -> Bool {
        started && !wantsPlayback && isRollingSession
            && !isCompatibilityFailure && !resumeInFlight
    }

    /// The control reporter's rendering of the server's pause-grace refusal
    /// (`http/hls/control.rs`, `410 pause_grace_expired`).
    nonisolated static func isPauseGraceExpiry(_ failure: String) -> Bool {
        failure == "transport:410:pause_grace_expired"
    }

    /// Where the replacement for a presentation retired during a pause opens.
    /// A seek the viewer made while paused wins; then the position saved when
    /// the item died; then the still-attached item's own clock; then the last
    /// observed position.
    nonisolated static func pausedRetirementReopenPositionMs(
        pendingSeekMs: Int?,
        retired: PausedRetirement,
        attachedPositionMs: Int?,
        lastObservedMs: Int
    ) -> Int {
        if let pendingSeekMs { return max(pendingSeekMs, 0) }
        if let saved = retired.positionMs { return max(saved, 0) }
        if let attachedPositionMs, attachedPositionMs > 0 { return attachedPositionMs }
        return max(lastObservedMs, 0)
    }

    /// Whether this open has to go through an HLS session for no reason other
    /// than making the file's text subtitles selectable. The whole of the
    /// `SubtitleReadiness` setting is this function; nothing else branches on
    /// it.
    ///
    /// `.onDemand` is the default (Paul, 2026-08-02, plan §2.5): a file with
    /// native text tracks direct-plays until one is actually asked for, so a
    /// play that never touches subtitles costs the server nothing and keeps
    /// true direct play — and the first selection pays for exactly one clean
    /// restart, the same one a burn already performs, at the same film
    /// position. `.instant` answers yes for any file carrying a native text
    /// track, which is the v0.2 behaviour: every track exists as a rendition
    /// before the menu is ever opened, so switching one is free.
    ///
    /// `subtitlesInUse` is sticky for the title, so turning subtitles back off
    /// does not drop the stream to direct play and charge the next selection a
    /// second restart.
    ///
    /// A file with no native text track answers no under either setting: there
    /// is nothing a session could publish. Bitmap and styled tracks are not
    /// native, so they cannot drag a direct-playable file into a session it
    /// gains nothing from.
    static func needsNativeSubtitleSession(
        hasNativeTextTrack: Bool,
        readiness: SubtitleReadiness,
        subtitlesInUse: Bool
    ) -> Bool {
        guard hasNativeTextTrack else { return false }
        switch readiness {
        case .instant: return true
        case .onDemand: return subtitlesInUse
        }
    }

    /// What one subtitle selection costs. A burn — or leaving one — replaces
    /// the video frames, and P2-7's direct-play → session boundary needs the
    /// session to exist before a native rendition can be selected at all.
    static func subtitleSelectionRoute(
        for index: Int?,
        tracks: [SubtitleTrack],
        activeBurn: Int?,
        isDirectPlayback: Bool,
        activeOverlay: Int? = nil
    ) -> SubtitleSelectionRoute {
        let needsBurn = index.map { subtitleRequiresBurn($0, in: tracks) } ?? false
        let targetUsesOverlay = subtitleUsesOverlay(index, in: tracks)
        let leavesDirectPlay = isDirectPlayback && index != nil && !targetUsesOverlay
        if needsBurn || activeBurn != nil || leavesDirectPlay { return .reopen }
        if targetUsesOverlay || (activeOverlay != nil && index == nil) { return .bitmapOverlay }
        return .mediaSelection
    }

    /// Rechecked after AVAsset's asynchronous legible-group load and before
    /// touching the item. A delayed A completion must not overwrite B.
    nonisolated static func subtitleMutationIsCurrent(
        expectedActionEpoch: Int,
        currentActionEpoch: Int
    ) -> Bool {
        expectedActionEpoch == currentActionEpoch
    }

    /// P1-2: what an `open()` still owes the viewer when it completes. The
    /// selection it applied was captured before its awaits, so a track chosen
    /// during a cold extraction has to be applied afterwards; nil means the
    /// stream already matches the selection the UI is showing.
    static func subtitleReconciliation(
        applied: Int?,
        current: Int?,
        tracks: [SubtitleTrack],
        activeBurn: Int?,
        isDirectPlayback: Bool,
        activeOverlay: Int? = nil
    ) -> SubtitleSelectionRoute? {
        guard applied != current else { return nil }
        return subtitleSelectionRoute(
            for: current,
            tracks: tracks,
            activeBurn: activeBurn,
            isDirectPlayback: isDirectPlayback,
            activeOverlay: activeOverlay
        )
    }

    /// ISO 639-1 / 639-2/B / 639-2/T spellings that mean the same language,
    /// mirrored group for group from `LANG_ALIASES` in
    /// crates/plurx-core/src/tracks.rs. The smaller copy this replaced knew
    /// twelve languages, so "dut"/"cze"/"gre"/"rum" never matched a viewer's
    /// "nl"/"cs"/"el"/"ro" — the same divergence plan §2.5's P2-2 filed
    /// against the server's own duplicate table.
    private static let languageAliases: [[String]] = [
        ["eng", "en"],
        ["jpn", "ja", "jp"],
        ["spa", "es"],
        ["fre", "fra", "fr"],
        ["ger", "deu", "de"],
        ["ita", "it"],
        ["por", "pt"],
        ["rus", "ru"],
        ["kor", "ko"],
        ["chi", "zho", "zh"],
        ["hin", "hi"],
        ["ara", "ar"],
        ["nld", "dut", "nl"],
        ["swe", "sv"],
        ["pol", "pl"],
        ["nor", "nob", "no"],
        ["dan", "da"],
        ["fin", "fi"],
        ["tur", "tr"],
        ["tha", "th"],
        ["vie", "vi"],
        ["ukr", "uk"],
        ["ces", "cze", "cs"],
        ["ell", "gre", "el"],
        ["heb", "he"],
        ["hun", "hu"],
        ["ron", "rum", "ro"],
    ]

    /// Collapse the spellings used by settings, ffprobe, and AVFoundation's
    /// `extendedLanguageTag` into the same comparison key. Region subtags are
    /// dropped ("en-US" is English) before the alias lookup.
    private static func languageCode(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let code = raw
            .lowercased()
            .replacingOccurrences(of: "_", with: "-")
            .split(separator: "-")
            .first
            .map(String.init) ?? ""
        guard !code.isEmpty else { return nil }
        return languageAliases.first(where: { $0.contains(code) })?
            .first(where: { $0.count == 2 }) ?? code
    }

    private nonisolated static func legacyMode(_ method: String) -> String {
        switch method {
        case "direct_play": return "direct"
        case "remux": return "remux"
        default: return "transcode"
        }
    }

    /// AVPlayer can accept a supported Dolby Vision Profile 5/8 decoder
    /// configuration from a progressive MP4, advance its clock, and still
    /// render a black video plane. Normalize every server-approved direct DV
    /// stream through copy-video HLS. This also protects clients talking to a
    /// server that predates the `dvhls=1` capability and still answers direct.
    nonisolated static func playbackMode(_ decision: Decision) -> String {
        let mode = decision.delivery?.mode ?? legacyMode(decision.method)
        guard mode == "direct", decision.source?.hdr?.lowercased() == "dolby_vision" else {
            return mode
        }
        return "remux"
    }

    /// Prefer the server's explicit preservation verdict. A legacy direct-DV
    /// answer is also safe to preserve: the server only returns direct after
    /// matching the exact profiles this client advertises (5 and 8).
    nonisolated static func shouldPreserveDolbyVision(_ decision: Decision) -> Bool {
        if let preserve = decision.delivery?.preserveDolbyVision
            ?? decision.preserveDolbyVision {
            return preserve
        }
        let serverMode = decision.delivery?.mode ?? legacyMode(decision.method)
        return serverMode == "direct"
            && decision.source?.hdr?.lowercased() == "dolby_vision"
    }

    /// Set by the player view while it is on screen. The lock screen and the
    /// control centre are inputs like any other: without this they were a
    /// second answer to "what does skip do", and they answered it during a
    /// pending scrub, where the touch table says `ignore`.
    #if os(iOS)
    var remoteInput: (@MainActor (PlayerContractInput) -> Bool)?
    #endif

    private func routeRemote(_ input: PlayerContractInput, otherwise fallback: () -> Void) {
        #if os(iOS)
        if let remoteInput {
            _ = remoteInput(input)
        } else {
            fallback()
        }
        #else
        fallback()
        #endif
    }

    private func installRemoteCommands() {
        RemoteCommandOwner.shared.claim(remoteOwnerToken) { [weak self] in
            self?.lastNowPlayingState = nil
            self?.updateNowPlaying()
        }
        guard remoteTargets.isEmpty else { return }
        let commands = MPRemoteCommandCenter.shared()
        commands.playCommand.isEnabled = true
        commands.pauseCommand.isEnabled = true
        commands.togglePlayPauseCommand.isEnabled = true
        commands.skipBackwardCommand.isEnabled = true
        commands.skipForwardCommand.isEnabled = true
        commands.changePlaybackPositionCommand.isEnabled = true
        commands.skipBackwardCommand.preferredIntervals = [10]
        commands.skipForwardCommand.preferredIntervals = [10]

        remoteTargets.append((commands.playCommand, commands.playCommand.addTarget { [weak self] _ in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            Task { @MainActor in
                self.setPlaybackRequested(true)
            }
            return .success
        }))
        remoteTargets.append((commands.pauseCommand, commands.pauseCommand.addTarget { [weak self] _ in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            Task { @MainActor in
                self.setPlaybackRequested(false)
            }
            return .success
        }))
        remoteTargets.append((commands.togglePlayPauseCommand,
                              commands.togglePlayPauseCommand.addTarget { [weak self] _ in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            Task { @MainActor in
                self.routeRemote(.playPause) { self.togglePlayPause() }
            }
            return .success
        }))
        remoteTargets.append((commands.skipBackwardCommand,
                              commands.skipBackwardCommand.addTarget { [weak self] _ in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            Task { @MainActor in
                self.routeRemote(.skipBack) { self.skip(seconds: -10) }
            }
            return .success
        }))
        remoteTargets.append((commands.skipForwardCommand,
                              commands.skipForwardCommand.addTarget { [weak self] _ in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            Task { @MainActor in
                self.routeRemote(.skipForward) { self.skip(seconds: 10) }
            }
            return .success
        }))
        remoteTargets.append((commands.changePlaybackPositionCommand,
                              commands.changePlaybackPositionCommand.addTarget { [weak self] event in
            guard let self, RemoteCommandOwner.shared.isCurrent(self.remoteOwnerToken) else {
                return .commandFailed
            }
            guard let event = event as? MPChangePlaybackPositionCommandEvent else {
                return .commandFailed
            }
            Task { @MainActor in self.seek(toMs: Int(event.positionTime * 1000)) }
            return .success
        }))
    }

    private func removeRemoteCommands() {
        RemoteCommandOwner.shared.release(remoteOwnerToken)
        for (command, target) in remoteTargets { command.removeTarget(target) }
        remoteTargets = []
    }

    private func updateNowPlaying() {
        guard knownDurationMs > 0,
              RemoteCommandOwner.shared.isCurrent(remoteOwnerToken) else { return }
        let state = NowPlayingState(
            title: title,
            durationMs: knownDurationMs,
            elapsedMs: realPositionMs(),
            rate: isPlaying ? (player.rate > 0 ? player.rate : preferredRate) : 0,
            isPlaying: isPlaying
        )
        guard lastNowPlayingState != state else { return }
        lastNowPlayingState = state
        MPNowPlayingInfoCenter.default().nowPlayingInfo = [
            MPMediaItemPropertyTitle: state.title,
            MPMediaItemPropertyPlaybackDuration: Double(state.durationMs) / 1000.0,
            MPNowPlayingInfoPropertyElapsedPlaybackTime: Double(state.elapsedMs) / 1000.0,
            MPNowPlayingInfoPropertyPlaybackRate: state.rate,
            MPNowPlayingInfoPropertyDefaultPlaybackRate: 1.0,
            MPNowPlayingInfoPropertyIsLiveStream: false,
        ]
        MPNowPlayingInfoCenter.default().playbackState = state.isPlaying ? .playing : .paused
    }
}

// MARK: - Passive playback control

extension PlayerController {
    /// Start reporting for a session the server said is controllable.
    ///
    /// A server that sends no bootstrap, or one this client cannot address,
    /// leaves the reporter silent. That is the passive M2 behaviour: playback
    /// does not depend on the control plane and never should.
    /// Adopt only the received report for this HLS response's own identity.
    /// This does not start a reporter, choose a route, or derive processing.
    func adoptEffectiveProcessing(_ hls: HlsStart) {
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        // The enhancement answer belongs to this response, controllable or
        // not; a report-gated one stays hidden without a matching report.
        playbackEnhancement = hls.enhancement
        guard let bootstrap = hls.control, bootstrap.isValid else { return }
        effectiveProcessingGeneration = bootstrap.generation
        if let report = hls.effectiveProcessing,
           report.matches(generation: bootstrap.generation, delivered: hls.deliveredDynamicRange) {
            effectiveProcessing = report
        }
    }

    func beginPlaybackControl(_ hls: HlsStart, origin: String) {
        adoptEffectiveProcessing(hls)
        autoRouteProtocol = hls.displayAwareAutoProtocol
        if hls.displayAwareAutoProtocol == "route-v1" {
            if let candidates = hls.qualityCandidates { decision?.qualityCandidates = candidates }
            decision?.measuredCandidateOutputs = hls.measuredCandidateOutputs
            autoActiveCandidateId = hls.qualityCandidateId
        }
        pendingPreparedControl = nil
        playbackControlOrigin = nil
        guard let bootstrap = hls.control, bootstrap.isValid else {
            playbackControl.end()
            playbackControlSummary = nil
            return
        }
        startPlaybackControl(bootstrap: bootstrap, session: hls.sessionId, origin: origin)
    }

    private func startPlaybackControl(bootstrap: ControlBootstrap, session: String, origin: String) {
        playbackControlOrigin = origin
        // The override describes the session that just ended. Carrying it into
        // the replacement would make its very first exchange — a session that
        // has rendered nothing yet — report a wedge that belongs to another.
        controlObservationOverride = nil
        controlRenderOverride = nil
        controlEvidencePositionMs = nil
        playbackControl.begin(
            bootstrap: bootstrap,
            transport: PlaybackControlTransport(
                origin: origin,
                authorize: { request in Session.shared.authorize(&request) }
            ),
            observe: { [weak self] in self?.playbackControlObservation() },
            linkReceipt: { [weak self] in
                guard let self, self.sessionId == session else { return nil }
                return self.currentLinkReceipt()
            },
            onSubtitleReady: { [weak self] in
                self?.retryNativeSubtitleAfterReadiness()
            },
            onSubtitleUnavailable: { [weak self] in
                guard let self, self.selectedSubtitle != nil else { return }
                self.showPlaybackNotice("That subtitle could not be prepared. Playback was kept unchanged.")
            },
            onProcessingGeneration: { [weak self] generation, report in
                guard let self else { return }
                self.effectiveProcessing = report.flatMap {
                    $0.matches(generation: generation, delivered: self.deliveredRange) ? $0 : nil
                }
                self.effectiveProcessingGeneration = generation
            },
            // Deliberately ungated at this call site. Everything that could
            // refuse a staging — one already live, one already settled, a
            // switch in flight, a playback that has proved it cannot prime one
            // — is decided inside `offer`, so the server-pushed path and the
            // viewer-initiated path cannot disagree. A refusal here still
            // settles the staging: an unanswered one holds the server's only
            // preparation slot for 330 seconds.
            onPreparedReplacement: { [weak self] action in
                guard let self else { return }
                self.preparedReplacement.offer(
                    action, filmPositionMs: self.positionForPlaybackIntent()
                )
            },
            onAcknowledgementDelivered: { [weak self] acknowledgement in
                guard let self else { return }
                self.preparedReplacement.acknowledgementDelivered(acknowledgement)
                guard acknowledgement.state == .committed,
                      let next = self.pendingPreparedControl, next.actionId == acknowledgement.actionId,
                      self.started, self.sessionId == next.sessionId else { return }
                self.pendingPreparedControl = nil
                // The predecessor carried the durable commit. Only its
                // accepted exchange authorizes the successor control owner.
                self.startPlaybackControl(bootstrap: next.bootstrap, session: next.sessionId, origin: next.origin)
                self.noteSurfaceLogOnly("prepared_control_rebound:committed=true")
            },
            onEffectiveSelection: { [weak self] effective in
                guard let self, self.model?.displayAwareAutoProtocol == "route-v1" else { return }
                self.autoActiveCandidateId = effective.candidateId
            },
            onExchangeFailure: { [weak self] failure in
                guard let self else { return }
                self.noteSurfaceLogOnly("control_exchange:\(failure)")
                if Self.isPauseGraceExpiry(failure), !self.wantsPlayback,
                   !self.isChangingStream, self.currentPausedRetirement == nil,
                   let retiredSession = self.sessionId {
                    // The item is still attached and may keep playing out its
                    // buffer, but the session behind it is gone. Resume must
                    // open the replacement rather than play into a 404.
                    self.pausedRetirement = PausedRetirement(
                        sessionId: retiredSession, positionMs: nil
                    )
                }
            }
        )
        playbackControlSummary = "Owner epoch \(bootstrap.controlEpoch) · reporting"
    }

    /// AVPlayer may keep the first empty `no-store` subtitle segment. Toggle
    /// only the legible selection when control reports the demanded window as
    /// ready; the video item and its producer stay untouched.
    private func retryNativeSubtitleAfterReadiness() {
        // Gated on the ROUTE, not on `activeNativeSubtitle`. That property
        // means "the index this session was opened with" — it is written only
        // by `open()` and `stop()`, and never by `applySubtitleSelection` —
        // so a viewer who turned subtitles on mid-playback left it `nil` and
        // this retry silently declined for the rest of the session. That is
        // the whole in-place selection path, which is the common one.
        guard let index = selectedSubtitle,
              Self.subtitleSelectionRoute(
                for: index,
                tracks: subtitles,
                activeBurn: activeBurnedSubtitle,
                isDirectPlayback: isDirectPlayback,
                activeOverlay: pgsOverlayTrackIndex
              ) == .mediaSelection,
              let item = player.currentItem
        else { return }
        let actionEpoch = viewerActionEpoch
        Task { @MainActor [weak self, weak item] in
            guard let self, let item, self.player.currentItem === item,
                  self.viewerActionEpoch == actionEpoch, self.selectedSubtitle == index
            else { return }
            await self.applyNativeSubtitleSelection(nil, to: item, expectedActionEpoch: actionEpoch)
            guard self.viewerActionEpoch == actionEpoch, self.selectedSubtitle == index,
                  self.player.currentItem === item
            else { return }
            await self.applyNativeSubtitleSelection(index, to: item, expectedActionEpoch: actionEpoch)
        }
    }

    /// Everything the mapping needs, read from the player once.
    ///
    /// This is the only place AVFoundation meets the control protocol, and it
    /// is deliberately all reads: nothing here decides anything, so every rule
    /// that could be wrong lives in `PlaybackControlMapping`, where it is
    /// tested.
    func playbackControlObservation() -> PlayerControlObservation? {
        guard let item = player.currentItem else { return nil }
        let position = realPositionMs()

        // Only the runway *ahead* is protocol runway. AVPlayer's loaded ranges
        // are in item time while the protocol wants title time, so the range
        // is reported as beginning at the playhead rather than converted with
        // an offset that a copy session's keyframe start would make wrong.
        var bufferedFromMs: Int?
        var bufferedThroughMs: Int?
        if let runwaySeconds = bufferedRunwaySeconds(), runwaySeconds.isFinite {
            bufferedFromMs = position
            bufferedThroughMs = position + Int((runwaySeconds * 1_000).rounded())
        }

        return PlayerControlObservation(
            positionMs: position,
            durationMs: max(0, knownDurationMs),
            bufferedFromMs: bufferedFromMs,
            bufferedThroughMs: bufferedThroughMs,
            rate: Double(player.rate),
            isPaused: !wantsPlayback,
            isEnded: finished,
            isSeeking: seekState.pendingMs != nil,
            seekTargetMs: seekState.pendingMs,
            hasStarted: started,
            waitingForMs: controlWaitingSince.map {
                max(0, Int(Date().timeIntervalSince($0) * 1_000))
            },
            isLikelyToKeepUp: item.isPlaybackLikelyToKeepUp,
            // AVPlayer's own error object is a generic fallback and its
            // message can carry a media URL, so only the class travels.
            errorCode: isPlaybackBlocked ? .media : nil,
            errorDetail: isPlaybackBlocked ? "avplayer_item_failed" : nil,
            droppedFrames: nil,
            observedDownloadBps: observedDownloadBitsPerSecond(item),
            // A recovery path's evidence is more specific than anything
            // derived from the player's own state, so it wins field by field.
            // Until now both were hardcoded nil and no recovery site set
            // them, so the mapper's merge had nothing to merge.
            observationOverride: controlObservationOverride,
            renderOverride: controlRenderOverride,
            selection: playbackControlSelection(),
            capabilities: autoControlCapabilities(),
            // Sticky until the exchange that carried it comes back: the
            // reporter coalesces, so a settlement parked anywhere but the
            // newest snapshot is the one thing a position update can drop.
            acknowledgement: preparedReplacement.pendingAcknowledgement
        )
    }

    /// What the viewer chose, in the protocol's vocabulary.
    ///
    /// Codec and dynamic range are `auto` on purpose: after `/decision` this
    /// client never forces either, and reporting anything else would tell the
    /// server it had made a choice it has not made.
    private func autoControlCapabilities() -> DynamicCapabilities {
        var caps = Caps.controlCapabilities()
        if model?.displayAwareAuto == true, model?.displayAwareAutoProtocol == "route-v1",
           decision?.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1" {
            caps.presentationTarget = presentationTarget
            if let video = decisionCaps?.video {
                caps.decoderCaps = DecoderCapabilitySnapshot(revision: 1, video: video)
            }
        }
        return caps
    }

    func playbackControlSelection() -> ClientSelection {
        let mode = playbackControlSubtitleMode()
        return ClientSelection(
            quality: manualQualityRetention.retained?.incumbent ?? selectedHeight.map { .manual(height: $0) }
                ?? (selectedQualityIsOriginal ? .original :
                    (model?.displayAwareAuto == true &&
                     model?.displayAwareAutoProtocol == "route-v1" && autoRouteProtocol == "route-v1"
                        ? autoControlQualitySelection(desired: autoDesiredCandidate,
                            installedId: autoActiveCandidateId, candidates: decision?.qualityCandidates) : .auto)),
            audioTrack: selectedAudio,
            subtitle: SubtitleSelection(
                mode: mode,
                track: mode == .off ? nil : selectedSubtitle
            ),
            audioOffsetMs: 0,
            codec: .auto,
            dynamicRange: .auto
        )
    }

    func playbackControlSubtitleMode() -> SubtitleMode {
        if activeBurnedSubtitle != nil { return .burn }
        if pgsOverlayIsActive { return .overlay }
        if selectedSubtitle != nil { return .native }
        return .off
    }

    /// Called wherever the player's state moves. The reporter coalesces, so
    /// this is cheap enough for the periodic time observer.
    /// Uses only fresh completed network bodies with explicit producer pacing provenance.
    /// Selects the next route for the existing bounded compatibility recovery owner.
    private func refreshAutoCatalogForViewerRecipe() {
        autoDesiredCandidate = nil
        autoPreparing = false
        autoUpgradeSinceMs = nil
        guard let model, model.displayAwareAuto, model.displayAwareAutoProtocol == "route-v1",
              let fileId = decision?.fileId, let fileContext,
              (try? fileContext.localID(expected: fileId)) != nil else { return }
        decision?.qualityCandidates = nil
        let selection = PrePlaySelection(audioIndex: selectedAudio,
                                        subtitleIndex: selectedSubtitle ?? PrePlaySelection.subtitleOff)
        let catalogAttempt = snapshotAttempt()
        let target = presentationTarget
        Task { @MainActor [weak self] in
            let incumbentReceipt = self?.decision?.fileId == fileId ? self?.currentLinkReceipt() : nil
            let fresh = try? await Caps.PresentationContext.$target.withValue(target) {
                try await PlaybackLinkRequestContext.$receipt.withValue(incumbentReceipt) {
                    try await model.playbackDecision(fileId: fileId, selection: selection, quality: .auto, audioOffsetMs: 0, fileContext: fileContext)
                }
            }
            guard let self, self.started,
                  self.fileContext?.sourceKey == fileContext.sourceKey,
                  self.attemptStillCurrent(catalogAttempt, fence: .autoCatalogRefresh),
                  self.decision?.fileId == fileId else { return }
            if fresh?.decision.displayAwareAutoProtocol == "route-v1" {
                self.decision?.qualityCandidates = fresh?.decision.qualityCandidates
                self.decision?.measuredCandidateOutputs = fresh?.decision.measuredCandidateOutputs
            }
            // A decision is a plan; it cannot relabel the attached presentation.
        }
    }

    private func resetAutoQualityBudgetForViewer() {
        autoSwitchTimes.removeAll()
        autoLastSwitchMs = nil
        autoUpgradeSinceMs = nil
        autoMildSamples = 0
        autoTransferPressure.clear(notBeforeMs: PlaybackControlSession.monotonicMs())
        latestAutoCompletedTransfer = nil
        if autoPreparing { autoDesiredCandidate = nil; autoPreparing = false }
    }

    private func cancelAutoCostCatalogRefresh() {
        autoCostRefreshGeneration &+= 1
        autoCostRefreshTask?.cancel()
        autoCostRefreshTask = nil
        autoCostRefreshDeadlineTask?.cancel()
        autoCostRefreshDeadlineTask = nil
        autoCostRefreshNextMs = 0
    }

    private func autoCostCatalogOwner() -> AutoCostCatalogOwner? {
        guard started, selectedHeight == nil, !selectedQualityIsOriginal,
              let file = decision?.fileId, let context = fileContext,
              let session = sessionId, let candidate = autoActiveCandidateId,
              let target = presentationTarget else { return nil }
        return AutoCostCatalogOwner(lifecycle: lifecycleGeneration, viewerAction: viewerActionEpoch,
            recipe: recipeRevision.desired, file: file, source: context.sourceKey,
            session: session, candidate: candidate, target: target)
    }

    /// Refresh advisory full-output measurements without restarting Auto or
    /// replacing the installed identity. One bounded fetch may run per 30s.
    private func refreshUnmeasuredAutoCosts(now: Int) {
        guard autoCostRefreshTask == nil, now >= autoCostRefreshNextMs,
              !autoPreparing, preparedPlayer == nil,
              let model, model.displayAwareAuto, model.autoAbr,
              model.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1",
              let owner = autoCostCatalogOwner(), let context = fileContext,
              (try? context.localID(expected: owner.file)) != nil,
              let item = player.currentItem,
              autoNeedsEncodedCostRefresh(decision?.qualityCandidates ?? [],
                  outputs: decision?.measuredCandidateOutputs, current: owner.candidate) else { return }
        autoCostRefreshNextMs = now + 30_000
        autoCostRefreshGeneration &+= 1
        let generation = autoCostRefreshGeneration
        let attempt = snapshotAttempt()
        let selection = PrePlaySelection(audioIndex: selectedAudio,
            subtitleIndex: selectedSubtitle ?? PrePlaySelection.subtitleOff)
        let receipt = currentLinkReceipt()
        let refresh = Task { @MainActor [weak self] in
            guard let self else { return }
            defer {
                if self.autoCostRefreshGeneration == generation {
                    self.autoCostRefreshTask = nil
                    self.autoCostRefreshDeadlineTask?.cancel()
                    self.autoCostRefreshDeadlineTask = nil
                }
            }
            let fresh = try? await Caps.PresentationContext.$target.withValue(owner.target) {
                try await PlaybackLinkRequestContext.$receipt.withValue(receipt) {
                    try await model.playbackDecision(fileId: owner.file, selection: selection,
                        quality: .auto, audioOffsetMs: 0, fileContext: context)
                }
            }
            guard !Task.isCancelled, self.autoCostRefreshGeneration == generation,
                  !self.autoPreparing, self.preparedPlayer == nil,
                  self.attemptStillCurrent(attempt, fence: .autoMissingCostCatalogRefresh),
                  owner.owns(self.autoCostCatalogOwner(), sameItem: self.player.currentItem === item),
                  let fresh, fresh.decision.fileId == owner.file,
                  fresh.decision.displayAwareAutoProtocol == "route-v1",
                  let catalog = validatedAutoCostCatalog(fresh.decision.qualityCandidates,
                      outputs: fresh.decision.measuredCandidateOutputs, current: owner.candidate) else { return }
            self.decision?.qualityCandidates = catalog.candidates
            self.decision?.measuredCandidateOutputs = catalog.outputs
        }
        autoCostRefreshTask = refresh
        autoCostRefreshDeadlineTask = Task { @MainActor in
            do { try await Task.sleep(for: .seconds(5)) } catch { return }
            refresh.cancel()
        }
    }

    private func measuredCostCatalog(_ offered: [QualityCandidate]) -> [QualityCandidate] {
        autoCostedCatalog(offered, outputs: decision?.measuredCandidateOutputs)
    }

    private func autoBoundaryIsCurrent(_ boundary: AutoBoundaryAttempt) -> Bool {
        autoBoundaryAttempt === boundary &&
            attemptStillCurrent(boundary.attempt, fence: .autoBoundaryOwnerCurrent) &&
            sessionId == boundary.sessionId &&
            player.currentItem.map(ObjectIdentifier.init) == boundary.item &&
            wantsPlayback && started && selectedHeight == nil && !selectedQualityIsOriginal &&
            autoDesiredCandidate?.id == boundary.candidateId
    }

    private func autoOriginalBoundaryCandidate(now: Int) -> QualityCandidate? {
        guard autoBoundaryAttempt == nil, !autoPreparing,
              model?.displayAwareAuto == true, model?.autoAbr == true,
              autoRouteProtocol == "route-v1", decision?.displayAwareAutoProtocol == "route-v1",
              selectedHeight == nil, !selectedQualityIsOriginal, wantsPlayback,
              preparedReplacement.shouldAskForPreparation, Caps.controlCapabilities().dualPlayerPreparation,
              player.currentItem?.status == .readyToPlay, (bufferedRunwaySeconds() ?? 0) >= 10,
              let sessionId, currentLinkReceipt() != nil,
              let sample = latestAutoCompletedTransfer,
              let link = autoCompletedTransferBps(sample, nowMs: now, maximumAgeMs: 15_000),
              let offered = decision?.qualityCandidates,
              let current = measuredCostCatalog(offered).first(where: { $0.id == autoActiveCandidateId })
        else { return nil }
        // Only a fresh body's actual negative can veto a boundary. The old
        // ninety-second mid-play cliff window is intentionally not consulted.
        if let cost = autoDownsideCostBps(current, sample: sample, sessionId: sessionId, nowMs: now), link < cost {
            return nil
        }
        return autoBoundaryOriginalCandidate(measuredCostCatalog(offered), current: current, link: link,
            outputs: decision?.measuredCandidateOutputs, rejected: autoDecoderRejected,
            blockedUntil: autoBlockedUntil, nowMs: now)
    }

    /// Ask for the boundary's original candidate through the same accepted
    /// prepared transaction the mid-play upgrade uses, and return at once. The
    /// incumbent is already playing for the viewer; the successor primes beside
    /// it and the ordinary commit hands off, or it is abandoned and the
    /// incumbent simply carries on. Commit, failure and withdrawal all settle
    /// through the prepared coordinator's own callbacks.
    private func beginAutoBoundaryPreparation(_ chosen: QualityCandidate, target: PresentationTarget) -> Bool {
        guard let item = player.currentItem, let sessionId else { return false }
        let boundary = AutoBoundaryAttempt(attempt: snapshotAttempt(), item: ObjectIdentifier(item),
            sessionId: sessionId, candidateId: chosen.id, previous: autoDesiredCandidate)
        autoBoundaryAttempt = boundary
        autoDesiredCandidate = chosen
        autoPreparing = true
        autoVoluntary = true
        autoExposed = false
        autoPreparedTargetRevision = target.revision
        autoPreparedViewerEpoch = viewerActionEpoch
        // Do not dirty recipeRevision: a refused optional stage must leave no
        // trace that turns the viewer's next seek into an ordinary create.
        Task { @MainActor [weak self] in
            guard let self else { return }
            let offered = await self.offerPreparedQualityChange(boundary: boundary)
            if !offered { self.withdrawAutoBoundary(boundary) }
        }
        return true
    }

    /// Put Auto back where the boundary found it, if nothing newer owns it.
    /// A boundary that was superseded or refused disproved nothing about the
    /// candidate, so it earns no backoff.
    private func withdrawAutoBoundary(_ boundary: AutoBoundaryAttempt) {
        if autoBoundaryAttempt === boundary { autoBoundaryAttempt = nil }
        guard autoDesiredCandidate?.id == boundary.candidateId else { return }
        autoDesiredCandidate = boundary.previous
        autoPreparing = false
        autoUpgradeSinceMs = nil
        playbackControl.reportEvidence()
    }

    private func selectAutoStallRecoveryCandidate(attempt: Attempt, deadline: Double?) async {
        guard model?.displayAwareAuto == true, model?.autoAbr == true,
              model?.displayAwareAutoProtocol == "route-v1", decision?.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1",
              selectedHeight == nil, !selectedQualityIsOriginal, wantsPlayback,
              seekState.pendingMs == nil, let offered = decision?.qualityCandidates,
              case let candidates = measuredCostCatalog(offered),
              let current = candidates.first(where: { $0.hasValidIdentity && $0.id == autoActiveCandidateId })
        else { return }
        let now = PlaybackControlSession.monotonicMs()
        autoUpgradeEvidence.bind(attachment: player.currentItem.map(ObjectIdentifier.init), attempt: playbackAttemptId, nowMs: now)
        autoSwitchTimes.removeAll { now - $0 >= 3_600_000 }
        let transfer = latestAutoCompletedTransfer
        guard let verdict = autoStallRecoveryVerdict(current: current, candidates: candidates,
            transfer: transfer,
            transferCurrent: autoTransferPressureOwnerCurrent && transfer.map(autoTransferOriginCurrent) == true,
            sessionId: sessionId, candidateId: autoActiveCandidateId,
            mildSamples: autoTransferPressure.mildSamples(nowMs: now),
            producerPressure: autoActiveProductionPressure(status: sessionStatus,
                observedAtMs: sessionStatusAgeMs.map { now - $0 }, nowMs: now,
                sessionId: sessionId, candidateId: current.id, runwaySeconds: bufferedRunwaySeconds()),
            rejected: autoDecoderRejected,
            switchBudgetOpen: autoSwitchTimes.count < 6 && (autoLastSwitchMs.map { now - $0 >= 60_000 } ?? true),
            nowMs: now)
        else { return }
        let severe = verdict.severe
        let producer = verdict.producer
        if severe, let transfer {
            autoUpgradeEvidence.cliff(completedAtMs: transfer.completedAtMs, nowMs: now)
        }
        if let next = verdict.next {
            if !producer {
                guard let deadline, let sample = latestAutoCompletedTransfer,
                      let receipt = sample.receipt, let incumbent = player.currentItem,
                      let incumbentSession = sessionId,
                      let payload = negativeLinkPayload(),
                      await acknowledgeNegativeLink(payload, receipt: receipt, deadline: deadline),
                      !Task.isCancelled, ProcessInfo.processInfo.systemUptime < deadline,
                      attemptStillCurrent(attempt, fence: .stallCandidateAcknowledgement),
                      player.currentItem === incumbent, sessionId == incumbentSession,
                      autoActiveCandidateId == current.id, selectedHeight == nil, !selectedQualityIsOriginal,
                      wantsPlayback, surface.presenting, seekState.pendingMs == nil,
                      stallRecoveryStillEligible, preparedPlayer == nil,
                      player.timeControlStatus == .waitingToPlayAtSpecifiedRate,
                      latestAutoCompletedTransfer?.receipt == receipt,
                      latestAutoCompletedTransfer?.completedAtMs == sample.completedAtMs,
                      sample.ageMs(nowMs: PlaybackControlSession.monotonicMs()) <= 15_000 else { return }
                autoLinkClaims[receipt] = (sample.completedAtMs, true)
            }
            if let sessionId, let item = player.currentItem {
                autoRecoveryCause = AutoRecoveryCauseTicket(session: sessionId, attachment: ObjectIdentifier(item),
                    incumbentCandidate: current.id, proposedCandidate: next.id,
                    cause: producer ? .encode : .link, observedAtMs: now,
                    linkReceipt: producer ? nil : latestAutoCompletedTransfer?.receipt)
            }
            reportAutoRecoveryCause(producer ? .encode : .link)
            autoDesiredCandidate = next
            autoPreparing = false
            autoUpgradeSinceMs = nil
            recipeRevision.change()
            noteSurfaceLogOnly("auto_quality_recovery:\(severe ? "link" : "encode"):\(next.id)")
        }
    }

    @discardableResult private func noteAutoDecodeFailure(pressure: AutoDecodePressureEvidence? = nil) async -> Bool {
        guard model?.displayAwareAuto == true, model?.autoAbr == true,
              model?.displayAwareAutoProtocol == "route-v1", decision?.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1",
              selectedHeight == nil, !selectedQualityIsOriginal,
              let candidates = decision?.qualityCandidates,
              let current = candidates.first(where: { $0.id == autoActiveCandidateId }),
              let sessionId, let item = player.currentItem else { return false }
        let observedAt = PlaybackControlSession.monotonicMs()
        // The failure is this device's own evidence, so the candidate joins the
        // local rejected set now. The server's acceptance of the sample only
        // authorizes server-side consequences (recording the decode cause on
        // the reopen), which the server checks for itself; nothing waits on it.
        guard rejectFailedDecoderCandidate(&autoDecoderRejected, candidateId: current.id) else { return false }
        postClientLog(AppleCandidateRecoveryLog(session_id: sessionId, candidate_recovery: .init(
                candidate_id: current.id, recipe_digest: current.recipeDigest,
                decoder_failed: pressure == nil,
                rendered_elapsed_ms: pressure?.elapsedMs ?? 0, position_progress_ms: pressure?.progressMs ?? 0,
                dropped_frames: pressure?.droppedFrames ?? 0,
                runway_ms: Int(max(0, (bufferedRunwaySeconds() ?? 0) * 1_000)))))
        guard !autoDecodeQualityResponseUsed else { return false }
        let next = autoRecoveryCandidate(candidates, current: current, rejected: autoDecoderRejected, decoderRecovery: true)
        if let next {
            autoDecodeQualityResponseUsed = true
            autoRecoveryCause = AutoRecoveryCauseTicket(session: sessionId, attachment: ObjectIdentifier(item),
                incumbentCandidate: current.id, proposedCandidate: next.id, cause: .decode,
                observedAtMs: observedAt)
            autoDesiredCandidate = next
            autoPreparing = false
            autoUpgradeSinceMs = nil
            recipeRevision.change()
            return true
        }
        return false
    }

    private func reportStagedLinkSample(_ sample: AutoCompletedTransfer, action: PreparedReplacementAction, item: AVPlayerItem) {
        let now = PlaybackControlSession.monotonicMs()
        guard preparedItem === item, preparedReplacement.activeAction?.sessionId == action.sessionId,
              sample.statusCode == 200, sample.networkLoad, !sample.fromLocalCache,
              autoTransferOriginCurrent(sample), sample.producerPaced == false,
              sample.ageMs(nowMs: now) <= 15_000, sample.bodyBytes > 0,
              let duration = sample.bodyDurationSeconds, duration > 0, duration <= 120,
              let receipt = sample.receipt, UUID(uuidString: receipt)?.uuidString.lowercased() == receipt,
              let etag = sample.etag, !etag.isEmpty,
              let url = URL(string: sample.segmentId), url.pathComponents.contains(action.sessionId),
              autoLinkClaims[receipt] == nil, autoLinkClaims.count < 32 else { return }
        postClientLog(AppleCandidateLinkLog(session_id: action.sessionId, link_sample: .init(
            receipt: receipt, object_name: url.lastPathComponent, etag: etag,
            body_bytes: sample.bodyBytes, body_duration_ms: Int((duration * 1000).rounded()),
            age_ms: sample.ageMs(nowMs: now), negative: false,
            media_duration_ms: sample.observedMediaDurationMs, presenting: false, stalled: false, runway_ms: 0)))
        autoLinkClaims[receipt] = (sample.completedAtMs, false)
    }

    private func autoStagedOriginalAllowsCommit(_ action: PreparedReplacementAction) -> Bool {
        guard autoPreparing, autoVoluntary, let desired = autoDesiredCandidate else { return true }
        guard let observation = autoStagedObservation, autoStagedObservationCurrent(observation) else { return false }
        let now = PlaybackControlSession.monotonicMs()
        let samples = autoStagedTransfers.filter { sample in
            sample.stageScope == observation.scope && sample.installedSessionId == observation.sessionId &&
                sample.installedCandidateId == observation.candidateId &&
                sample.segmentId.contains("/\(action.sessionId)/") && sample.receipt != nil && sample.etag != nil
                && autoTransferOriginCurrent(sample)
                && sample.statusCode == 200 && sample.networkLoad && !sample.fromLocalCache
                && sample.producerPaced == false && sample.ageMs(nowMs: now) <= 15_000
                && (sample.bodyDurationSeconds ?? 0) > 0 && sample.bodyBytes > 0
        }
        let costed = measuredCostCatalog(decision?.qualityCandidates ?? [])
            .first(where: { $0.id == desired.id && $0.recipeDigest == desired.recipeDigest }) ?? desired
        if autoUnknownOriginalTrial(costed, outputs: decision?.measuredCandidateOutputs) {
            guard let intervals = observation.intervals else { return false }
            return autoVODEmpiricalMargin(samples, intervals: intervals, scope: observation.scope,
                sessionId: observation.sessionId, candidateId: observation.candidateId,
                nowMs: now, deadlineMs: observation.deadlineMs)
        }
        // The staged successor must itself prove the headroom its cost needs
        // — measured or declared, the same figure that admitted it.
        guard let cost = autoDeclaredCostBps(costed) else { return false }
        return samples.contains { Double($0.bodyBytes) * 8 / ($0.bodyDurationSeconds ?? 1) >= cost * 1.8 }
    }

    private func autoStagedProductionAllowsCommit(_ action: PreparedReplacementAction) -> Bool {
        guard autoPreparing, autoVoluntary, let desired = autoDesiredCandidate,
              desired.route == "encode", !desired.completeCache else { return true }
        return autoStagedEncodeProof(autoStagedStatus, observedAtMs: autoStagedStatusObservedMs,
            nowMs: PlaybackControlSession.monotonicMs(), sessionId: action.sessionId, candidateId: desired.id)
    }

    private func autoTrialAllowsExposure(_ action: PreparedReplacementAction) -> Bool {
        autoPreparing && model?.displayAwareAuto == true && model?.autoAbr == true &&
            model?.displayAwareAutoProtocol == "route-v1" && decision?.displayAwareAutoProtocol == "route-v1" &&
            autoRouteProtocol == "route-v1" &&
            autoTrialMayCommit(requestedId: autoDesiredCandidate?.id, offeredId: action.effectiveSelection.candidateId,
                requestedTargetRevision: autoPreparedTargetRevision, targetRevision: presentationTarget?.revision,
                requestedViewerEpoch: autoPreparedViewerEpoch, viewerEpoch: viewerActionEpoch,
                automatic: selectedHeight == nil && !selectedQualityIsOriginal,
                presenting: wantsPlayback && surface.presenting && player.rate > 0,
                seeking: seekState.pendingMs != nil) &&
            autoStagedProductionAllowsCommit(action) && autoStagedOriginalAllowsCommit(action)
    }

    private func tickDisplayAwareAuto(pressureOnly: Bool = false) {
        let now = PlaybackControlSession.monotonicMs()
        autoUpgradeEvidence.bind(attachment: player.currentItem.map(ObjectIdentifier.init), attempt: playbackAttemptId, nowMs: now)
        if !pressureOnly {
            guard now >= autoNextTickMs else { return }
            autoNextTickMs = now + 5_000
            refreshUnmeasuredAutoCosts(now: now)
        }
        guard model?.displayAwareAuto == true, model?.autoAbr == true,
              model?.displayAwareAutoProtocol == "route-v1",
              decision?.displayAwareAutoProtocol == "route-v1", autoRouteProtocol == "route-v1",
              selectedHeight == nil, !selectedQualityIsOriginal, wantsPlayback,
              player.rate > 0, surface.presenting, seekState.pendingMs == nil, !autoPreparing,
              preparedPlayer == nil, preparedReplacement.shouldAskForPreparation,
              let target = presentationTarget,
              let offered = decision?.qualityCandidates,
              case let candidates = measuredCostCatalog(offered),
              let current = candidates.first(where: { $0.hasValidIdentity && $0.id == autoActiveCandidateId })
        else { if !pressureOnly { autoUpgradeSinceMs = nil }; return }
        // A viewer boundary re-plans original first, once, on the first
        // evaluation where the playing incumbent has the runway a handoff
        // needs. The incumbent was never held for it.
        // Without a fresh link proof there is no candidate, and the boundary
        // stays owed for the next evaluation rather than being spent on nothing.
        let runway = bufferedRunwaySeconds()
        if !pressureOnly, let chosen = autoBoundaryReplan.take(runwaySeconds: runway,
                                                isCurrent: { attemptStillCurrent($0, fence: .autoBoundaryReplanCurrent) },
                                                produce: { autoOriginalBoundaryCandidate(now: now) }),
           beginAutoBoundaryPreparation(chosen, target: target) {
            return
        }
        autoSwitchTimes.removeAll { now - $0 >= 3_600_000 }
        let eligible = candidates.filter {
            $0.hasValidIdentity && !autoDecoderRejected.contains($0.id) &&
            $0.width > 0 && $0.height > 0 && $0.decoderCompatible &&
            ($0.route != "encode" || $0.grade == current.grade) &&
            (autoBlockedUntil[$0.id] ?? 0) <= now
        }
        autoMildSamples = autoTransferPressure.mildSamples(nowMs: now)
        let transfer = latestAutoCompletedTransfer
        // The whole pressure decision — link, cost, severity and the rung a
        // link cause moves to — is `autoLinkPressureVerdict`, decided from
        // evidence alone. Link pressure on a costed incumbent is pressure
        // whatever the runway: an original is costed by its declared source
        // rate, and a link below it is draining the buffer whether or not it
        // has reached ten seconds yet.
        guard let verdict = autoLinkPressureVerdict(current: current, eligible: eligible,
            transfer: transfer,
            transferCurrent: autoTransferPressureOwnerCurrent && transfer.map(autoTransferOriginCurrent) == true,
            sessionId: sessionId, candidateId: autoActiveCandidateId, mildSamples: autoMildSamples,
            producerPressure: autoActiveProductionPressure(status: sessionStatus,
                observedAtMs: sessionStatusAgeMs.map { now - $0 }, nowMs: now,
                sessionId: sessionId, candidateId: current.id, runwaySeconds: bufferedRunwaySeconds()),
            runwaySeconds: bufferedRunwaySeconds(), playbackRate: Double(player.rate),
            rejected: autoDecoderRejected, pressureOnly: pressureOnly, nowMs: now)
        else { return }
        let severe = verdict.severe
        let pressure = verdict.pressure
        if severe, let transfer {
            autoUpgradeEvidence.cliff(completedAtMs: transfer.completedAtMs, nowMs: now)
        }
        let currentArea = Int64(current.width) * Int64(current.height)
        let aspect = Double(current.width) / Double(current.height)
        let neededWidth = min(Double(target.widthPx), Double(target.heightPx) * aspect)
        let neededHeight = min(Double(target.heightPx), Double(target.widthPx) / aspect)
        let chosen: QualityCandidate?
        if pressure {
            chosen = verdict.chosen
            autoUpgradeSinceMs = nil
        } else {
            chosen = autoUpgradeCandidate(eligible, link: verdict.link, outputs: decision?.measuredCandidateOutputs,
                                          neededWidth: neededWidth, neededHeight: neededHeight)
            guard let chosen,
                  Int64(chosen.width) * Int64(chosen.height) > currentArea ||
                    (current.route == "encode" && chosen.route != "encode"),
                  (bufferedRunwaySeconds() ?? 0) >= 10,
                  autoSwitchTimes.count < 6,
                  autoLastSwitchMs.map({ now - $0 >= 60_000 }) ?? true
            else { autoUpgradeSinceMs = nil; return }
            if autoUpgradeSinceMs == nil { autoUpgradeSinceMs = now }
            guard autoUpgradeEvidence.allowsProposal(nowMs: now, headroomStartedAtMs: autoUpgradeSinceMs) else { return }
            if let evaluation = autoLastEvaluation,
               player.currentItem.map(ObjectIdentifier.init) == evaluation.item,
               playbackAttemptId == evaluation.attempt,
               now < evaluation.atMs || now - evaluation.atMs < 60_000 { return }
        }
        guard let chosen else { return }
        guard severe || ((autoLastSwitchMs.map { now - $0 >= 60_000 } ?? true) && autoSwitchTimes.count < 6) else { return }
        autoDesiredCandidate = chosen
        if !pressure, let item = player.currentItem {
            autoLastEvaluation = (ObjectIdentifier(item), playbackAttemptId, now)
        }
        autoVoluntary = !pressure
        autoPreparing = true
        autoExposed = false
        autoPreparedTargetRevision = target.revision
        autoPreparedViewerEpoch = viewerActionEpoch
        recipeRevision.change()
        let offerAttempt = snapshotAttempt()
        Task { @MainActor [weak self] in
            guard let self else { return }
            let offered = await self.offerPreparedQualityChange()
            guard self.attemptStillCurrent(offerAttempt, fence: .autoQualityOffer) else {
                // The newer intent owns its flags; an old offer cannot clear them.
                return
            }
            if !offered {
                self.autoBlockedUntil[chosen.id] = PlaybackControlSession.monotonicMs() + 300_000
                self.autoDesiredCandidate = nil
                self.autoPreparing = false
                self.autoUpgradeSinceMs = nil
                self.playbackControl.reportEvidence()
            }
        }
    }

    func playbackControlPlayerChanged() {
        tickDisplayAwareAuto()
        sampleSeekPresentationClocks()
        refreshControlWaiting()
        expireControlEvidenceIfProgressed()
        playbackControl.playerChanged()
    }

    private func refreshControlWaiting() {
        if player.timeControlStatus == .waitingToPlayAtSpecifiedRate {
            if controlWaitingSince == nil { controlWaitingSince = Date() }
        } else {
            controlWaitingSince = nil
        }
    }

    /// A moving film clock is the only proof the stall this evidence describes
    /// is over.
    ///
    /// `timeControlStatus == .playing` is not that proof and must never be
    /// used as it: a `.silent` stall is *defined* as a player whose clock
    /// stopped while it claimed motion, so clearing on that status would
    /// discard the one kind the server cannot derive from anything it holds —
    /// and would discard it inside the same call that published it.
    private func expireControlEvidenceIfProgressed() {
        guard let publishedAt = controlEvidencePositionMs else { return }
        // Any movement, not only forward movement. A viewer who stalls an hour
        // in and scrubs back to five minutes has ended the stall this evidence
        // describes, and the mapper ranks an override above everything the
        // player reports — so a forward-only test would report `stalled` for
        // the rest of the title.
        guard realPositionMs() != publishedAt else { return }
        controlObservationOverride = nil
        controlRenderOverride = nil
        controlEvidencePositionMs = nil
    }

    /// Publish what a recovery owner is about to act on, and send it now.
    ///
    /// Two things are load-bearing. The override is set before the snapshot is
    /// taken, or the exchange carries the player's own vaguer version of the
    /// same moment. And the report is urgent rather than coalesced, because
    /// the owner's reopen normally ends this reporter before its next
    /// scheduled exchange — the same reason the web client drains inline here.
    func reportControlEvidence(
        _ observation: ClientObservation?,
        render: RenderState? = nil
    ) {
        refreshControlWaiting()
        controlObservationOverride = observation
        controlRenderOverride = render
        controlEvidencePositionMs = realPositionMs()
        playbackControl.reportEvidence()
    }
}

// MARK: - The prepared successor

/// The AVFoundation half of a prepared quality handoff.
///
/// Everything policy-shaped — which acknowledgement is owed, whether an
/// offered staging is a replay, when a bound has elapsed — lives in
/// `PreparedReplacementCoordinator`, where it is provable without a device.
/// What is left here is the part that genuinely needs AVFoundation, and it is
/// written on the assumption that **the fallback is ordinary**: M5.5 proved
/// two `AVPlayer`s coexist on both Apple devices, and that is a logical
/// pipeline count, not a hardware decoder-slot count. A design in which
/// failing to prime a successor is an error branch produces a stuck player on
/// the first device that has one slot.
extension PlayerController: PreparedSuccessorHost {
    func acceptsPreparedSuccessor(_ action: PreparedReplacementAction) -> Bool {
        let matches = autoPreparedCandidateMatches(selection: playbackControlSelection().quality,
            effective: action.effectiveSelection)
        let safeCandidate = { (id: String?) -> String in
            guard let id, let candidate = self.decision?.qualityCandidates?.first(where: {
                $0.hasValidIdentity && $0.id == id
            }) else { return "unknown" }
            return candidate.id
        }
        let fingerprint = { (value: String) in
            SHA256.hash(data: Data(value.utf8)).prefix(8).map { String(format: "%02x", $0) }.joined()
        }
        let response = decision?.qualityCandidates?.first {
            $0.hasValidIdentity && $0.id == action.effectiveSelection.candidateId
        }
        let recipe = response?.recipeDigest.map { String(format: "%02x", $0) }.joined() ?? "unknown"
        noteSurfaceLogOnly("prepared_acquisition_binding:accepted=\(matches):action=\(fingerprint(action.actionId))"
            + ":session=\(fingerprint(action.sessionId)):desired=\(safeCandidate(autoDesiredCandidate?.id))"
            + ":installed=\(safeCandidate(autoActiveCandidateId)):response=\(safeCandidate(action.effectiveSelection.candidateId))"
            + ":recipe=\(recipe)")
        return matches
    }

    /// Build the muted second pipeline on its staging layer and prime it to the film
    /// position the viewer is watching.
    func startPreparedSuccessor(
        _ action: PreparedReplacementAction,
        filmPositionMs: Int
    ) -> Bool {
        guard autoPreparedCandidateMatches(selection: playbackControlSelection().quality,
            effective: action.effectiveSelection) else { return false }
        discardPreparedSuccessor()
        let capturedFilmPositionMs = max(0, filmPositionMs)
        guard let incumbentItem = player.currentItem else { return false }
        let incumbentPlayer = player
        let incumbentEpoch = viewerActionEpoch
        let anchors = PreparedAcquisitionAnchors(
            observedIntentFilmMs: capturedFilmPositionMs,
            allowsAdvance: autoPreparing && !autoVoluntary && wantsPlayback && player.rate > 0
                && incumbentItem.status == .readyToPlay && autoPreparedViewerEpoch == viewerActionEpoch
                && player.rate == preferredRate && seekState.pendingMs == nil && !isChangingStream
                && preparedReplacement.activeAction?.actionId == action.actionId,
            actualRunwaySeconds: bufferedRunwaySeconds(), playbackRate: Double(preferredRate),
            overlapRemainingMs: PreparedReplacementBounds.overlapMs,
            durationMs: knownDurationMs > 0 ? knownDurationMs : nil)
        // `playlistUrl` was proven node-relative before this action reached
        // the player, so this join cannot reach another origin — the same
        // property the control bootstrap's path check gives the reporter.
        guard started,
              player === incumbentPlayer, player.currentItem === incumbentItem,
              viewerActionEpoch == incumbentEpoch,
              let actionURL = Session.shared.url(action.playlistUrl),
              let url = preparedAcquisitionURL(actionURL, filmPositionMs: anchors.acquisitionFilmMs)
        else { return false }
        preparedOverlapStartedAtMs = Int(ProcessInfo.processInfo.systemUptime * 1_000)
        let item = AVPlayerItem(url: url)
        Self.configureBuffering(item, growingHLS: true)
        let output = AVPlayerItemVideoOutput(pixelBufferAttributes: nil)
        item.add(output)
        preparedVideoOutput = output
        let successor = AVPlayer(playerItem: item)
        // Muted on a retained staging layer; the predecessor owns playback
        // until the target's layer and aligned item are ready for promotion.
        // The audio session itself is deliberately untouched — the successor
        // gets whatever the incumbent already established.
        successor.isMuted = true
        successor.volume = 0
        successor.allowsExternalPlayback = false
        // And it does not get to pick tracks. The incumbent owns media
        // selection outright (see `start()`); this player was left at
        // AVFoundation's default `true`, so while it primed it was free to
        // enable a legible rendition from the master's AUTOSELECT/DEFAULT
        // metadata and the system caption preference. The item it hands over
        // at commit then carried whatever THIS player chose rather than what
        // the viewer chose — which is how a subtitle that was showing stops
        // while still selected in the menu.
        successor.appliesMediaSelectionCriteriaAutomatically = false
        preparedPlayer = successor
        stagedSurfacePlayer = successor
        preparedItem = item
        if autoPreparing, let desired = autoDesiredCandidate, desired.recipeDigest.count == 32,
           action.effectiveSelection.candidateId == desired.id {
            let now = PlaybackControlSession.monotonicMs()
            let credentials = Session.shared.credentials
            autoStagedObservation = AutoStagedObservation(scope: UUID(), item: ObjectIdentifier(item),
                pipeline: ObjectIdentifier(successor), sessionId: action.sessionId, candidateId: desired.id,
                recipeDigest: desired.recipeDigest, credentialOrigin: credentials.origin,
                credentialToken: credentials.token, startedAtMs: now,
                deadlineMs: now + 15_000)
        }
        installAutoTransferMetrics(for: item, staged: true, stagedAction: action)
        preparedAcquisitionAnchors = anchors
        preparedAcquisitionFilmPositionMs = anchors.acquisitionFilmMs
        noteSurfaceLogOnly("prepared_acquisition_anchor:observed_film_ms=\(anchors.observedIntentFilmMs):acquisition_film_ms=\(anchors.acquisitionFilmMs):advanced=\(anchors.acquisitionFilmMs > anchors.observedIntentFilmMs ? 1 : 0)")
        // The successor's session-relative zero is not the film's. This is the
        // same arithmetic a created session gets, over a raw origin — and it
        // is *recorded* here rather than performed, because the item is one
        // statement old, its status is `.unknown`, and AVFoundation drops a
        // seek on an item that cannot yet honour it. The created-session path
        // has the same problem and solves it the same way, by waiting for
        // readiness first (`seekWhenReady`).
        preparedSeekMs = Self.sessionAttachSeekMs(
            requestedStartMs: preparedAcquisitionFilmPositionMs,
            mediaOriginMs: action.mediaOriginMs
        )
        acquirePreparedIncumbentBufferPreference(action)
        observePreparedLifecycle()
        beginPreparedReadinessMonitor()
        return true
    }

    /// Backgrounding is an exit like any other. On tvOS a leaked second player
    /// survives screensaver activation, and on iOS the app's audio background
    /// mode would keep it alive behind the viewer's back.
    private func observePreparedLifecycle() {
        preparedLifecycle = [
            NotificationCenter.default
                .publisher(for: UIApplication.didEnterBackgroundNotification)
                .sink { [weak self] _ in
                    MainActor.assumeIsolated {
                        self?.preparedReplacement.abandonWithoutFallback(.aborted)
                    }
                }
        ]
    }

    /// Row 18. Nothing the viewer can see changed — the incumbent's item, its
    /// session and its pointer are exactly as they were — so this raises the
    /// one source that maps to no class and draws nothing.
    func notePreparedSuccessorAbandoned(_ reason: PreparedReplacementAbandonment) {
        let wasAutomatic = autoPreparing
        if !autoPreparing { retainManualQualityFailure() }
        // A boundary preparation that was superseded or refused (`aborted`)
        // is withdrawn without backoff; one that failed is an ordinary Auto
        // failure.
        if let boundary = autoBoundaryAttempt {
            autoBoundaryAttempt = nil
            if reason == .aborted { withdrawAutoBoundary(boundary) }
        }
        if autoPreparing {
            if let candidate = autoDesiredCandidate {
                autoBlockedUntil[candidate.id] = PlaybackControlSession.monotonicMs() + 300_000
            }
            autoDesiredCandidate = nil
            autoPreparing = false
            autoUpgradeSinceMs = nil
        }
        if reason == .failed {
            let itemError = preparedSuccessorErrorDetail(preparedItem?.error as NSError?)
            let phase = String(describing: preparedReplacement.phase)
            let remaining = preparedReplacement.readinessRemainingMs()
            let readiness = remaining.map(String.init) ?? "unknown"
            let itemStatus = preparedItem?.status.rawValue.description ?? "missing"
            let playerStatus = preparedPlayer?.status.rawValue.description ?? "missing"
            let candidate = decision?.qualityCandidates?.first {
                $0.hasValidIdentity && $0.id == preparedReplacement.activeAction?.effectiveSelection.candidateId
            }
            let route = candidate.map { ["original", "remux", "encode"].contains($0.route) ? $0.route : "unknown" } ?? "unknown"
            let elapsed = preparedOverlapStartedAtMs.map {
                max(0, Int(ProcessInfo.processInfo.systemUptime * 1_000) - $0)
            }.map(String.init) ?? "unknown"
            let cause = preparedItem?.status == .failed ? "item_failed"
                : preparedOverlapStartedAtMs != nil && preparedOverlapRemainingMs == 0 ? "overlap_deadline"
                : remaining == 0 ? (preparedReplacement.phase == .building ? "metadata_deadline" : "readiness_deadline")
                : "unclassified"
            // Capture acquisition failures before disposal; after exposure,
            // missing prepared state is unknown, not an expired overlap.
            // Never log media URLs,
            // request headers or error descriptions, which may carry credentials.
            noteSurfaceLogOnly(
                "prepared_successor_failure:stage=\(phase):item=\(itemError)"
                    + ":ready_ms=\(readiness):overlap_ms=\(preparedOverlapRemainingMs)"
                    + ":cause=\(cause):elapsed_ms=\(elapsed):item_status=\(itemStatus):player_status=\(playerStatus)"
                    + ":candidate=\(candidate?.id ?? "unknown"):route=\(route):height=\(candidate?.height ?? 0)"
                    + ":automatic=\(wasAutomatic):voluntary=\(autoVoluntary)"
            )
        }
        noteSurfaceLogOnly(
            "prepared_successor_abandoned:\(reason == .failed ? "failed" : "aborted")"
        )
    }

    func discardPreparedSuccessor() {
        restorePreparedIncumbentBufferPreference()
        clearPreparedCadence()
        preparedOverlapStartedAtMs = nil
        // The preparation a boundary asked for is gone with its pipeline.
        if preparedReplacement.phase != .switching { autoBoundaryAttempt = nil }
        autoStagedPlaylistTask?.cancel()
        autoStagedPlaylistTask = nil
        autoStagedObservation = nil
        autoStagedTransferMetricTask?.cancel()
        autoStagedTransferMetricTask = nil
        autoStagedTransfers.removeAll()
        autoStagedStatus = nil
        autoStagedStatusObservedMs = nil
        preparedMonitor?.cancel()
        preparedMonitor = nil
        preparedLifecycle.removeAll()
        if let output = preparedVideoOutput {
            output.setDelegate(nil, queue: nil)
            preparedItem?.remove(output)
        }
        preparedVideoOutput = nil
        preparedPlayer?.cancelPendingPrerolls()
        preparedPlayer?.replaceCurrentItem(with: nil)
        preparedPlayer = nil
        if let previous = warmPredecessor, previous !== player {
            previous.pause()
            previous.replaceCurrentItem(with: nil)
        }
        warmPredecessor = nil
        stagedSurfacePlayer = nil
        preparedItem = nil
        preparedAcquisitionFilmPositionMs = 0
        preparedAcquisitionAnchors = nil
        preparedSeekMs = nil
    }

    /// Watch the second pipeline until it is worth switching to, gives up, or
    /// runs out of time.
    private func beginPreparedReadinessMonitor() {
        preparedMonitor?.cancel()
        preparedMonitor = Task { @MainActor [weak self] in
            guard let self, let item = self.preparedItem,
                  let successor = self.preparedPlayer else { return }
            let observer = AVPlayerItemObserver(item: item, player: successor)
            defer { observer.cancel() }
            // Subscribe before the initial status read. The timer uses the
            // coordinator's original open clock, not a fresh six seconds.
            if item.status == .unknown {
                let remaining = min(self.preparedReplacement.readinessRemainingMs() ?? 0,
                                    self.preparedOverlapRemainingMs)
                let ready = await withTaskGroup(of: Bool.self) { group in
                    group.addTask { @MainActor in
                        for await event in observer.events {
                            guard case .status(let status) = event else { continue }
                            if status == .readyToPlay { return true }
                            if status == .failed { return false }
                        }
                        return false
                    }
                    group.addTask {
                        try? await Task.sleep(for: .milliseconds(remaining))
                        return false
                    }
                    defer { group.cancelAll() }
                    return await group.next() ?? false
                }
                guard !Task.isCancelled, self.preparedItem === item,
                      self.preparedPlayer === successor else { return }
                if !ready {
                    self.preparedReplacement.abandon(.failed)
                    return
                }
            }
            while !Task.isCancelled {
                self.restorePreparedIncumbentBufferPreferenceIfOwnerMoved()
                guard self.preparedItem === item,
                      self.preparedPlayer === successor else { return }
                // A boundary preparation whose owner moved on is withdrawn,
                // not failed.
                if let boundary = self.autoBoundaryAttempt, !self.autoBoundaryIsCurrent(boundary) {
                    self.preparedReplacement.abandonWithoutFallback(.aborted)
                    return
                }
                if self.autoPreparing && self.autoVoluntary {
                    let now = PlaybackControlSession.monotonicMs()
                    let current = self.measuredCostCatalog(self.decision?.qualityCandidates ?? [])
                        .first { $0.id == self.autoActiveCandidateId }
                    let pressure = current.map { autoActiveProductionPressure(status: self.sessionStatus,
                        observedAtMs: self.sessionStatusAgeMs.map { now - $0 }, nowMs: now,
                        sessionId: self.sessionId, candidateId: $0.id,
                        runwaySeconds: self.bufferedRunwaySeconds()) } ?? true
                    var linkPressure = false
                    if let current, let sample = self.latestAutoCompletedTransfer,
                       sample.installedSessionId == self.sessionId, sample.installedCandidateId == current.id,
                       sample.statusCode == 200, sample.receipt != nil, sample.etag != nil,
                       self.autoTransferOriginCurrent(sample),
                       let link = autoCompletedTransferBps(sample, nowMs: now, maximumAgeMs: 15_000),
                       let cost = autoDownsideCostBps(current, sample: sample, sessionId: self.sessionId, nowMs: now) {
                        linkPressure = link < cost * (current.peakBps == nil ? 1.0 : 1.3)
                        if link < cost * 0.7 { self.autoUpgradeEvidence.cliff(completedAtMs: sample.completedAtMs, nowMs: now) }
                    }
                    if self.autoStagedObservation.map({ !self.autoStagedObservationCurrent($0) }) ?? true ||
                        (self.bufferedRunwaySeconds() ?? 0) < 10 || pressure || linkPressure ||
                        (self.autoBoundaryAttempt.map(self.autoBoundaryIsCurrent) != true &&
                            !self.autoUpgradeEvidence.allowsUpgrade(nowMs: now)) {
                        self.preparedReplacement.abandonWithoutFallback(.aborted)
                        return
                    }
                }
                if item.status == .failed {
                    self.preparedReplacement.abandon(.failed)
                    return
                }
                if self.preparedReplacement.readinessBoundElapsed() || self.preparedOverlapRemainingMs == 0 {
                    self.preparedReplacement.abandon(.failed)
                    return
                }
                // `.readyToPlay` is AVFoundation saying the item is playable
                // and its tracks and duration are known — the whole of what
                // `metadata_ready` claims.
                if item.status == .readyToPlay {
                    self.preparedReplacement.successorIsMetadataReady()
                    // The seek the successor owes, issued exactly once and
                    // only now that the item can honour it. Runway measured
                    // before it lands is runway from the successor's own zero,
                    // which is a different point in the film.
                    if let seekMs = self.preparedSeekMs {
                        // Cleared before the await, not after: this loop runs
                        // every 100 ms and a flag set on the far side of a
                        // suspension would issue the same seek repeatedly.
                        // Awaited rather than given a completion handler
                        // because AVFoundation calls those on a queue of its
                        // choosing, and asserting main-actor isolation inside
                        // one is what killed every play on build 90.
                        self.preparedSeekMs = nil
                        let aligned = await self.awaitPreparedAlignment(of: item, to: seekMs, policy: .initialParking)
                        guard !Task.isCancelled, self.preparedItem === item,
                              self.preparedPlayer === successor else { return }
                        if !aligned {
                            self.preparedReplacement.abandon(.failed)
                            return
                        }
                        continue
                    } else {
                        if let action = self.preparedReplacement.activeAction {
                            self.prepareCadenceIfNeeded(item, successor: successor, action: action)
                        }
                        if self.preparedCadenceFailed {
                            self.preparedReplacement.abandon(.failed)
                            return
                        }
                        if let throughMs = self.preparedBufferedThroughMs(item),
                              throughMs >= self.preparedAcquisitionFilmPositionMs
                                  + PreparedReplacementBounds.switchRunwayMs,
                              self.preparedAcquisitionTargetHasRunway(item),
                              self.preparedReplacement.activeAction.map({
                                  self.autoStagedProductionAllowsCommit($0) && self.autoStagedOriginalAllowsCommit($0) &&
                                      self.preparedLoadedRendezvous(item, action: $0, rate: self.preferredRate) != nil
                              }) == true {
                            self.preparedReplacement.successorIsBuffered(throughMs: throughMs)
                            await self.preparedReplacement.commit()
                            return
                        }
                    }
                }
                let delayMs = min(
                    PreparedReplacementBounds.pollMs,
                    self.preparedReplacement.readinessRemainingMs() ?? PreparedReplacementBounds.pollMs
                )
                if delayMs > 0 {
                    try? await Task.sleep(nanoseconds: UInt64(delayMs) * 1_000_000)
                }
            }
        }
    }

    /// How far the successor is buffered, in film time.
    ///
    /// Its loaded ranges are in its own item time, and its zero is the film
    /// position its `media_origin_ms` names — which is exactly why the
    /// protocol carries that field.
    private func preparedBufferedThroughMs(_ item: AVPlayerItem) -> Int? {
        guard let action = preparedReplacement.activeAction else { return nil }
        let playhead = item.currentTime().seconds
        guard playhead.isFinite else { return nil }
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds
            let end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, end >= start else { return nil }
            return start...end
        }
        let runway = Self.bufferedRunwaySeconds(playheadSeconds: playhead, ranges: ranges)
        guard runway > 0 else { return nil }
        return action.mediaOriginMs + Int(((playhead + runway) * 1_000).rounded())
    }

    /// Loaded media must cover the acquisition target, not merely the earlier
    /// coarse landing. A later island or any positive gap cannot prove runway.
    private func preparedAcquisitionTargetHasRunway(_ item: AVPlayerItem) -> Bool {
        guard let action = preparedReplacement.activeAction else { return false }
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds, end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, start >= 0, end >= start else { return nil }
            return start...end
        }
        return PreparedSeekPolicy.originalTargetHasRunway(
            filmPositionMs: preparedAcquisitionFilmPositionMs, mediaOriginMs: action.mediaOriginMs,
            ranges: ranges)
    }

    /// Readiness and commit admit the same loaded future point. The incumbent
    /// keeps moving while the successor is paused, so acquisition-target runway
    /// alone cannot prove a handoff the commit can still reach.
    private func preparedLoadedRendezvous(_ item: AVPlayerItem,
                                         action: PreparedReplacementAction,
                                         rate: Float, cadence: PreparedDecodedCadence? = nil) -> PreparedCommitRendezvous? {
        let seconds = player.currentTime().seconds
        guard rate.isFinite, rate > 0, seconds.isFinite, seconds >= 0, baseMs >= 0,
              seconds * 1_000 + Double(baseMs) < Double(Int.max) else { return nil }
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds, end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, start >= 0, end >= start else { return nil }
            return start...end
        }
        return preparedRendezvousWithCadence(
            stagedFilmPositionMs: preparedAcquisitionFilmPositionMs,
            incumbentFilmPositionMs: realPositionMs(), mediaOriginMs: action.mediaOriginMs,
            playbackRate: Double(rate), overlapRemainingMs: preparedOverlapRemainingMs,
            loadedItemRanges: ranges, cadence: cadence)
    }

    private enum PreparedIncumbentBufferRefusal: String {
        case playbackNotRequested = "playback_not_requested"
        case itemMissing = "item_missing"
        case runwayUnknown = "runway_unknown"
        case policyIneligible = "policy_ineligible"
    }

    private func notePreparedIncumbentBufferRefusal(
        _ reason: PreparedIncumbentBufferRefusal, runway: Double? = nil, original: Double? = nil
    ) {
        func fact(_ value: Double?) -> String {
            guard let value, value.isFinite else { return "unknown" }
            return String(value)
        }
        noteSurfaceLogOnly("prepared_incumbent_buffer:phase=refuse:reason=\(reason.rawValue):runway_seconds=\(fact(runway)):original_seconds=\(fact(original)):rate=\(preferredRate)")
    }

    private func acquirePreparedIncumbentBufferPreference(_ action: PreparedReplacementAction) {
        restorePreparedIncumbentBufferPreference()
        guard started else { return }
        guard wantsPlayback else { notePreparedIncumbentBufferRefusal(.playbackNotRequested); return }
        guard let item = player.currentItem else { notePreparedIncumbentBufferRefusal(.itemMissing); return }
        // Membership tolerates clock rounding; a physical lease cannot bridge gaps.
        guard let runway = bufferedRunwaySeconds(gapTolerance: 0) else {
            notePreparedIncumbentBufferRefusal(.runwayUnknown, original: item.preferredForwardBufferDuration)
            return
        }
        guard let policy = PreparedIncumbentBufferPreference(
            original: item.preferredForwardBufferDuration,
            actualRunwaySeconds: runway, playbackRate: Double(preferredRate)) else {
            notePreparedIncumbentBufferRefusal(.policyIneligible, runway: runway,
                original: item.preferredForwardBufferDuration)
            return
        }
        preparedIncumbentBufferLease = (item, player, viewerActionEpoch, action.actionId, policy)
        item.preferredForwardBufferDuration = policy.hint
        noteSurfaceLogOnly("prepared_incumbent_buffer:phase=acquire:original_seconds=\(policy.original):hint_seconds=\(policy.hint):runway_seconds=\(runway):rate=\(preferredRate):overlap_ms=\(preparedOverlapRemainingMs)")
    }

    private func restorePreparedIncumbentBufferPreference() {
        guard let lease = preparedIncumbentBufferLease else { return }
        preparedIncumbentBufferLease = nil
        let restore = lease.policy.restorationPreference(currentHint: lease.item.preferredForwardBufferDuration)
        if let restore { lease.item.preferredForwardBufferDuration = restore }
        noteSurfaceLogOnly("prepared_incumbent_buffer:phase=restore:restored=\(restore != nil):original_seconds=\(lease.policy.original):hint_seconds=\(lease.policy.hint):overlap_ms=\(preparedOverlapRemainingMs)")
    }

    private func restorePreparedIncumbentBufferPreferenceIfOwnerMoved() {
        guard let lease = preparedIncumbentBufferLease else { return }
        if player !== lease.player || player.currentItem !== lease.item || viewerActionEpoch != lease.epoch ||
            preparedReplacement.activeAction?.actionId != lease.actionId {
            restorePreparedIncumbentBufferPreference()
        }
    }

    private func clearPreparedCadence() {
        preparedCadenceGeneration &+= 1
        preparedCadenceTask?.cancel()
        preparedCadenceTask = nil
        preparedCadenceProof = nil
        preparedCadenceOwner = nil
        preparedCadenceFailed = false
    }

    /// Prepare one item-owned cadence proof while loaded media arrives. A
    /// completed proof is immutable; an adjacent fragment may need a new one.
    /// The probe reads only this original-target interval's already loaded
    /// media and shares the preparation's existing deadline.
    private func prepareCadenceIfNeeded(_ item: AVPlayerItem, successor: AVPlayer,
                                       action: PreparedReplacementAction) {
        if let owner = preparedCadenceOwner,
           owner.item != ObjectIdentifier(item) || owner.player != ObjectIdentifier(successor) ||
               owner.epoch != viewerActionEpoch || owner.action != action.actionId {
            clearPreparedCadence()
        }
        guard preparedCadenceTask == nil, !preparedCadenceFailed,
              preparedItem === item, preparedPlayer === successor,
              preferredRate.isFinite, preferredRate > 0,
              player.currentTime().seconds.isFinite else { return }
        if let proof = preparedCadenceProof,
           preparedLoadedRendezvous(item, action: action, rate: preferredRate, cadence: proof) != nil { return }
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds, end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, start >= 0, end >= start else { return nil }
            return start...end
        }
        guard let probeMs = preparedCadenceProbeItemMs(
            stagedFilmPositionMs: preparedAcquisitionFilmPositionMs, incumbentFilmPositionMs: realPositionMs(),
            mediaOriginMs: action.mediaOriginMs, playbackRate: Double(preferredRate),
            overlapRemainingMs: preparedOverlapRemainingMs, loadedItemRanges: ranges),
              let opened = preparedReplacement.openedAtMs else { return }
        let probe = Double(probeMs) / 1_000
        if let proof = preparedCadenceProof,
           proof.itemInterval == nil || proof.itemInterval?.contains(probe) == true { return }
        let remaining = min(PreparedReplacementBounds.alignmentMs, preparedOverlapRemainingMs,
            opened + PreparedReplacementBounds.readinessMs - PlaybackControlSession.monotonicMs())
        guard remaining > 0 else { return }
        let generation = preparedCadenceGeneration
        let epoch = viewerActionEpoch
        preparedCadenceOwner = (ObjectIdentifier(item), ObjectIdentifier(successor), epoch, action.actionId)
        preparedCadenceTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer {
                if self.preparedCadenceGeneration == generation { self.preparedCadenceTask = nil }
            }
            let proof = await preparedDecodedFrameDuration(of: item, boundMs: remaining,
                targetItemSeconds: probe) { [weak self] detail in
                    self?.noteSurfaceLogOnly("prepared_cadence:\(detail)")
                }
            guard !Task.isCancelled, self.preparedCadenceGeneration == generation,
                  self.preparedItem === item, self.preparedPlayer === successor,
                  self.viewerActionEpoch == epoch,
                  self.preparedReplacement.activeAction?.actionId == action.actionId else { return }
            self.preparedCadenceProof = proof
            self.preparedCadenceFailed = proof == nil
        }
    }

    /// Promote the primed player and its retained layer after item-specific
    /// decoded alignment. The item and decoder output stay bound throughout;
    /// physical display and audio qualification remains a separate observation.
    func commitPreparedSuccessor(
        _ action: PreparedReplacementAction
    ) async -> PreparedCommitOutcome {
        let automaticTrial = autoPreparing
        // Switching changes the coordinator's phase, not the original opening
        // timestamp. Capture its existing readiness deadline before any await;
        // layer and selection waits must never restart that clock.
        guard let openedAtMs = preparedReplacement.openedAtMs else { return .refused }
        let readinessDeadlineMs = openedAtMs + PreparedReplacementBounds.readinessMs
        // The viewer action this commit began under. Every step below, and the
        // rendezvous and decoded-alignment waits it hands this to, asks
        // `preparedCommitStillOwned` rather than comparing the epoch by hand.
        let commitAttempt = snapshotAttempt()
        // Playlist type proves delivery, not the film origin. Only the
        // current stage's explicit zero origin permits film-local mapping.
        let stagedFilmLocalVOD = autoStagedObservation.map {
            autoStagedObservationCurrent($0) && $0.intervals != nil && action.mediaOriginMs == 0
        } ?? false
        if automaticTrial && !autoTrialAllowsExposure(action) { return preparedCommitFailure(at: "automatic-exposure") }
        guard autoStagedProductionAllowsCommit(action) else { return .refused }
        guard autoStagedOriginalAllowsCommit(action) else { return .refused }
        guard started,
              let successor = preparedPlayer,
              let item = preparedItem,
              let overlapStartedAtMs = preparedOverlapStartedAtMs,
              preparedOverlapRemainingMs > 0,
              player.currentItem != nil,
              // A paused viewer produces no new frame, ever: the item's output
              // clock does not advance at rate zero, so a switch made while
              // paused could only ever time out — and timing out after the
              // swap means telling the server to abort a session the viewer is
              // looking at. A failed optional change retains the healthy
              // paused incumbent until Retry or explicit Apply with restart.
              wantsPlayback,
              !pictureInPictureIsActive, !player.isExternalPlaybackActive,
              playbackSurface != nil,
              !isChangingStream
        else { return .refused }
        // Dropped rather than cancelled: this runs *inside* the readiness
        // monitor's own task, and cancelling it here would make every
        // `Task.isCancelled` check below — including the first-frame wait —
        // answer true immediately and turn every commit into a `failed`. The
        // monitor returns as soon as this call does.
        preparedMonitor = nil
        preparedLifecycle.removeAll()
        // Prepare a future film instant on the muted successor's own layer.
        // The incumbent remains visible and audible while its seek lands and
        // its decoded output is checked, then advances to that same instant.
        // All waits consume the pipeline's original physical overlap budget.
        // The coordinator records a failed outcome before disposing this
        // staging. Early disposal here erases the item's typed error and
        // original overlap budget before that failure journal can read them.
        let layerReady = await awaitBoundedValue(
            boundMs: max(0, min(PreparedReplacementBounds.alignmentMs, preparedOverlapRemainingMs,
                readinessDeadlineMs - PlaybackControlSession.monotonicMs())),
            pollMs: PreparedReplacementBounds.pollMs,
            now: { Int(ProcessInfo.processInfo.systemUptime * 1_000) },
            sleep: { try? await Task.sleep(nanoseconds: UInt64($0) * 1_000_000) },
            read: { [weak self] in
                guard let self, self.preparedCommitStillOwned(commitAttempt),
                      self.preparedPlayer === successor, self.wantsPlayback,
                      !self.pictureInPictureIsActive, !self.player.isExternalPlaybackActive else { return false }
                return self.playbackSurface?.canPromote(successor) == true ? true : nil
            }
        )
        guard layerReady == true, preparedPlayer === successor,
              preparedCommitStillOwned(commitAttempt), wantsPlayback else {
            return preparedCommitFailure(at: "surface")
        }
        guard await reconcilePreparedMediaSelections(to: item, successor: successor,
                commit: commitAttempt, overlapStartedAtMs: overlapStartedAtMs,
                readinessDeadlineMs: readinessDeadlineMs) else {
            return preparedCommitFailure(at: "media-selection")
        }
        let rendezvousRate = preferredRate
        // Cadence preparation can consume its existing bounded allowance
        // while readiness loads media. Choose the final future instant only
        // when that measured interval and the actual loaded runway agree.
        // Keep two seconds of strict runway and the original post-swap reserve.
        // Selection is complete; no target moves after the final choice.
        let incumbentSeconds = player.currentTime().seconds
        guard rendezvousRate.isFinite, rendezvousRate > 0,
              incumbentSeconds.isFinite, incumbentSeconds >= 0, baseMs >= 0,
              incumbentSeconds * 1_000 + Double(baseMs) < Double(Int.max) else {
            return preparedCommitFailure(at: "rate-or-overlap")
        }
        // Layer readiness and media selection may consume the lead that
        // admitted readiness. Await actual expansion within the original
        // deadline, then freeze one target with interval-valid cadence before
        // exact alignment.
        var loadedRendezvous: PreparedCommitRendezvous?
        var loadedCadence: PreparedDecodedCadence?
        while PlaybackControlSession.monotonicMs() < readinessDeadlineMs,
              preparedOverlapRemainingMs > 0 {
            guard !Task.isCancelled, preparedCommitStillOwned(commitAttempt),
                  preparedItem === item, preparedPlayer === successor,
                  started, player.currentItem != nil, playbackSurface != nil, !isChangingStream,
                  wantsPlayback, preferredRate == rendezvousRate,
                  !pictureInPictureIsActive, !player.isExternalPlaybackActive,
                  item.status == .readyToPlay,
                  autoStagedProductionAllowsCommit(action), autoStagedOriginalAllowsCommit(action)
            else { return .refused }
            prepareCadenceIfNeeded(item, successor: successor, action: action)
            if preparedCadenceFailed { return preparedCommitFailure(at: "frame-duration") }
            if let proof = preparedCadenceProof,
               let plan = preparedLoadedRendezvous(item, action: action, rate: rendezvousRate, cadence: proof) {
                loadedCadence = proof
                loadedRendezvous = plan
                break
            }
            let remainingMs = min(readinessDeadlineMs - PlaybackControlSession.monotonicMs(),
                                  preparedOverlapRemainingMs)
            guard remainingMs > 0 else { return .refused }
            try? await Task.sleep(nanoseconds: UInt64(min(PreparedReplacementBounds.pollMs, remainingMs)) * 1_000_000)
        }
        guard let rendezvous = loadedRendezvous, let cadence = loadedCadence else { return .refused }
        // Cadence was prepared before this single frozen target. No track or
        // media inspection can consume its lead after this point.
        let frameDurationSeconds = cadence.frameDurationSeconds
        guard cadence.covers(itemPositionMs: rendezvous.itemPositionMs) else {
            return preparedCommitFailure(at: "cadence-interval")
        }
        // Bounded, because an unbounded one does not degrade the way it looks
        // as though it would. The incumbent does keep playing — but the
        // readiness monitor has already been dropped two statements above, so
        // nothing can cancel a suspended commit; `.switching` makes `abandon`,
        // `abandonWithoutFallback` and `offer` all bail, so the coordinator
        // never frees and every later quality change in the session skips the
        // prepared path; the server holds its one preparation slot for the full
        // deadline because no acknowledgement is queued in `.switching`; and
        // the viewer's tap produces nothing at all, because the prepared path
        // already claimed it and suppressed the in-place reopen.
        guard await awaitPreparedAlignment(of: item, to: rendezvous.itemPositionMs, policy: .exactAlignment) else {
            return automaticTrial ? preparedCommitFailure(at: "seek") : PreparedCommitRendezvous.outcomeWhenAlignmentCannotLand
        }
        guard await awaitPreparedPreroll(item: item, successor: successor,
                rate: rendezvousRate, commit: commitAttempt) else {
            return preparedCommitFailure(at: "preroll")
        }
        guard let alignedOutput = preparedVideoOutput,
              await awaitPreparedDecodedAlignment(
                item: item, successor: successor, output: alignedOutput,
                rendezvous: rendezvous, frameDurationSeconds: frameDurationSeconds, commit: commitAttempt
              ) else {
            return preparedCommitFailure(at: "decoded-alignment")
        }

        let originalAutomaticWaiting = successor.automaticallyWaitsToMinimizeStalling
        var scheduledHiddenClock = false
        defer {
            successor.automaticallyWaitsToMinimizeStalling = originalAutomaticWaiting
            if scheduledHiddenClock && player !== successor { successor.pause() }
        }
        guard preparedCommitStillOwned(commitAttempt), preparedItem === item,
              preparedPlayer === successor, successor.currentItem === item,
              wantsPlayback, preferredRate == rendezvousRate,
              successor.status == .readyToPlay, successor.rate == 0, successor.isMuted,
              player.rate == rendezvousRate, player.timeControlStatus == .playing else {
            return preparedCommitFailure(at: "clock-ownership")
        }
        guard let anchor = await awaitPreparedClockRunway(
            item: item, successor: successor, rendezvous: rendezvous,
            frameDurationSeconds: frameDurationSeconds, rate: rendezvousRate,
            commit: commitAttempt
        ) else {
            return preparedCommitFailure(at: "clock-anchor")
        }
        // Apple requires this policy only for the precise host-clock API.
        // The captured successor's original policy is restored after proof,
        // rollback or pre-exposure refusal; ordinary playback is unchanged.
        successor.automaticallyWaitsToMinimizeStalling = false
        successor.setRate(rendezvousRate,
            time: CMTime(value: CMTimeValue(rendezvous.itemPositionMs), timescale: 1_000),
            atHostTime: CMTime(seconds: anchor.hostSeconds, preferredTimescale: 1_000_000_000))
        scheduledHiddenClock = true

        guard await awaitPreparedRendezvous(
            item: item, successor: successor, rendezvous: rendezvous,
            frameDurationSeconds: frameDurationSeconds, rate: rendezvousRate,
            anchor: anchor, commit: commitAttempt
        ) else {
            return preparedCommitFailure(at: "rendezvous", deltaMs: realPositionMs() - rendezvous.filmPositionMs)
        }

        // The staging can be taken away under that await — the player ending,
        // the app backgrounding. `.switching` stops anything else *opening*
        // one, but it does not stop the pipeline being freed, and handing a
        // released item to the incumbent would be worse than refusing.
        guard preparedCommitStillOwned(commitAttempt), wantsPlayback else { return preparedCommitFailure(at: "ownership") }
        guard preparedItem === item, preparedPlayer === successor,
              started, player.currentItem != nil,
              !automaticTrial || autoTrialAllowsExposure(action)
        else {
            return automaticTrial ? preparedCommitFailure(at: "staging") : .refused
        }
        guard Double(abs(realPositionMs() - rendezvous.filmPositionMs)) <= frameDurationSeconds * 1_000 else {
            return preparedCommitFailure(at: "boundary", deltaMs: realPositionMs() - rendezvous.filmPositionMs)
        }
        restorePreparedIncumbentBufferPreference()
        let incumbentPlayer = player
        let incumbentVolume = incumbentPlayer.volume
        let incumbentMuted = incumbentPlayer.isMuted
        let exposureRecipeRevision = recipeRevision.desired
        clearPreparedCadence()
        preparedPlayer = nil
        preparedItem = nil
        preparedVideoOutput = nil
        preparedOverlapStartedAtMs = nil
        warmPredecessor = incumbentPlayer
        stagedSurfacePlayer = incumbentPlayer
        defer {
            if warmPredecessor === incumbentPlayer {
                if incumbentPlayer !== player {
                    incumbentPlayer.pause()
                    incumbentPlayer.replaceCurrentItem(with: nil)
                }
                warmPredecessor = nil
                stagedSurfacePlayer = nil
            }
            if successor !== player {
                successor.pause()
                successor.replaceCurrentItem(with: nil)
            }
        }
        let incumbentHealthy = hasHealthyQualityIncumbent
        // Player ownership is moving, so every monitor
        // that would otherwise read a not-yet-ready item as a stall has to
        // know a change is in flight — the same flag, for the same window,
        // that `open()` holds across its own attach. Without it the end-of-
        // item handler, the stall recovery and `positionForPlaybackIntent`
        // all act on a stream that is mid-switch.
        isChangingStream = true
        defer { isChangingStream = false }

        // The film position the switch happens at — the position the successor
        // was just aligned to, not a fresh reading of a clock that has moved
        // on since. A first frame whose own display time falls before this
        // boundary is a pixel buffer that predates the switch, and reporting
        // it as `first_frame_unix_ms` would be worse than reporting nothing:
        // it is the instrument that could not separate a codec change from no
        // change. Taking it from the alignment is what makes the successor's
        // own first frame able to satisfy it at all.
        let boundaryMs = rendezvous.filmPositionMs
        let incumbent = player.currentItem
        let incumbentState = (sessionId: sessionId, candidateId: autoActiveCandidateId,
            baseMs: baseMs, path: activeMediaPath,
            authenticated: activeMediaAuthenticated, height: sessionHeight, direct: isDirectPlayback,
            vod: isVOD, directTimeline: usesDirectTimeline,
            status: sessionStatus, diagnostic: diagnosticSessionStatus, observedAt: diagnosticSessionStatusObservedAt)
        let exposureAttempt = snapshotAttempt()
        sampleThePreparedSwitch()
        guard playbackSurface?.promote(successor) == true else { return .failedWithoutReopen }
        incumbentPlayer.isMuted = true
        unprovenPreparedItem = item
        effectiveProcessing = nil
        effectiveProcessingGeneration = nil
        // The successor's own answer, as its prepare action carries it (nil
        // when the server sent none). A report-gated one still stays hidden
        // until a current effective-processing report is held.
        playbackEnhancement = action.enhancement
        adoptWarmPlayer(successor)
        player.volume = incumbentVolume
        player.isMuted = incumbentMuted
        stopStatusPolling()
        installSeekVideoOutput(on: item, retaining: alignedOutput)
        #if os(iOS)
        item.externalMetadata = [titleMetadata(title)]
        #endif
        retireItemObserver()
        pgsOverlayItemGeneration &+= 1
        pgsOverlayWindowTask?.cancel()
        pgsOverlayLoadingRange = nil
        pgsOverlayWindowFailures = 0
        pgsOverlayWindow = nil
        stallObservation.reset()
        preparedSwitch.note(commitAtMs: PlaybackControlSession.monotonicMs())
        // The successor is a full session in every respect but its pointer, so
        // everything keyed on "which session am I playing" moves with it.
        if autoPreparing { autoExposed = true }
        sessionId = action.sessionId
        autoActiveCandidateId = action.effectiveSelection.candidateId
        baseMs = action.mediaOriginMs
        progressiveCaptions.mediaChanged()
        isVOD = stagedFilmLocalVOD
        usesDirectTimeline = stagedFilmLocalVOD
        activeMediaPath = clusterRelativeMediaPath(action.playlistUrl)
        activeMediaAuthenticated = false
        if action.effectiveSelection.height > 0 {
            sessionHeight = action.effectiveSelection.height
        }
        isDirectPlayback = false
        // Metrics capture their installed session/candidate once. Bind every
        // successor, including manual-to-Auto, before installing observation;
        // a voluntary proposal is not required for a truthful item identity.
        installItemObserver(for: item)
        // Selections were resolved and applied on the hidden successor before
        // alignment. Exposing its layer must not introduce an async pause.
        applyDisplayCriteria(for: item, generation: openGeneration)
        startRecoveryEvidencePoll()
        // The hidden successor is already running on the shared host clock.
        // Resetting its rate here would reintroduce startup clock settlement.
        if !wantsPlayback { player.pause() }
        isPlaying = wantsPlayback
        refreshPGSOverlayWindow(at: boundaryMs, reason: .force)
        ttffMeasurement.rebasePosition(at: realPositionMs())
        let exposedAtUnixMs = Int(Date().timeIntervalSince1970 * 1_000)
        let firstFrameUnixMs = await awaitPreparedFirstFrame(boundaryMs: boundaryMs,
            overlapStartedAtMs: overlapStartedAtMs, frameDurationSeconds: frameDurationSeconds)
        // Do not DELETE the predecessor here. The committed control exchange
        // is the compare-and-swap that makes this successor authoritative and
        // starts the predecessor's bounded drain. Ending it first removes the
        // exact route that CAS is bound to, so even a visibly successful
        // switch could never settle durably.
        guard let firstFrameUnixMs else {
            guard automaticTrial || incumbentHealthy else { return .switchedWithoutAFrame }
            // Keep the exact incumbent item alive until the new picture is proved.
            // A newer user/lifecycle owner must never be overwritten by rollback.
            if let incumbent, started,
               attemptStillCurrent(exposureAttempt, fence: .preparedPipelineRollback),
               recipeRevision.desired == exposureRecipeRevision,
               player.currentItem === item,
               playbackSurface?.promote(incumbentPlayer) == true {
                stopStatusPolling()
                retireItemObserver()
                let latestVolume = player.volume
                let latestMuted = player.isMuted
                player.isMuted = true
                adoptWarmPlayer(incumbentPlayer)
                player.volume = latestVolume
                player.isMuted = latestMuted
                unprovenPreparedItem = nil
                sessionId = incumbentState.sessionId
                autoActiveCandidateId = incumbentState.candidateId
                baseMs = incumbentState.baseMs
                progressiveCaptions.mediaChanged()
                activeMediaPath = incumbentState.path
                activeMediaAuthenticated = incumbentState.authenticated
                sessionHeight = incumbentState.height
                isDirectPlayback = incumbentState.direct
                isVOD = incumbentState.vod
                usesDirectTimeline = incumbentState.directTimeline
                sessionStatus = incumbentState.status
                diagnosticSessionStatus = incumbentState.diagnostic
                diagnosticSessionStatusObservedAt = incumbentState.observedAt
                installSeekVideoOutput(on: incumbent)
                installItemObserver(for: incumbent)
                applyDisplayCriteria(for: incumbent, generation: openGeneration)
                startRecoveryEvidencePoll()
                if wantsPlayback { player.playImmediately(atRate: preferredRate) } else { player.pause() }
                isPlaying = wantsPlayback
                currentMs = realPositionMs()
                refreshPGSOverlayWindow(at: currentMs, reason: .force)
            }
            autoExposed = false
            return .failedWithoutReopen
        }
        if let requested = autoDesiredCandidate, action.effectiveSelection.candidateId == requested.id {
            let now = PlaybackControlSession.monotonicMs()
            autoActiveCandidateId = requested.id
            // Observation is reinstalled only after the existing owner accepted
            // the successor's actual candidate identity and picture.
            installAutoTransferMetrics(for: item)
            autoLastSwitchMs = now
            if autoVoluntary { autoSwitchTimes.append(now) }
            autoPreparing = false
            autoUpgradeSinceMs = nil
            autoExposed = false
        }
        // A boundary preparation ends with its own first frame, exactly like
        // the mid-play upgrade it now is.
        autoBoundaryAttempt = nil
        Caps.PreparedHandoffTelemetry.shared.note(
            outcome: "committed \(max(0, firstFrameUnixMs - exposedAtUnixMs)) ms"
        )
        // M3. The successor's first reading and the frame the viewer saw. The
        // developer tab is not inside a playback session and so cannot ask this
        // controller; these are advisory and nothing reads them back.
        preparedSwitch.note(firstFrameUnixMs: firstFrameUnixMs)
        sampleThePreparedSwitch()
        Caps.PreparedHandoffTelemetry.shared.note(switch: preparedSwitch.reading())
        if player.currentItem === item {
            unprovenPreparedItem = nil
            player.allowsExternalPlayback = incumbentPlayer.allowsExternalPlayback
        }
        if !automaticTrial, preparedCommitStillOwned(commitAttempt) {
            if let pin = manualQualityRetention.finishCommitted(viewerEpoch: viewerActionEpoch, firstFrameUnixMs: firstFrameUnixMs),
               !pin.carryingSeek, seekState.generation == pin.seekGeneration {
                // The quality tap pinned a progress destination; it did not
                // ask to seek there. A frame-proved moving handoff settles
                // that pin, or the next tap still advertises a stale seek.
                seekState.clear()
                currentMs = realPositionMs()
            }
            manualQualityRetention.clear()
            qualityChangeRetained = false
            recipeRevision.didAttach(recipeRevision.desired)
        }
        if let bootstrap = action.control, let origin = playbackControlOrigin {
            pendingPreparedControl = (action.actionId, action.sessionId, bootstrap, origin)
        }
        return .committed(firstFrameUnixMs: firstFrameUnixMs)
    }

    /// Retain a finite checkpoint when AVFoundation reports no item error.
    /// Only fixed call-site labels and relative film-clock drift are logged.
    private func preparedCommitFailure(at checkpoint: String, deltaMs: Int? = nil) -> PreparedCommitOutcome {
        let drift = deltaMs.map(String.init) ?? "unknown"
        noteSurfaceLogOnly("prepared_commit_failed:checkpoint=\(checkpoint):delta_ms=\(drift)")
        return .failedWithoutReopen
    }

    /// Asset track loading may ignore task cancellation. Return at the original
    /// overlap deadline and fence any eventual selection from the cancelled task.
    private func reconcilePreparedMediaSelections(to item: AVPlayerItem, successor: AVPlayer,
            commit: Attempt, overlapStartedAtMs: Int, readinessDeadlineMs: Int) async -> Bool {
        let now = PlaybackControlSession.monotonicMs()
        let remaining = max(0, min(readinessDeadlineMs - now,
            PreparedReplacementBounds.overlapMs - max(0, now - overlapStartedAtMs)))
        guard remaining > 0 else { return false }
        var completed: Bool?
        let task = Task { @MainActor [weak self] in
            guard let self else { return }
            let ready = await self.prepareStagedMediaSelections(to: item) { [weak self] in
                guard let self else { return false }
                return self.started && self.preparedItem === item && self.preparedPlayer === successor
                    && self.preparedCommitStillOwned(commit) && self.wantsPlayback
            }
            if !Task.isCancelled { completed = ready }
        }
        let ready = await awaitBoundedValue(
            boundMs: remaining,
            pollMs: PreparedReplacementBounds.pollMs,
            now: { Int(ProcessInfo.processInfo.systemUptime * 1_000) },
            sleep: { try? await Task.sleep(nanoseconds: UInt64($0) * 1_000_000) },
            read: { completed }
        )
        task.cancel()
        return ready == true
    }

    /// Resolve tracks while the exact successor is still hidden. No active-item
    /// guard can stand in for staged ownership, and no async resolution belongs
    /// between exposing its paused layer and starting playback.
    func prepareStagedMediaSelections(to item: AVPlayerItem,
            isCurrent: @escaping @MainActor () -> Bool) async -> Bool {
        let actionEpoch = viewerActionEpoch
        let current = { !Task.isCancelled && isCurrent() && self.viewerActionEpoch == actionEpoch }
        guard current() else { return false }
        let fields = Self.sessionSubtitleFields(selected: selectedSubtitle, tracks: subtitles)
        let nativeSubtitle = pgsOverlayIsActive || activeBurnedSubtitle != nil ? nil : fields.native
        let audio = audioOverride == nil && prePlaySelection.audioIndex == nil
            ? await mediaSelectionPreparation.audio(audioLanguage, item) : nil
        guard current() else { return false }
        let native = await mediaSelectionPreparation.native(nativeSubtitle, subtitles, item)
        guard current() else { return false }
        audio?()
        return native.apply()
    }

    /// Put the successor's playhead on the rendezvous, and give up on the seek
    /// if it does not come back inside `PreparedReplacementBounds.alignmentMs`.
    ///
    /// The seek runs in a task of its own rather than being awaited directly,
    /// so that giving up on it is possible at all: `AVPlayerItem.seek` does not
    /// observe task cancellation, and the readiness monitor that would
    /// otherwise own this work was dropped before the commit began. The
    /// abandoned seek is left to resolve or not; the generation check is what
    /// stops it reporting into a commit that has moved on.
    private func awaitPreparedAlignment(of item: AVPlayerItem, to itemMs: Int, policy: PreparedSeekPolicy = .exactAlignment) async -> Bool {
        let remaining = min(PreparedReplacementBounds.alignmentMs, preparedOverlapRemainingMs)
        guard remaining > 0 else { return false }
        preparedAlignmentGeneration &+= 1
        let generation = preparedAlignmentGeneration
        preparedAlignmentOutcome = nil
        let seek = Task { @MainActor [weak self] in
            let landed = await item.seek(
                to: CMTime(value: CMTimeValue(max(0, itemMs)), timescale: 1_000),
                toleranceBefore: CMTime(value: CMTimeValue(policy.toleranceBeforeMs), timescale: 1_000),
                toleranceAfter: CMTime(value: CMTimeValue(policy.toleranceAfterMs), timescale: 1_000)
            )
            guard let self, self.preparedAlignmentGeneration == generation else { return }
            self.preparedAlignmentOutcome = landed
        }
        let landed = await awaitBoundedValue(
            boundMs: remaining,
            pollMs: PreparedReplacementBounds.pollMs,
            now: { Int(ProcessInfo.processInfo.systemUptime * 1_000) },
            sleep: { try? await Task.sleep(nanoseconds: UInt64($0) * 1_000_000) },
            read: { [weak self] in self?.preparedAlignmentOutcome }
        )
        // Best effort, and expected to do nothing to the seek itself; it stops
        // the wrapper task rather than the media operation inside it.
        seek.cancel()
        if landed == nil { item.cancelPendingSeeks() }
        return landed ?? false
    }

    /// Prime the aligned paused successor without exposing it or granting a
    /// new overlap budget. A successful callback belongs only to this item,
    /// player, viewer action and rate; native cancellation releases late callbacks.
    private func awaitPreparedPreroll(
        item: AVPlayerItem, successor: AVPlayer, rate: Float, commit: Attempt
    ) async -> Bool {
        let remaining = min(PreparedReplacementBounds.alignmentMs, preparedOverlapRemainingMs)
        let beganAtMs = Int(ProcessInfo.processInfo.systemUptime * 1_000)
        let ownsPreroll = { [weak self] in
            guard let self else { return false }
            return self.preparedItem === item && self.preparedPlayer === successor
                && successor.currentItem === item && self.preparedCommitStillOwned(commit)
                && self.wantsPlayback && self.preferredRate == rate
                && self.preparedOverlapRemainingMs > 0
                && UIApplication.shared.applicationState == .active
                && !self.pictureInPictureIsActive && !self.player.isExternalPlaybackActive
                && item.status == .readyToPlay && successor.status == .readyToPlay
                && successor.rate == 0
        }
        guard remaining > 0, rate.isFinite, rate > 0, ownsPreroll() else { return false }
        let result = await PlayerSeekCompletion.runOwned(
            operation: {
                await withCheckedContinuation { continuation in
                    successor.preroll(atRate: rate) { finished in
                        continuation.resume(returning: finished)
                    }
                }
            },
            cancelOperation: { successor.cancelPendingPrerolls() },
            ownsOperation: ownsPreroll,
            waitForDeadline: {
                while ownsPreroll(), !Task.isCancelled {
                    let elapsed = Int(ProcessInfo.processInfo.systemUptime * 1_000) - beganAtMs
                    guard elapsed < remaining else { return }
                    try await Task.sleep(for: .milliseconds(min(
                        PreparedReplacementBounds.pollMs, remaining - elapsed
                    )))
                }
            }
        )
        return result == .finished(true) && ownsPreroll()
    }

    /// Whether the prepared commit captured as `commit` still belongs to the
    /// viewer action it began under. The commit is one continuation with
    /// several suspensions — the layer, frame-duration, decoded-alignment and
    /// rendezvous waits — and every one of them asks this, so the scope set
    /// is named once (`AttemptFence.preparedCommit`) and a refusal raises
    /// `attempt_stale` like any other migrated fence.
    private func preparedCommitStillOwned(_ commit: Attempt) -> Bool {
        restorePreparedIncumbentBufferPreferenceIfOwnerMoved()
        return attemptStillCurrent(commit, fence: .preparedCommit)
    }

    /// Seeking the future rendezvous may load a segment that starts ahead
    /// of the incumbent. Keep the incumbent visible until its clock enters
    /// that contiguous range, without changing the target or overlap deadline.
    private func awaitPreparedClockRunway(
        item: AVPlayerItem, successor: AVPlayer, rendezvous: PreparedCommitRendezvous,
        frameDurationSeconds: Double, rate: Float, commit: Attempt
    ) async -> PreparedCommitRendezvous.ClockAnchor? {
        while preparedOverlapRemainingMs > 0 {
            guard !Task.isCancelled, preparedCommitStillOwned(commit),
                  preparedItem === item, preparedPlayer === successor,
                  successor.currentItem === item, wantsPlayback, preferredRate == rate,
                  UIApplication.shared.applicationState == .active,
                  !pictureInPictureIsActive, !player.isExternalPlaybackActive,
                  item.status == .readyToPlay, successor.status == .readyToPlay,
                  successor.rate == 0, successor.isMuted,
                  player.rate == rate, player.timeControlStatus == .playing else { return nil }
            let hostNow = CMClockGetTime(CMClockGetHostTimeClock())
            guard hostNow.isValid, hostNow.isNumeric,
                  let anchor = rendezvous.clockAnchor(
                    incumbentFilmMs: player.currentTime().seconds * 1_000 + Double(baseMs),
                    hostSeconds: hostNow.seconds, rate: Double(rate),
                    frameDurationSeconds: frameDurationSeconds,
                    overlapRemainingMs: preparedOverlapRemainingMs
                  ) else { return nil }
            if preparedClockRunway(item, rendezvous: rendezvous, anchor: anchor) {
                return anchor
            }
            try? await Task.sleep(nanoseconds: 8_000_000)
        }
        return nil
    }

    private func preparedClockRunway(
        _ item: AVPlayerItem, rendezvous: PreparedCommitRendezvous,
        anchor: PreparedCommitRendezvous.ClockAnchor
    ) -> Bool {
        let ranges = item.loadedTimeRanges.compactMap { value -> ClosedRange<Double>? in
            let range = value.timeRangeValue
            let start = range.start.seconds, end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, start >= 0, end >= start else { return nil }
            return start...end
        }
        return preparedClockStartupHasRunway(earliestItemMs: anchor.earliestItemMs,
            rendezvousItemMs: rendezvous.itemPositionMs, loadedItemRanges: ranges)
    }

    /// The successor waits at a future film instant while the incumbent
    /// advances to it. Seeking the moving incumbent clock chases seek latency.
    private func awaitPreparedRendezvous(
        item: AVPlayerItem, successor: AVPlayer, rendezvous: PreparedCommitRendezvous,
        frameDurationSeconds: Double, rate: Float,
        anchor: PreparedCommitRendezvous.ClockAnchor, commit: Attempt
    ) async -> Bool {
        let began = Int(ProcessInfo.processInfo.systemUptime * 1_000)
        // A fixed future rendezvous can be farther away when inspection/seek
        // finish early. Spend only the original overlap's remaining time;
        // never create a fresh preparation deadline at this suspension.
        let remaining = preparedOverlapRemainingMs
        while Int(ProcessInfo.processInfo.systemUptime * 1_000) - began < remaining,
              preparedOverlapRemainingMs > 0 {
            guard !Task.isCancelled, preparedItem === item, preparedPlayer === successor,
                  preparedCommitStillOwned(commit), wantsPlayback, preferredRate == rate,
                  UIApplication.shared.applicationState == .active,
                  !pictureInPictureIsActive, !player.isExternalPlaybackActive,
                  item.status == .readyToPlay else { return false }
            let incumbentMs = player.currentTime().seconds * 1_000 + Double(baseMs)
            let successorMs = successor.currentTime().seconds * 1_000
            let hostSeconds = CMClockGetTime(CMClockGetHostTimeClock()).seconds
            if hostSeconds >= anchor.hostSeconds,
               player.rate == rate, player.timeControlStatus == .playing,
               successor.rate == rate, successor.timeControlStatus == .playing,
               rendezvous.clocksMatch(incumbentFilmMs: incumbentMs,
                   successorItemMs: successorMs, frameDurationSeconds: frameDurationSeconds) {
                return true
            }
            if !incumbentMs.isFinite || !successorMs.isFinite || !hostSeconds.isFinite
                || incumbentMs > Double(rendezvous.filmPositionMs) + frameDurationSeconds * 1_000 {
                return false
            }
            try? await Task.sleep(nanoseconds: 8_000_000)
        }
        return false
    }

    /// Check the parked item's decoded output after its alignment seek.
    /// A hidden layer ready bit from before the seek cannot satisfy this proof.
    private func awaitPreparedDecodedAlignment(
        item: AVPlayerItem, successor: AVPlayer, output: AVPlayerItemVideoOutput,
        rendezvous: PreparedCommitRendezvous, frameDurationSeconds: Double, commit: Attempt
    ) async -> Bool {
        let startedAt = Int(ProcessInfo.processInfo.systemUptime * 1_000)
        while Int(ProcessInfo.processInfo.systemUptime * 1_000) - startedAt < PreparedReplacementBounds.alignmentMs,
              preparedOverlapRemainingMs > 0 {
            guard !Task.isCancelled, preparedItem === item, preparedPlayer === successor,
                  preparedVideoOutput === output, preparedCommitStillOwned(commit),
                  wantsPlayback, UIApplication.shared.applicationState == .active,
                  !pictureInPictureIsActive, !player.isExternalPlaybackActive,
                  item.status == .readyToPlay else { return false }
            let itemTime = item.currentTime()
            var displayTime = CMTime.invalid
            if itemTime.isValid, itemTime.seconds.isFinite,
               output.hasNewPixelBuffer(forItemTime: itemTime),
               let pixels = output.copyPixelBuffer(forItemTime: itemTime, itemTimeForDisplay: &displayTime),
               displayTime.isValid,
               rendezvous.acceptsDecodedAlignment(
                displaySeconds: displayTime.seconds,
                width: CVPixelBufferGetWidth(pixels), height: CVPixelBufferGetHeight(pixels),
                frameDurationSeconds: frameDurationSeconds
               ), playbackSurface?.canPromote(successor) == true {
                return true
            }
            try? await Task.sleep(nanoseconds: UInt64(PreparedReplacementBounds.pollMs) * 1_000_000)
        }
        return false
    }

    /// Wall clock at the successor's first frame that is actually at or past
    /// the switch boundary.
    ///
    /// Taken from the item's own video output rather than a timer, because a
    /// timer measures how long this code took, not when the viewer's picture
    /// changed. `AVPlayerItemVideoOutput` does not acknowledge physical
    /// display by `AVPlayerLayer` — the same caveat the seek monitor carries —
    /// so device qualification still has to verify the final boundary.
    private func awaitPreparedFirstFrame(boundaryMs: Int, overlapStartedAtMs: Int, frameDurationSeconds: Double) async -> Int? {
        guard let output = preparedSeekVideoOutput(), let item = player.currentItem else {
            return nil
        }
        preparedFrameBudget = PreparedActiveWallBudget(
            boundMs: PreparedReplacementBounds.firstFrameMs,
            nowMs: Int(ProcessInfo.processInfo.systemUptime * 1_000),
            playbackRequested: wantsPlayback,
            overlapBoundMs: PreparedReplacementBounds.overlapMs,
            overlapStartedAtMs: overlapStartedAtMs
        )
        defer { preparedFrameBudget = nil }
        while true {
            guard !Task.isCancelled, started, player.currentItem === item,
                  seekVideoOutput === output else { return nil }
            if preparedFrameBudget?.update(
                nowMs: Int(ProcessInfo.processInfo.systemUptime * 1_000),
                playbackRequested: wantsPlayback
            ) != false { return nil }
            let itemTime = output.itemTime(forHostTime: ProcessInfo.processInfo.systemUptime)
            var displayTime = CMTime.invalid
            if itemTime.isValid,
               itemTime.seconds.isFinite,
               output.hasNewPixelBuffer(forItemTime: itemTime),
               output.copyPixelBuffer(
                   forItemTime: itemTime, itemTimeForDisplay: &displayTime
               ) != nil,
               displayTime.isValid,
               displayTime.seconds.isFinite {
                let presentedMs = baseMs + max(0, Int(displayTime.seconds * 1_000))
                if PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: Double(presentedMs), boundaryMs: boundaryMs, frameDurationSeconds: frameDurationSeconds) {
                    return Int(Date().timeIntervalSince1970 * 1_000)
                }
            }
            try? await Task.sleep(
                nanoseconds: UInt64(PreparedReplacementBounds.pollMs) * 1_000_000
            )
        }
    }

    private func preparedSeekVideoOutput() -> AVPlayerItemVideoOutput? {
        // An audio-only source produces no pixel buffer, ever, so waiting for
        // one would turn every such commit into a `failed`. There is no
        // prepared handoff for audio-only sources today; this is the guard
        // that says so out loud rather than hanging on it.
        guard decision?.source?.videoCodec != nil else { return nil }
        return seekVideoOutput
    }

    /// The in-place quality change this platform has always done.
    ///
    /// Not an error branch. It is what happens whenever the second pipeline
    /// cannot be made ready — including on a device with one hardware decoder
    /// slot, which M5.5 did not rule out — and it has to be as ordinary as the
    /// path it replaces.
    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction) {
        if retainManualQualityFailure() { return }
        if action.effectiveSelection.qualityAuto, action.effectiveSelection.candidateId != nil,
           selectedHeight == nil, !selectedQualityIsOriginal, !autoExposed {
            // The coordinator recorded abandonment before discarding the
            // successor. Repeating it here loses the item and its deadline
            // and makes one failed preparation appear to be two.
            playbackControl.reportEvidence()
            return
        }
        let actionEpoch = viewerActionEpoch
        Task { @MainActor [weak self] in
            guard let self, self.started, self.viewerActionEpoch == actionEpoch else { return }
            // Recorded inside the guard, never before it. A newer viewer
            // action owns the player by the time this hop lands, and a
            // fallback that reopened anyway would move the film out from under
            // the command that superseded it.
            Caps.PreparedHandoffTelemetry.shared.note(outcome: "fell back")
            await self.reopen(at: self.positionForPlaybackIntent())
        }
    }

    func preparedSuccessorOwesAnExchange() {
        playbackControl.reportEvidence()
    }

    /// How long the viewer's picture was interrupted by a fallback.
    ///
    /// Apple's fallback interruption is unmeasured, because Apple passed dual
    /// preparation and never exercised the fallback. Recording it here is what
    /// turns the first device that takes this path into the measurement.
    func recordPreparedFallbackInterruption(ms: Int) {
        preparedFallbackInterruptionMs = ms
    }
}
