import AVKit
import Combine
import Foundation
import MediaPlayer

/// Separate fixed Shared owner: no Local file/history/recovery dispatch is
/// reachable from this controller. The server retains physical ownership.
///
/// One owned operation holds the player at a time — the Start, a control
/// exchange, a reopen — so a viewer action never races the attachment it acts
/// on, and `stop()` joins whatever is in flight before it releases B sessions.
/// Every B session this controller started and no longer plays is listed in
/// `unreleased` until its DELETE has been sent, including one whose release
/// was interrupted by `stop()` cancelling the operation.
@MainActor
final class SharedPlayerController: ObservableObject {
    private enum Operation {
        case start
        case remoteStart(SharedPlaybackPlan, () -> Bool, (CinemaRemoteOutcome) -> Void)
        case control(SharedControlIntent)
        case remoteControl(SharedControlIntent, () -> Bool, (CinemaRemoteOutcome) -> Void)
        case change(SharedDirectedChange)
        case restartDirect(session: String)
        case caption(SharedCaptionAsset, Int)
    }
    let player = AVPlayer()
    private var captionIntentSerial = 0
    let captions = SharedCaptionReadSession()
    private var captionObservation: AnyCancellable?
    private func observeCaptions() { captionObservation = captions.objectWillChange.sink { [weak self] in self?.objectWillChange.send() } }
    private let makeClient: () throws -> SharedDecisionClient
    init() { makeClient = { try SharedDecisionClient() }; observeCaptions() }
    #if DEBUG
    private var beforeStopCleanup: (() async -> Void)?
    init(testConfiguration: URLSessionConfiguration, beforeStopCleanup: (() async -> Void)? = nil) {
        makeClient = { try SharedDecisionClient(testConfiguration: testConfiguration) }
        self.beforeStopCleanup = beforeStopCleanup
        observeCaptions()
    }
    /// Inject only an already authenticated Start and its attached item. The
    /// caption/change operation and B control exchange remain production code.
    func seedCaptionPlaybackForTesting(plan: SharedPlaybackPlan, playback: SharedStartedPlayback,
                                       client: SharedDecisionClient, item: AVPlayerItem) throws {
        self.plan=plan;self.playback = .hls(playback);self.client=client
        var capabilities=Caps.controlCapabilities();capabilities.dualPlayerPreparation=false
        channel=try SharedControlChannel(playback:playback,clientInstanceId:clientInstanceId,capabilities:capabilities)
        player.replaceCurrentItem(with:item)
        configureCaptions(plan,client:client)
    }
    #endif
    @Published private(set) var playback: SharedStartedMedia?
    @Published private(set) var plan: SharedPlaybackPlan?
    @Published private(set) var failure: String?
    @Published private(set) var notice: String?
    @Published private(set) var starting = false
    @Published private(set) var busy = false
    @Published private(set) var playing = false
    @Published private(set) var statusSummary: String?
    private var client: SharedDecisionClient?
    private var channel: SharedControlChannel?
    /// Stable for this player; B's tuple is per session, this is per viewer.
    private let clientInstanceId = UUID().uuidString.lowercased()
    private var operation: Task<Void, Never>?
    private var operationSerial = 0
    private var networkOperationSerial: Int?
    private var stopCleanup: Task<Void, Never>?
    private var progressTask: Task<Void, Never>?
    private var unreleased: [SharedStartedMedia] = []
    private var timeObserver: Any?
    private var ticks = 0
    private var lastPositionMs = 0
    private var historyFrameObserved = false
    private var historyOutput: AVPlayerItemVideoOutput?
    private var authorizationObserver: UUID?
    private var itemObservers: [NSObjectProtocol] = []
    private var statusObservation: NSKeyValueObservation?
    /// B answered 410 to a pause: the item stays paused and the next play or
    /// seek starts fresh at its position.
    private var sessionEnded = false
    /// One fresh Start per attachment after B lost the session. A new
    /// attachment earns it again when it reaches its own timeline.
    private var restartAllowance = false
    private var closing = false
    /// Called once the title played to its natural end and its final watched
    /// beat and B release are done. The view decides whether a next Shared
    /// episode follows; this controller never starts another title itself.
    var onNaturalEnd: (() -> Void)?
    /// A prepared change is waiting for B's offer or priming B's successor
    /// beside the incumbent. A viewer action may still take the player back
    /// (the preparation settles `aborted`); never during the switch itself or
    /// its settlement.
    @Published private(set) var preparing = false
    private var preemptRequested = false
    private var preempting = false
    /// The Local M6 coordinator for the one live preparation.
    private var preparation: PreparedReplacementCoordinator?
    private var preparedPlayer: AVPlayer?
    private var preparedItem: AVPlayerItem?
    private var preparedOutput: AVPlayerItemVideoOutput?
    private var preparedFilmPositionMs = 0
    private var preparedSeekPending = false
    /// B's staged successor, bound to this player before it is primed. Until
    /// adopted it is not this player's session; `stop()` still ends it.
    private var preparedAdoption: (plan: SharedPlaybackPlan, playback: SharedStartedPlayback)?
    private var preparedFallbackOwed = false
    private var preparedSwapped = false
    private var preparedAlignment: (generation: Int, landed: Bool?) = (0, nil)
    private var lastExchangeMs = 0

    var isDirect: Bool { playback?.isDirect == true }

    func start(_ plan: SharedPlaybackPlan) async {
        guard self.plan == nil, !closing else { return }
        self.plan = plan; starting = true
        player.appliesMediaSelectionCriteriaAutomatically = false
        authorizationObserver = Session.shared.observeAuthorizationChanges { [weak self] _ in
            Task { @MainActor in await self?.stop() }
        }.id
        await run(.start)
    }

    /// Seek, pause or play. On HLS B accepts it first; on direct play there is
    /// no control route (Local or shared), so the renderer is the whole state.
    func control(_ intent: SharedControlIntent) async {
        await retireNetworkBeforePhysicalOperation()
        await run(.control(intent))
    }
    private(set) var remoteRetryRequiresPreparation = false
    var remoteStartAvailable: Bool { !closing && !busy && plan == nil }
    // A visible stopped/ended owner still admits idempotent Stop and exit.
    var remoteStopAvailable: Bool { true }
    func startRemote(_ plan: SharedPlaybackPlan, permit: @escaping () -> Bool) async -> CinemaRemoteOutcome {
        guard remoteStartAvailable, permit() else { return .unavailable }
        self.plan = plan; starting = true
        player.appliesMediaSelectionCriteriaAutomatically = false
        authorizationObserver = Session.shared.observeAuthorizationChanges { [weak self] _ in Task { @MainActor in await self?.stop() } }.id
        var result: CinemaRemoteOutcome = .unavailable
        await run(.remoteStart(plan, permit, { result = $0 }), network: true)
        starting = false
        if result != .applied, !closing, playback == nil,
           self.plan?.request.requestId == plan.request.requestId {
            self.plan = nil
            remoteRetryRequiresPreparation = true
            failure = "Shared playback could not be confirmed. Press Play to prepare this file again."
            if let authorizationObserver { Session.shared.removeAuthorizationObserver(authorizationObserver); self.authorizationObserver = nil }
        } else if result == .applied {
            remoteRetryRequiresPreparation = false
            failure = nil
        }
        return result
    }
    var remoteTransportAvailable: Bool { !closing && playback != nil && !busy && !sessionEnded }
    func cancelNetworkControl() {
        guard networkOperationSerial == operationSerial else { return }
        operation?.cancel()
    }
    private func retireNetworkBeforePhysicalOperation() async {
        guard networkOperationSerial == operationSerial, let pending = operation else { return }
        pending.cancel()
        await pending.value
    }
    /// Uses the incumbent operation owner and B control channel. A network
    /// request never preempts unrelated preparation or creates another renderer.
    func controlRemote(_ intent: SharedControlIntent, permit: @escaping () -> Bool) async -> CinemaRemoteOutcome {
        guard intent == .play || intent == .pause, remoteTransportAvailable, permit() else { return .unavailable }
        var result: CinemaRemoteOutcome = .unavailable
        await run(.remoteControl(intent, permit, { result = $0 }), network: true)
        return result
    }

    /// Quality, audio or subtitle. B declines a directed change on a shared
    /// session with `preparation: "none"`, and the answer is a fresh Start.
    func change(_ change: SharedDirectedChange) async {
        if change.subtitleIndex != nil || change.audioIndex != nil { captionIntentSerial += 1 }
        await retireNetworkBeforePhysicalOperation(); await run(.change(change))
    }

    func selectCaption(_ asset: SharedCaptionAsset) async {
        captionIntentSerial += 1
        let intent = captionIntentSerial
        await retireNetworkBeforePhysicalOperation()
        guard intent == captionIntentSerial else { return }
        await run(.caption(asset, intent))
    }
    func useLocalCaptionOutput() { player.allowsExternalPlayback=false }
    private func performCaptionSelection(_ asset: SharedCaptionAsset, intent: Int) async {
        guard intent == captionIntentSerial, let initialPlan = plan else { return }
        let owner = operationSerial, source = initialPlan.subject.context.sourceKey
        let burned = initialPlan.request.subtitleBurn != nil
        let incumbent = player.currentItem
        guard !player.isExternalPlaybackActive else { notice="Return playback to this device to display published captions.";return }
        if plan?.request.subtitleBurn != nil { await performChange(SharedDirectedChange(subtitleIndex:.some(nil))) }
        guard !closing,!Task.isCancelled,intent == captionIntentSerial,owner == operationSerial,!preemptRequested,
              let acceptedPlan=plan,acceptedPlan.subject.context.sourceKey == source,let item=player.currentItem else { return }
        guard acceptedPlan.request.subtitleBurn == nil,
              !burned || (item !== incumbent && acceptedPlan.rawSubtitleIndex == nil) else {
            notice="Captions were not selected because subtitle removal did not attach. Try again after the stream change finishes."
            return
        }
        if let group=try? await item.asset.loadMediaSelectionGroup(for:.legible) {
            guard !closing,!Task.isCancelled,intent == captionIntentSerial,owner == operationSerial,!preemptRequested,player.currentItem===item else { return }
            item.select(nil,in:group)
        }
        guard intent == captionIntentSerial,!closing,!Task.isCancelled,!preemptRequested,player.currentItem===item else { return }
        captions.choose(asset)
        if captions.selected != nil { player.allowsExternalPlayback=false }
    }
    private func configureCaptions(_ plan: SharedPlaybackPlan,client: SharedDecisionClient) {
        let key=plan.subject.context.sourceKey
        let tracks=plan.decision.presentation.audio ?? []
        let ordinal=plan.rawAudioIndex ?? plan.decision.presentation.delivery?.audio ?? tracks.first(where: { $0.default })?.index ?? (tracks.count==1 ? tracks.first?.index : nil)
        captions.configure(client:client,context:plan.subject.context,audio:ordinal,
            clock:{ [weak self] in self?.currentPositionMs() ?? 0 },
            current:{ [weak self] in self?.closing==false && self?.plan?.subject.context.sourceKey==key })
    }

    func currentPositionMs() -> Int { positionMs().map { Int($0) } ?? lastPositionMs }

    private func run(_ op: Operation, network: Bool = false) async {
        guard !closing else { return }
        if network && operation != nil { return }
        if let current = operation {
            // Only a preparation that has not reached its switch yields, and
            // only to one waiting action: it settles `aborted` and returns.
            guard preparing, !preempting else { return }
            preempting = true; preemptRequested = true
            await current.value
            preempting = false
            guard operation == nil, !closing else { return }
        }
        operationSerial += 1
        let serial = operationSerial
        networkOperationSerial = network ? serial : nil
        busy = true; preemptRequested = false
        let task = Task {
            await self.perform(op)
            if self.operationSerial == serial, !self.closing { self.operation = nil; self.busy = false; self.networkOperationSerial = nil }
        }
        operation = task
        await task.value
    }

    private func perform(_ op: Operation) async {
        switch op {
        case .start: await performStart()
        case .remoteStart(let plan, let permit, let complete):
            do {
                guard permit(), !closing, !Task.isCancelled else { complete(.unavailable); return }
                let client = try makeClient(); self.client = client
                try await attach(plan, client: client, play: true, predecessor: nil, permit: permit)
                complete(playing ? .applied : .unavailable)
            } catch { complete(.unavailable) }
        case .control(let intent): _ = await performControl(intent)
        case .remoteControl(let intent, let permit, let complete): complete(await performControl(intent, permit: permit))
        case .change(let change): await performChange(change)
        case .restartDirect(let session): await restartDirect(session)
        case .caption(let asset, let intent): await performCaptionSelection(asset, intent: intent)
        }
    }

    private func performStart() async {
        guard let plan else { return }
        do {
            let client = try makeClient(); self.client = client
            try await attach(plan, client: client, play: true, predecessor: nil)
        } catch {
            if !closing, !(error is CancellationError) { failure = error.localizedDescription }
        }
        starting = false
    }

    /// Start `plan`, put its media on the renderer at the plan's position, and
    /// only then release `predecessor`. A failed successor leaves the
    /// predecessor attached and serving.
    private func attach(_ plan: SharedPlaybackPlan, client: SharedDecisionClient, play: Bool,
                        predecessor: SharedStartedMedia?, permit: (() -> Bool)? = nil) async throws {
        if let permit, !permit() || closing || Task.isCancelled { throw CancellationError() }
        let media = try await client.startMedia(context: plan.subject.context, request: plan.request)
        let url: URL
        var nextChannel: SharedControlChannel?
        do {
            try Task.checkCancellation()
            guard !closing, permit?() ?? true else { throw CancellationError() }
            switch media {
            case .hls(let started):
                url = try client.playlistURL(playback: started)
                let measured = Caps.controlCapabilities()
                var capabilities = Caps.controlCapabilities(
                    hevc: plan.caps.video.contains { $0.codec == "hevc" },
                    av1: plan.caps.video.contains { $0.codec == "av1" },
                    displayHDR: plan.caps.display.hdr,
                    dolbyVision: plan.caps.display.dolbyVision,
                    dualPlayerPreparation: measured.dualPlayerPreparation)
                // The same Developer switch Local reads. A source without video
                // has no frame to prove a switch with, so it keeps the reopen.
                capabilities.dualPlayerPreparation = capabilities.dualPlayerPreparation
                    && plan.decision.presentation.source?.videoCodec != nil
                nextChannel = try SharedControlChannel(playback: started, clientInstanceId: clientInstanceId,
                                                       capabilities: capabilities)
            case .direct(let direct):
                url = try client.directURL(direct)
            }
        } catch {
            unreleased.append(media)
            let cleanup = Task { await self.releaseUnreleased(client) }
            await cleanup.value
            throw error
        }
        // The returned B session must be released if its exact permit retired;
        // it must never attach to a replacement native renderer owner.
        if let permit, !permit() || closing || Task.isCancelled {
            let cleanup = Task { try? await client.end(media: media) }
            await cleanup.value
            throw CancellationError()
        }
        // Direct play reads the signed alias with no account header.
        let item = AVPlayerItem(url: url)
        historyFrameObserved = false
        let output = AVPlayerItemVideoOutput(pixelBufferAttributes: nil)
        item.add(output); historyOutput = output
        observe(item)
        player.replaceCurrentItem(with: item)
        self.plan = plan; playback = media; channel = nextChannel
        configureCaptions(plan,client:client)
        statusSummary = nil; sessionEnded = false; notice = nil
        if let predecessor { unreleased.append(predecessor) }
        let resume = plan.subject.resumeMs
        lastPositionMs = Int(resume)
        if resume > 0 {
            await player.seek(to: CMTime(seconds: Double(resume) / 1000, preferredTimescale: 1000),
                              toleranceBefore: .zero, toleranceAfter: .zero)
        }
        await applySubtitle(plan, item: item, permit: permit)
        let release = Task { await self.releaseUnreleased(client) }
        await release.value
        guard !closing, player.currentItem === item else { return }
        if let permit, !permit() || Task.isCancelled {
            player.pause(); playing = false
            // This session is owned but was never allowed to start rendering.
            let cleanup = Task { try? await client.end(media: media) }
            await cleanup.value
            if player.currentItem === item { player.replaceCurrentItem(with: nil); playback = nil; channel = nil }
            throw CancellationError()
        }
        if play { player.play() } else { player.pause() }
        playing = play
        installTimeObserver()
    }

    /// DELETE every B session this controller holds but no longer plays. An
    /// interrupted DELETE stays listed for `stop()`.
    private func releaseUnreleased(_ client: SharedDecisionClient) async {
        while let media = unreleased.first {
            try? await client.end(media: media)
            if Task.isCancelled { return }
            unreleased.removeFirst()
        }
    }

    private func applySubtitle(_ plan: SharedPlaybackPlan, item: AVPlayerItem, permit: (() -> Bool)? = nil) async {
        guard let group = try? await item.asset.loadMediaSelectionGroup(for: .legible),
              !closing, permit?() ?? true, player.currentItem === item else { return }
        _ = PlayerController.selectNativeSubtitle(captions.selected == nil ? plan.rawSubtitleIndex : nil, tracks: plan.decision.presentation.subtitles ?? [],
                                              in: group, of: item)
    }

    private func performControl(_ intent: SharedControlIntent, permit: (() -> Bool)? = nil) async -> CinemaRemoteOutcome {
        func allowed() -> Bool { !closing && !Task.isCancelled && (permit?() ?? true) }
        guard allowed(), let media = playback, let client, let plan else { return .unavailable }
        notice = nil
        switch media {
        case .direct:
            guard allowed() else { return .unavailable }
            await applyRenderer(intent)
            return allowed() ? .applied : .unavailable
        case .hls(let started):
            if sessionEnded {
                if intent == .pause { return .applied }
                guard permit == nil, allowed() else { return .unavailable }
                await reopen(at: target(intent), change: nil, play: intent == .play)
                return .applied
            }
            guard var channel else { return .unavailable }
            do {
                let request = try channel.request(intent, sample: sample(), selection: try plan.frozenControlSelection())
                guard allowed() else { return .unavailable }
                self.channel = channel
                let outcome = try await exchange(request, playback: started, channel: channel, client: client, permit: permit)
                // Settling an already-owned unwanted offer is resource cleanup,
                // independent of the expired/cancelled semantic permission.
                let cleanup: (() async -> Void)?
                if case .offered(let offer, _) = outcome { cleanup = { await self.abortUnwanted(offer, playback: started, client: client) } }
                else { cleanup = nil }
                return await SharedRemoteControlCompletion.finish(cleanup: cleanup, permit: allowed) {
                // Once B was contacted, cancellation/expiry cannot prove rollback.
                switch SharedControlStep.after(intent, outcome: outcome, playing: playing, restartAllowed: restartAllowance) {
                case .apply:
                    guard allowed() else { return .unavailable }
                    await applyRenderer(intent)
                    return .applied
                case .pauseEnded:
                    guard allowed() else { return .unavailable }
                    player.pause(); playing = false; sessionEnded = true
                    return .applied
                case .reopen(let play):
                    guard permit == nil, allowed() else { return .unavailable }
                    restartAllowance = false
                    await reopen(at: target(intent), change: nil, play: play)
                    return .applied
                case .ended: failure = "This Shared session ended."; return .unavailable
                case .refused: notice = "The Shared server did not accept this control. Try again."; return .unsupported
                }
                }
            } catch {
                if allowed() { notice = "The Shared server did not accept this control. Try again." }
                return .unavailable
            }
        }
    }

    private func performChange(_ change: SharedDirectedChange) async {
        guard let media = playback, let client, let plan else { return }
        let captionWasSelected = captions.selected != nil
        if change.subtitleIndex != nil || change.audioIndex != nil {
            captions.deselect(); player.allowsExternalPlayback = true
        }
        notice = nil
        switch media {
        case .direct:
            // Raw bytes have no rendition to change: any real change is a
            // fresh session, which leaves direct play for copy or encoded HLS.
            guard (change.quality ?? plan.rawQuality) != plan.rawQuality
                    || (change.audioIndex ?? plan.rawAudioIndex) != plan.rawAudioIndex
                    || (change.subtitleIndex ?? plan.rawSubtitleIndex) != plan.rawSubtitleIndex else {
                if captionWasSelected,let item=player.currentItem { await applySubtitle(plan,item:item) }
                return
            }
            await reopen(at: currentPositionMs(), change: change, play: playing)
        case .hls(let started):
            if sessionEnded { await reopen(at: currentPositionMs(), change: change, play: false); return }
            guard var channel else { return }
            do {
                let selection = try plan.directedSelection(change)
                let frozen = try plan.frozenControlSelection()
                guard selection != frozen else {
                    if captionWasSelected,let item=player.currentItem { await applySubtitle(plan,item:item) }
                    return
                }
                if channel.prepares {
                    try await prepareChange(change, selection: selection, frozen: frozen, playback: started, client: client)
                    return
                }
                let request = try channel.request(nil, sample: sample(), selection: selection)
                self.channel = channel
                let outcome = try await exchange(request, playback: started, channel: channel, client: client)
                // B never carries a Source successor, so an evaluated answer is
                // a decline: reopen at once, at the position sampled when it
                // arrived. Absence is never a decline.
                switch SharedControlStep.afterChange(outcome, playing: playing) {
                case .reopen(let play): await reopen(at: currentPositionMs(), change: change, play: play)
                default:
                    if case .accepted = outcome { notice = "This change is not available for this Shared title." }
                    else { notice = "The Shared server did not accept this change. Try again." }
                }
            } catch is CancellationError {
            } catch {
                notice = error.localizedDescription
            }
        }
    }

    /// One exchange, replayed once with identical bytes when B asks for it.
    private func exchange(_ request: SharedControlRequest, playback: SharedStartedPlayback,
                          channel: SharedControlChannel, client: SharedDecisionClient, permit: (() -> Bool)? = nil) async throws -> SharedControlOutcome {
        if let permit, !permit() || Task.isCancelled || closing { throw CancellationError() }
        let first = try await client.control(playback: playback, channel: channel, request: request)
        lastExchangeMs = PlaybackControlSession.monotonicMs()
        guard case .retry(let delay) = first else { return first }
        try await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000)
        defer { lastExchangeMs = PlaybackControlSession.monotonicMs() }
        if let permit, !permit() || Task.isCancelled || closing { throw CancellationError() }
        return try await client.control(playback: playback, channel: channel, request: request)
    }

    private func applyRenderer(_ intent: SharedControlIntent) async {
        switch intent {
        case .play: player.play(); playing = true
        case .pause: player.pause(); playing = false; lastPositionMs = currentPositionMs()
        case .seek(let ms):
            captions.mediaChanged(seekTo: max(0,ms))
            await player.seek(to: CMTime(seconds: Double(max(0, ms)) / 1000, preferredTimescale: 1000),
                              toleranceBefore: .zero, toleranceAfter: .zero)
            lastPositionMs = currentPositionMs()
        }
    }

    /// A fresh shared Start of the same file at `positionMs` with no lineage
    /// fields, under the same playback id. The ordered watch order is keyed by
    /// the Source item, so the next beat continues the sequence on the new B
    /// session. The predecessor is released once the new one is attached; B
    /// also supersedes it when the new session publishes.
    private func reopen(at positionMs: Int, change: SharedDirectedChange?, play: Bool) async {
        guard let plan, let client, let predecessor = playback else { return }
        await progressTask?.value
        do {
            let quality = change?.quality ?? plan.rawQuality
            let audio = change?.audioIndex ?? plan.rawAudioIndex
            let subtitle = change?.subtitleIndex ?? plan.rawSubtitleIndex
            let subject = SharedPlaybackSubject(context: plan.subject.context, title: plan.subject.title,
                                                resumeMs: Int64(max(0, positionMs)), watchSequence: plan.subject.watchSequence)
            let decided = try await client.decision(context: subject.context,
                selection: PrePlaySelection(audioIndex: audio, subtitleIndex: subtitle ?? PrePlaySelection.subtitleOff),
                quality: quality)
            let next = try SharedPlaybackPlan.make(subject: subject, decision: decided.decision, caps: decided.caps,
                quality: quality, audioIndex: audio, subtitleIndex: subtitle, playbackId: plan.request.playbackId)
            try await attach(next, client: client, play: play, predecessor: predecessor)
        } catch is CancellationError {
        } catch {
            notice = "This Shared change could not start: \(error.localizedDescription)"
        }
    }

    private func restartDirect(_ session: String) async {
        guard case .direct(let direct)? = playback, direct.start.sessionId == session, let client else { return }
        let gone = (try? await client.directSessionGone(direct)) == true
        guard gone, restartAllowance else {
            failure = "Shared direct play stopped."
            return
        }
        restartAllowance = false
        await reopen(at: lastPositionMs, change: nil, play: true)
    }

    private func target(_ intent: SharedControlIntent) -> Int {
        if case .seek(let ms) = intent { return ms }
        return currentPositionMs()
    }

    private func sample() -> SharedRendererSample {
        let position = currentPositionMs()
        var from: Int?, through = position
        if let item = player.currentItem {
            for value in item.loadedTimeRanges {
                let range = value.timeRangeValue
                let start = range.start.seconds * 1000, end = range.end.seconds * 1000
                guard start.isFinite, end.isFinite, start <= Double(position), end >= Double(position) else { continue }
                from = Int(start.rounded(.down)); through = max(position, Int(end.rounded(.down)))
            }
        }
        return SharedRendererSample(positionMs: position, bufferedFromMs: from, bufferedThroughMs: through,
                                    playing: player.timeControlStatus != .paused)
    }

    private func observe(_ item: AVPlayerItem) {
        for observer in itemObservers { NotificationCenter.default.removeObserver(observer) }
        itemObservers = [
            NotificationCenter.default.addObserver(forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main) { [weak self] _ in
                Task { @MainActor in
                    guard let self, !self.closing else { return }
                    await self.stop(watched: true)
                    self.onNaturalEnd?()
                }
            },
            NotificationCenter.default.addObserver(forName: .AVPlayerItemFailedToPlayToEndTime, object: item, queue: .main) { [weak self, weak item] _ in
                Task { @MainActor in if let item { self?.rendererFailed(item) } }
            },
        ]
        statusObservation = item.observe(\.status, options: [.new]) { [weak self] item, _ in
            let status = item.status
            Task { @MainActor in
                guard let self, self.player.currentItem === item else { return }
                // The attachment reached its own timeline: it earns one fresh
                // Start for a session B later loses.
                if status == .readyToPlay { self.restartAllowance = true }
                if status == .failed { self.rendererFailed(item) }
            }
        }
    }

    private func rendererFailed(_ item: AVPlayerItem) {
        guard !closing, player.currentItem === item else { return }
        if case .direct(let direct)? = playback {
            let session = direct.start.sessionId
            Task { await self.run(.restartDirect(session: session)) }
        } else {
            failure = item.error?.localizedDescription ?? "Shared playback stopped."
        }
    }

    private func installTimeObserver() {
        guard timeObserver == nil else { return }
        timeObserver = player.addPeriodicTimeObserver(forInterval: CMTime(seconds: 1, preferredTimescale: 1000), queue: .main) { [weak self] _ in
            Task { @MainActor in
                guard let self, let position = self.positionMs() else { return }
                if !self.historyFrameObserved, let output = self.historyOutput {
                    let time = output.itemTime(forHostTime: ProcessInfo.processInfo.systemUptime)
                    if time.isValid, output.hasNewPixelBuffer(forItemTime: time),
                       output.copyPixelBuffer(forItemTime: time, itemTimeForDisplay: nil) != nil {
                        self.historyFrameObserved = true
                    }
                }
                self.lastPositionMs = Int(position)
                self.captions.mediaChanged()
                self.ticks += 1
                if self.ticks % 10 == 0 { self.reportProgress() }
            }
        }
    }

    private func positionMs() -> Int64? {
        let value = player.currentTime().seconds * 1000
        guard value.isFinite, value >= 0, value <= 9_007_199_254_740_991 else { return nil }
        return Int64(value.rounded(.down))
    }

    private func durationMs(_ media: SharedStartedMedia) -> Int64? {
        if let duration = media.durationMs { return duration }
        let seconds = player.currentItem?.duration.seconds ?? .nan
        guard seconds.isFinite, seconds >= 0 else { return nil }
        return Int64((seconds * 1000).rounded(.down))
    }

    private func reportProgress() {
        guard !closing, historyFrameObserved, progressTask == nil, operation == nil, let client, let media = playback, let plan,
              let position = positionMs() else { return }
        let duration = durationMs(media)
        progressTask = Task { [weak self] in
            _ = try? await client.orderedProgress(media: media, initialWatchSequence: plan.subject.watchSequence,
                                                  positionMs: position, durationMs: duration)
            // Status belongs to the playback it was read for: a reopen in
            // between makes the answer stale, and direct play has none.
            if case .hls(let started) = media, self?.closing == false {
                let status = try? await client.status(playback: started)
                if self?.playback?.sessionId == started.start.response.sessionId { self?.statusSummary = status?.summary }
            }
            self?.progressTask = nil
        }
    }

    /// Local stop is synchronous; owned resource cleanup survives dismissal,
    /// command-credit expiry and cancellation of the caller awaiting its result.
    @discardableResult
    func beginStop(watched: Bool = false) -> Task<Void, Never> {
        if let stopCleanup { return stopCleanup }
        closing = true
        captions.retire()
        player.pause(); playing = false
        if let timeObserver { player.removeTimeObserver(timeObserver); self.timeObserver = nil }
        for observer in itemObservers { NotificationCenter.default.removeObserver(observer) }
        itemObservers = []; statusObservation = nil
        if let authorizationObserver { Session.shared.removeAuthorizationObserver(authorizationObserver); self.authorizationObserver = nil }
        operation?.cancel()
        let cleanup = Task { @MainActor in await self.finishStop(watched: watched) }
        stopCleanup = cleanup
        return cleanup
    }

    func stop(watched: Bool = false) async { await beginStop(watched: watched).value }

    private func finishStop(watched: Bool) async {
        #if DEBUG
        await beforeStopCleanup?()
        #endif
        if let operation { await operation.value }
        operation = nil; busy = false; preparing = false
        discardPreparedSuccessor(); preparation = nil
        await progressTask?.value
        if let client, let media = playback, let plan {
            if historyFrameObserved, let position = positionMs() {
                let duration = durationMs(media)
                let result = try? await client.orderedProgress(media: media, initialWatchSequence: plan.subject.watchSequence,
                    positionMs: position, durationMs: duration, watched: watched)
                if result == .previousBeatAcknowledged {
                    _ = try? await client.orderedProgress(media: media, initialWatchSequence: plan.subject.watchSequence,
                        positionMs: position, durationMs: duration, watched: watched)
                }
            }
            player.replaceCurrentItem(with: nil)
            unreleased.append(media)
        }
        // A staged successor this player never adopted is ended too: after a
        // commit whose answer was lost, it may be the session B now serves.
        if let staged = preparedAdoption?.playback { unreleased.append(.hls(staged)); preparedAdoption = nil }
        if let client { await releaseUnreleased(client) }
        player.replaceCurrentItem(with: nil); historyOutput = nil; playback = nil; channel = nil; statusSummary = nil
    }
}

// MARK: - Shared prepared successor

/// The Shared half of a prepared quality handoff, on the Local M6 machinery:
/// `PreparedOfferWait` decides when an offer, a decline or the bound has
/// arrived; `PreparedReplacementCoordinator` owns the ledger (build once,
/// abandon exactly once, commit only after a frame, always settle); the commit
/// uses the same rendezvous, alignment bound and first-frame rule as Local.
/// What is Shared: the offer names only B's successor session and control
/// bootstrap, every acknowledgement rides the predecessor's B channel, and a
/// committed successor becomes this player's session (control, status and
/// shared progress) without a DELETE of the predecessor, which B retires.
/// `none`, the wait's bound, and a failed or refused successor take the P0
/// reopen.
extension SharedPlayerController {
    func prepareChange(_ change: SharedDirectedChange, selection: SharedPlaybackJSON, frozen: SharedPlaybackJSON,
                       playback started: SharedStartedPlayback, client: SharedDecisionClient) async throws {
        preparing = true
        defer { preparing = false; preparedFallbackOwed = false; preparedSwapped = false }
        // B records a session's first exchange as the ask it was started for
        // and stages only a different one, so a silent session states its own
        // ask before the change.
        if channel?.sequence == 0 {
            let (_, first) = try await send(selection: frozen, playback: started, client: client)
            if case .ended = first { await reopen(at: currentPositionMs(), change: change, play: playing); return }
        }
        var (sequence, outcome) = try await send(selection: selection, playback: started, client: client)
        var wait = PreparedOfferWait(tappedAtMs: PlaybackControlSession.monotonicMs(), floorSequence: sequence)
        while true {
            // A newer viewer action carries the frozen ask, which withdraws
            // anything B staged for this one.
            if preemptRequested { return }
            if case .ended = outcome { await reopen(at: currentPositionMs(), change: change, play: playing); return }
            let answer = SharedPreparedHandoff.answer(outcome, sequence: sequence)
            switch wait.observe(answer: answer, nowMs: PlaybackControlSession.monotonicMs()) {
            case .reopen:
                await reopen(at: currentPositionMs(), change: change, play: playing)
                return
            case .keepWaiting(let delay):
                try await Task.sleep(nanoseconds: UInt64(max(1, delay)) * 1_000_000)
                if preemptRequested { continue }
                (sequence, outcome) = try await send(selection: selection, playback: started, client: client)
            case .offered:
                guard case .offered(let offer, _) = outcome else { return }
                do { try await prime(offer, change: change, selection: selection, playback: started, client: client) }
                catch is CancellationError { throw CancellationError() }
                catch {
                    // The viewer may already be on the successor's item while
                    // B never took the commit: a fresh Start puts them back on
                    // a session B serves.
                    if preparedSwapped { await reopen(at: currentPositionMs(), change: change, play: playing) }
                    throw error
                }
                return
            }
        }
    }

    /// One ordinary exchange on the current channel with no viewer intent.
    private func send(selection: SharedPlaybackJSON, playback: SharedStartedPlayback,
                      client: SharedDecisionClient) async throws -> (Int, SharedControlOutcome) {
        guard var channel else { throw APIError.badURL }
        await paceExchange()
        let request = try channel.request(nil, sample: sample(), selection: selection)
        self.channel = channel
        return (request.sequence, try await exchange(request, playback: playback, channel: channel, client: client))
    }

    /// B refuses exchanges closer than 250 ms; a preparation's own sequence of
    /// exchanges keeps clear of that rather than earning a 429.
    private func paceExchange() async {
        let wait = lastExchangeMs + 300 - PlaybackControlSession.monotonicMs()
        if wait > 0 { try? await Task.sleep(nanoseconds: UInt64(wait) * 1_000_000) }
    }

    /// One acknowledgement on the predecessor's channel, on its own ordered
    /// sequence. B answers an acknowledgement exchange byte-identically on a
    /// replay, so a lost or deferred answer resends the same bytes, at most
    /// three sends in all.
    private func acknowledge(_ acknowledgement: ActionAcknowledgement, selection: SharedPlaybackJSON,
                             playback: SharedStartedPlayback, client: SharedDecisionClient) async throws -> SharedControlOutcome {
        guard var channel else { return .refused }
        await paceExchange()
        let request = try channel.request(nil, sample: sample(), selection: selection, acknowledgement: acknowledgement)
        self.channel = channel
        var outcome = SharedControlOutcome.refused
        for attempt in 0..<3 {
            if attempt > 0, case .retry(let delay) = outcome { try await Task.sleep(nanoseconds: UInt64(delay) * 1_000_000) }
            outcome = try await client.control(playback: playback, channel: channel, request: request)
            lastExchangeMs = PlaybackControlSession.monotonicMs()
            guard case .retry = outcome else { break }
        }
        return outcome
    }

    /// B offered a successor this player did not ask for. It is settled
    /// `aborted` at once so B frees its slots instead of holding them to the
    /// deadline.
    func abortUnwanted(_ offer: SharedPreparedOffer, playback: SharedStartedPlayback, client: SharedDecisionClient) async {
        guard let plan, let selection = try? plan.frozenControlSelection() else { return }
        _ = try? await acknowledge(ActionAcknowledgement(actionId: offer.action.actionId, state: .aborted),
                                   selection: selection, playback: playback, client: client)
    }

    private func prime(_ offer: SharedPreparedOffer, change: SharedDirectedChange, selection: SharedPlaybackJSON,
                       playback started: SharedStartedPlayback, client: SharedDecisionClient) async throws {
        guard let plan else { return }
        let position = currentPositionMs()
        // Bound to this player before anything is primed: an offer that does
        // not bind to this context is a failed preparation, never a played one.
        let adoption: (plan: SharedPlaybackPlan, playback: SharedStartedPlayback)
        do { adoption = try plan.adopting(offer, change: change, predecessor: started, positionMs: position) }
        catch {
            _ = try await acknowledge(ActionAcknowledgement(actionId: offer.action.actionId, state: .failed),
                                      selection: selection, playback: started, client: client)
            await reopen(at: currentPositionMs(), change: change, play: playing)
            return
        }
        preparedAdoption = adoption
        let coordinator = PreparedReplacementCoordinator(host: self)
        preparation = coordinator
        defer { preparation = nil; discardPreparedSuccessor() }
        coordinator.offer(offer.action, filmPositionMs: position)
        var lastProgressMs = PlaybackControlSession.monotonicMs()
        while coordinator.hasActivePreparation, !coordinator.ledger.isSwitching {
            if preemptRequested || closing || Task.isCancelled { coordinator.abandonWithoutFallback(.aborted); break }
            guard let item = preparedItem else { coordinator.abandon(.failed); break }
            if item.status == .failed || coordinator.readinessBoundElapsed() { coordinator.abandon(.failed); break }
            if item.status == .readyToPlay {
                coordinator.successorIsMetadataReady()
                if preparedSeekPending {
                    // Issued once, and only once the item can honour it.
                    preparedSeekPending = false
                    let remaining = min(PreparedReplacementBounds.alignmentMs, coordinator.readinessRemainingMs() ?? 0)
                    guard await alignPrepared(item, to: preparedFilmPositionMs, boundMs: remaining) else {
                        if preemptRequested || closing || Task.isCancelled {
                            coordinator.abandonWithoutFallback(.aborted)
                        } else {
                            coordinator.abandon(.failed)
                        }
                        break
                    }
                    continue
                }
                if let through = preparedBufferedThroughMs(item),
                   through >= preparedFilmPositionMs + PreparedReplacementBounds.switchRunwayMs {
                    coordinator.successorIsBuffered(throughMs: through)
                    preparing = false
                    await coordinator.commit()
                    break
                }
            }
            // Progress states are optional and each costs an exchange: at most
            // one a second, and a lost one is not resent.
            if let ack = coordinator.pendingAcknowledgement, !ack.state.isTerminal,
               PlaybackControlSession.monotonicMs() - lastProgressMs >= 1_000 {
                _ = try await acknowledge(ack, selection: selection, playback: started, client: client)
                coordinator.acknowledgementDelivered(ack)
                lastProgressMs = PlaybackControlSession.monotonicMs()
            }
            try await Task.sleep(nanoseconds: UInt64(PreparedReplacementBounds.pollMs) * 1_000_000)
        }
        preparing = false
        // Everything still owed goes out in order; only a commit's answer
        // decides what this player plays next.
        var committed = false
        while let ack = coordinator.pendingAcknowledgement {
            let outcome = try await acknowledge(ack, selection: selection, playback: started, client: client)
            coordinator.acknowledgementDelivered(ack)
            if ack.state == .committed { committed = SharedPreparedHandoff.settled(outcome) }
        }
        if committed {
            adopt(adoption)
            return
        }
        if preparedSwapped && !preparedFallbackOwed {
            // The viewer is on the successor's item but B did not accept the
            // commit: end it with the predecessor once a fresh Start attaches.
            unreleased.append(.hls(adoption.playback))
            preparedFallbackOwed = true
        }
        preparedAdoption = nil
        if preparedFallbackOwed { await reopen(at: currentPositionMs(), change: change, play: playing) }
    }

    /// The committed successor becomes this player's session: its own B tuple
    /// on a fresh ordered channel, its status, and the shared progress beat
    /// (which names it from the next beat on, continuing the item's order).
    private func adopt(_ adoption: (plan: SharedPlaybackPlan, playback: SharedStartedPlayback)) {
        preparedAdoption = nil
        guard let capabilities = channel?.capabilities,
              let next = try? SharedControlChannel(playback: adoption.playback, clientInstanceId: clientInstanceId,
                                                   capabilities: capabilities)
        else { failure = "This Shared change could not continue."; return }
        plan = adoption.plan; playback = .hls(adoption.playback); channel = next
        if let client { configureCaptions(adoption.plan,client:client) }
        // The prepared commit already proved its successor frame.
        historyFrameObserved = true; historyOutput = nil
        statusSummary = nil; sessionEnded = false; restartAllowance = true
    }

    private func preparedBufferedThroughMs(_ item: AVPlayerItem) -> Int? {
        let playhead = item.currentTime().seconds
        guard playhead.isFinite else { return nil }
        for value in item.loadedTimeRanges {
            let range = value.timeRangeValue
            let start = range.start.seconds, end = CMTimeRangeGetEnd(range).seconds
            guard start.isFinite, end.isFinite, start <= playhead + 0.5, end > playhead else { continue }
            return Int((end * 1_000).rounded(.down))
        }
        return nil
    }

    /// Local's alignment: the seek runs in its own task so giving up on it at
    /// `alignmentMs` is possible at all (`AVPlayerItem.seek` ignores
    /// cancellation); the generation stops a late landing reporting in.
    private func alignPrepared(_ item: AVPlayerItem, to itemMs: Int, boundMs: Int = PreparedReplacementBounds.alignmentMs) async -> Bool {
        guard boundMs > 0, !preemptRequested, !closing, !Task.isCancelled else { return false }
        preparedAlignment = (preparedAlignment.generation &+ 1, nil)
        let generation = preparedAlignment.generation
        let seek = Task { @MainActor [weak self] in
            let landed = await item.seek(to: CMTime(value: CMTimeValue(max(0, itemMs)), timescale: 1_000),
                                         toleranceBefore: .zero, toleranceAfter: .zero)
            guard let self, self.preparedAlignment.generation == generation else { return }
            self.preparedAlignment.landed = landed
        }
        defer {
            // AVFoundation can ignore task cancellation. Invalidate the result
            // before cancelling the actual item seek, so a late callback cannot
            // satisfy this preparation or its successor generation.
            preparedAlignment.generation &+= 1
            seek.cancel()
            item.cancelPendingSeeks()
        }
        let landed = await awaitBoundedValue(
            boundMs: boundMs, pollMs: PreparedReplacementBounds.pollMs,
            now: { Int(ProcessInfo.processInfo.systemUptime * 1_000) },
            sleep: { try? await Task.sleep(nanoseconds: UInt64($0) * 1_000_000) },
            read: { [weak self] in
                guard let self, !self.preemptRequested, !self.closing, !Task.isCancelled,
                      self.preparedItem === item else { return false }
                return self.preparedAlignment.landed
            })
        return landed ?? false
    }

    /// Wall clock at the successor's own first frame at or past the switch.
    private func awaitPreparedFirstFrame(_ output: AVPlayerItemVideoOutput, boundaryMs: Int, frameDurationSeconds: Double) async -> Int? {
        let deadline = ProcessInfo.processInfo.systemUptime + Double(PreparedReplacementBounds.firstFrameMs) / 1_000
        while ProcessInfo.processInfo.systemUptime < deadline {
            if Task.isCancelled || closing { return nil }
            let itemTime = output.itemTime(forHostTime: ProcessInfo.processInfo.systemUptime)
            var displayTime = CMTime.invalid
            if itemTime.isValid, itemTime.seconds.isFinite, output.hasNewPixelBuffer(forItemTime: itemTime),
               output.copyPixelBuffer(forItemTime: itemTime, itemTimeForDisplay: &displayTime) != nil,
               displayTime.isValid, displayTime.seconds.isFinite,
               PreparedCommitRendezvous.frameHasReachedBoundary(displayMs: max(0, displayTime.seconds * 1_000), boundaryMs: boundaryMs, frameDurationSeconds: frameDurationSeconds) {
                return Int(Date().timeIntervalSince1970 * 1_000)
            }
            try? await Task.sleep(nanoseconds: UInt64(PreparedReplacementBounds.pollMs) * 1_000_000)
        }
        return nil
    }
}

extension SharedPlayerController: PreparedSuccessorHost {
    /// The muted, layer-less second pipeline on B's successor playlist. A
    /// shared session is VOD, so its timeline zero is the film's (Local's
    /// `sessionMediaOriginMs`) and the successor is primed at the film
    /// position itself, once its item can honour a seek.
    func startPreparedSuccessor(_ action: PreparedReplacementAction, filmPositionMs: Int) -> Bool {
        discardPreparedSuccessor()
        guard !closing, player.currentItem != nil, let client, let adoption = preparedAdoption,
              adoption.playback.start.response.sessionId == action.sessionId,
              let url = try? client.playlistURL(playback: adoption.playback) else { return false }
        let item = AVPlayerItem(url: url)
        let output = AVPlayerItemVideoOutput(pixelBufferAttributes: nil)
        item.add(output)
        let successor = AVPlayer(playerItem: item)
        successor.isMuted = true; successor.volume = 0
        successor.appliesMediaSelectionCriteriaAutomatically = false
        preparedPlayer = successor; preparedItem = item; preparedOutput = output
        let origin = PlayerController.sessionMediaOriginMs(vod: true, mediaOriginMs: action.mediaOriginMs,
                                                           startSeconds: nil, requestedStartMs: filmPositionMs)
        preparedFilmPositionMs = max(0, filmPositionMs - origin)
        preparedSeekPending = preparedFilmPositionMs > 0
        return true
    }

    func discardPreparedSuccessor() {
        preparedPlayer?.replaceCurrentItem(with: nil)
        preparedPlayer = nil; preparedItem = nil; preparedOutput = nil
        preparedFilmPositionMs = 0; preparedSeekPending = false
    }

    /// Local's commit, for one AVPlayer the view owns: align the successor to
    /// where the incumbent is, move its item onto the viewer's player, and
    /// report its own first qualifying frame. The predecessor is not DELETEd;
    /// the committed exchange is what retires it on B.
    func commitPreparedSuccessor(_ action: PreparedReplacementAction) async -> PreparedCommitOutcome {
        // A paused viewer renders no new frame, so a paused switch could only
        // time out: it takes the reopen instead, as Local does.
        guard !closing, playing, let successor = preparedPlayer, let item = preparedItem, let output = preparedOutput,
              player.currentItem != nil, let adoption = preparedAdoption,
              adoption.playback.start.response.sessionId == action.sessionId
        else { return .refused }
        // One future switch point, chosen before either bounded suspension,
        // exactly as Local does: inspection and alignment can each take up to
        // `alignmentMs`, and a point taken from the playhead after inspecting
        // routinely lands in the next fragment, refusing a healthy handoff.
        let rate = Double(player.rate)
        guard rate.isFinite, rate > 0,
              let leadWallMs = PreparedCommitRendezvous.commitLeadWallMs(overlapRemainingMs: .max) else { return .refused }
        let rendezvous = PreparedCommitRendezvous.plan(stagedFilmPositionMs: preparedFilmPositionMs,
                                                       incumbentFilmPositionMs: currentPositionMs(), mediaOriginMs: 0,
                                                       leadMs: Int(min(16, rate) * Double(leadWallMs)))
        guard let cadence = await preparedDecodedFrameDuration(of: item, boundMs: PreparedReplacementBounds.alignmentMs,
                targetItemSeconds: Double(rendezvous.itemPositionMs) / 1000),
              !closing, playing, preparedItem === item, preparedPlayer === successor else { return .refused }
        guard cadence.covers(itemPositionMs: rendezvous.itemPositionMs) else { return .refused }
        let frameDurationSeconds = cadence.frameDurationSeconds
        guard await alignPrepared(item, to: rendezvous.itemPositionMs), !closing,
              preparedItem === item, preparedPlayer === successor else {
            discardPreparedSuccessor()
            return PreparedCommitRendezvous.outcomeWhenAlignmentCannotLand
        }
        // The parked successor waits at the switch point for the incumbent,
        // so the swap neither skips nor repeats film.
        guard await awaitSharedRendezvous(rendezvous, frameDurationSeconds: frameDurationSeconds,
                                          boundMs: leadWallMs + PreparedReplacementBounds.alignmentMs),
              !closing, playing, preparedItem === item, preparedPlayer === successor else {
            discardPreparedSuccessor()
            return PreparedCommitRendezvous.outcomeWhenAlignmentCannotLand
        }
        successor.replaceCurrentItem(with: nil)
        guard successor.currentItem == nil else { discardPreparedSuccessor(); return .refused }
        preparedPlayer = nil; preparedItem = nil; preparedOutput = nil
        observe(item)
        player.replaceCurrentItem(with: item)
        preparedSwapped = true
        await applySubtitle(adoption.plan, item: item)
        player.play()
        guard let frame = await awaitPreparedFirstFrame(output, boundaryMs: rendezvous.filmPositionMs, frameDurationSeconds: frameDurationSeconds) else {
            return .switchedWithoutAFrame
        }
        return .committed(firstFrameUnixMs: frame)
    }

    /// Wait for the incumbent to reach the planned switch point. Arriving
    /// within one frame is the meeting; passing it means the point was missed.
    private func awaitSharedRendezvous(_ rendezvous: PreparedCommitRendezvous,
                                       frameDurationSeconds: Double, boundMs: Int) async -> Bool {
        let began = Int(ProcessInfo.processInfo.systemUptime * 1_000)
        while Int(ProcessInfo.processInfo.systemUptime * 1_000) - began < boundMs {
            guard !Task.isCancelled, !closing, playing, player.rate > 0 else { return false }
            let drift = Double(currentPositionMs()) - Double(rendezvous.filmPositionMs)
            if abs(drift) <= frameDurationSeconds * 1_000 { return true }
            if drift > 0 { return false }
            try? await Task.sleep(nanoseconds: 8_000_000)
        }
        return false
    }

    /// The P0 reopen, run by the preparation once its settlement is sent.
    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction) { preparedFallbackOwed = true }
    /// The preparation loop delivers whatever the ledger owes, in order.
    func preparedSuccessorOwesAnExchange() {}
    func notePreparedSuccessorAbandoned(_ reason: PreparedReplacementAbandonment) {}
    func recordPreparedFallbackInterruption(ms: Int) {}
}

/// Actual successor cadence, bounded by its caller's existing preparation budget.
/// Both Local and Shared use the track's sample grid; absence never invents fps.
@MainActor
func preparedDecodedFrameDuration(
    of item: AVPlayerItem, boundMs remaining: Int, targetItemSeconds: Double,
    report: (@MainActor (String) -> Void)? = nil
) async -> PreparedDecodedCadence? {
        guard remaining > 0 else { return nil }
        var result: PreparedDecodedCadence?
        var finished = false
        let began = ProcessInfo.processInfo.systemUptime
        let task = Task { @MainActor in
            defer { finished = true }
            do {
                let tracks = try await item.asset.loadTracks(withMediaType: .video)
                guard !Task.isCancelled else { return }
                report?("tracks=\(tracks.count)")
                // Inspect ready-item track presence without substituting
                // nominal cadence for the required minimum sample duration.
                let presentationTracks = item.tracks.filter { $0.isEnabled }.compactMap(\.assetTrack).filter { $0.mediaType == .video }
                report?("item_tracks=\(item.tracks.count):video_tracks=\(presentationTracks.count)")
                let duration = try await tracks.first?.load(.minFrameDuration)
                guard !Task.isCancelled else { return }
                let seconds = duration?.seconds ?? 0
                let valid = seconds.isFinite && seconds > 0 && seconds <= 1
                report?("duration_valid=\(valid):timescale=\(duration?.timescale ?? 0):value=\(duration?.value ?? 0)")
                if valid {
                    result = PreparedDecodedCadence(frameDurationSeconds: seconds)
                    return
                }
            } catch {
                if !Task.isCancelled { report?("metadata_error_code=\((error as NSError).code)") }
            }
            guard !Task.isCancelled, let asset = item.asset as? AVURLAsset else { return }
            let left = remaining - Int((ProcessInfo.processInfo.systemUptime - began) * 1000)
            let inspected = await HLSFragmentCadence.inspect(url: asset.url, itemSeconds: targetItemSeconds, boundMs: left)
            guard !Task.isCancelled else { return }
            result = inspected
            report?("fragment_verified=\(inspected != nil)")
        }
        let cadence = await awaitBoundedValue(
            boundMs: remaining, pollMs: PreparedReplacementBounds.pollMs,
            now: { Int(ProcessInfo.processInfo.systemUptime * 1_000) },
            sleep: { try? await Task.sleep(nanoseconds: UInt64($0) * 1_000_000) },
            read: { Task.isCancelled ? PreparedDecodedCadence(frameDurationSeconds: 0)
                : finished ? (result ?? PreparedDecodedCadence(frameDurationSeconds: 0)) : nil }
        )
        task.cancel()
        if cadence == nil { report?("deadline_exhausted=true") }
        return cadence.flatMap { $0.frameDurationSeconds > 0 ? $0 : nil }
}
