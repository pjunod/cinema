import AVKit
import SwiftUI

/// Kept for the app lifetime: leaving and reopening a tab cannot forget an
/// unconfirmed cleanup or a lost start response's expiry barrier.
@MainActor
final class LiveTvPlayerController: ObservableObject {
    static let shared = LiveTvPlayerController()

    /// A controller wired to stub requests, for tests that need to count what
    /// actually reached the server rather than assert a constant.
    static func testing(
        requests: LiveTvRequests,
        activateAudioSession: @escaping () -> Void = {},
        deactivateAudioSession: @escaping () -> Void = {}
    ) -> LiveTvPlayerController {
        let controller = LiveTvPlayerController()
        controller.lease = LiveTvLease(requests: requests)
        controller.activateAudioSession = activateAudioSession
        controller.deactivateAudioSession = deactivateAudioSession
        // A real client only so `watch` gets past its own guard; it is never
        // asked for the network, because the stub lease answers first.
        controller.api = LiveTvAPI(origin: "http://127.0.0.1:1", token: nil)
        controller.channels = []
        return controller
    }
    @Published private(set) var channels: [LiveTvChannel] = []
    @Published private(set) var message = "Choose a channel to watch live."
    @Published private(set) var title: String?
    @Published private(set) var busy = false
    @Published private(set) var playing = false
    @Published private(set) var paused = false
    /// True only for a sustained mid-stream stall. AVPlayer's ordinary
    /// buffering-rate evaluation at startup is not something the viewer
    /// should be told about.
    @Published private(set) var waiting = false
    /// Distance from the playhead to the edge the current playlist offers.
    /// This is not end-to-end broadcast latency.
    @Published private(set) var behindEdgeSeconds: Double?
    /// Media available after the playhead in the loaded range that contains it.
    @Published private(set) var bufferedSeconds: Double?
    @Published private(set) var pausedAt: Date?
    @Published private(set) var attachedAt: Date?
    /// Fullscreen copy only. Lineup status in `message` never reaches the
    /// picture merely because the browser loaded or refreshed.
    @Published private(set) var surfaceMessage: String?
    /// The guide is a second, independent read. It never gates the lineup and
    /// never gates a start: a page that waited on it would be a page that
    /// cannot tune while a guide host is slow.
    @Published private(set) var guide: LiveTvGuide?
    @Published private(set) var watching: LiveTvChannel?
    @Published private(set) var status: LiveTvStatus?
    @Published private(set) var delivery: LiveTvDelivery?
    let player = AVPlayer()
    private var api: LiveTvAPI?
    private var lease: LiveTvLease?
    private var profileOrigin: String?
    private var profileToken: String?
    private var serial = 0
    private var loadId = UUID()
    private var heartbeat: Task<Void, Never>?
    private var guideRefresh: Task<Void, Never>?
    private var channelChange: Task<Void, Never>?
    private var timeControlObservation: NSKeyValueObservation?
    private var waitingDebounce: Task<Void, Never>?
    private var ownsAudioSession = false
    private var activateAudioSession: () -> Void = {
#if os(iOS)
        try? AVAudioSession.sharedInstance().setCategory(.playback)
        try? AVAudioSession.sharedInstance().setActive(true)
#endif
    }
    private var deactivateAudioSession: () -> Void = {
#if os(iOS)
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
#endif
    }

    func load(origin: String, token: String?) async {
        let loading = UUID()
        loadId = loading
        do {
            if profileOrigin != origin || profileToken != token || api == nil {
                channels = []
                try await stopChecked()
                guard loadId == loading else { return }
                let client = LiveTvAPI(origin: origin, token: token)
                api = client
                lease = LiveTvLease(requests: client)
                profileOrigin = origin
                profileToken = token
            }
            guard let api else { return }
            let lineup = try await api.lineup()
            guard loadId == loading else { return }
            channels = lineup.channels.map { channel in
                watching?.id == channel.id ? (watching ?? channel) : channel
            }
            expireSourceFormats(now: Int(Date().timeIntervalSince1970))
            startGuideRefresh(loading)
            message = lineup.freshness == "stale"
                ? "Cached lineup (\(lineup.ageSeconds)s old). Starting a channel requires a fresh tuner check."
                : "\(channels.count) channels. DRM-protected channels cannot be played."
            await resumeIfRecent(loading)
        } catch {
            if loadId == loading { message = error.localizedDescription }
        }
    }

    /// The open-time step of the start contract: a session an unclean end left
    /// running comes back with no press at all. It is not an auto-tune — the
    /// owner only ever answers `live` for a session this viewer already owns,
    /// and every other answer leaves the channel list on screen.
    private func resumeIfRecent(_ loading: UUID) async {
        guard !playing, watching == nil, let lease, let api else { return }
        guard let resumed = await lease.resumeIfRecent(), loadId == loading else { return }
        serial += 1
        let expected = serial
        do {
            try attach(resumed, channel: resumed.channel, api: api,
                       expected: expected, compatibilityRetry: false)
        } catch {
            message = error.localizedDescription
            do { try await lease.stop() } catch { message += " Cleanup is unconfirmed; use Stop to retry." }
            endAudioSession()
        }
    }

    func watch(_ channel: LiveTvChannel, compatibilityRetry: Bool = false) async {
        guard channel.watchable, let lease, let api else { return }
        // A second tune while the first is starting used to be dropped in
        // silence — the viewer pressed a channel and nothing happened at all.
        // Bumping the serial supersedes the in-flight start instead: the lease
        // serialises the release, so this is still exactly one live session.
        if busy { serial += 1 }
        serial += 1
        let expected = serial
        detach()
        busy = true
        message = "Starting \(channel.title)…"
        do {
            let info = try await lease.start(channel.id)
            guard serial == expected, let info else { return }
            try attach(info, channel: channel, api: api,
                       expected: expected, compatibilityRetry: compatibilityRetry)
        } catch {
            guard serial == expected else { return }
            message = error.localizedDescription
            // A URL/attachment failure after acquiring a capability must also
            // release it. The lease retains ownership if cleanup cannot finish.
            do { try await lease.stop() } catch { message += " Cleanup is unconfirmed; use Stop to retry." }
            endAudioSession()
        }
        if serial == expected { busy = false }
    }

    /// Everything a granted session does after the POST answers: the player
    /// item, the audio session, the published state and the heartbeat. A
    /// resume enters playback through this exact path, so a session recovered
    /// at open time is indistinguishable from one the viewer just pressed for.
    private func attach(
        _ info: LiveTvStarted,
        channel: LiveTvChannel,
        api: LiveTvAPI,
        expected: Int,
        compatibilityRetry: Bool
    ) throws {
        let item = AVPlayerItem(url: try api.playlistURL(info.sessionId))
        item.preferredForwardBufferDuration = 12
        player.replaceCurrentItem(with: item)
        // Live TV owns a separate AVPlayer from finite-media playback, so
        // it must establish the same playback audio session itself. The
        // default category follows the iPhone silent switch: video moves,
        // but the AAC track is inaudible.
        beginAudioSession()
        title = channel.title
        watching = info.channel
        delivery = info.delivery
        attachedAt = Date()
        playing = true
        player.play()
        surfaceMessage = nil
        message = "Playing live"
        timeControlObservation = player.observe(
            \.timeControlStatus,
            options: [.initial, .new]
        ) { [weak self] player, _ in
            let status = player.timeControlStatus
            let reason = player.reasonForWaitingToPlay
            Task { @MainActor [weak self] in
                guard let self, self.serial == expected else { return }
                self.applyTimeControl(status: status, reason: reason)
            }
        }
        heartbeat = Task { @MainActor [weak self] in
            var progress = LiveTvPlaybackWatchdog()
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 5_000_000_000) } catch { return }
                guard let self, self.serial == expected else { return }
                do {
                    if item.status == .failed { throw Self.playerFailure(item.error) }
                    let position = self.player.currentTime().seconds
                    self.sampleLiveEdge(item: item, position: position)
                    if !self.paused && progress.observe(position: position) {
                        try await api.keepalive(info.sessionId)
                        guard self.serial == expected else { return }
                        // The other half of the keepalive: the hint's
                        // `touched_at` is what tells a later press how long
                        // ago something was really watching this start.
                        self.lease?.touchHint()
                        let status = try await api.status(info.sessionId)
                        guard self.serial == expected else { return }
                        if status.state != "active" { throw LiveTvFailure(code: "stream_failed") }
                        if let observed = status.channel,
                           observed.id == self.watching?.id {
                            self.watching = observed
                            self.channels = self.channels.map {
                                $0.id == observed.id ? observed : $0
                            }
                        }
                        self.status = status
                        self.delivery = status.delivery ?? info.delivery
                        self.expireSourceFormats(now: Int(Date().timeIntervalSince1970))
                    } else if progress.expired {
                        if self.paused {
                            self.message = "Paused for 30 seconds. The tuner was released; select a channel to resume live."
                            self.surfaceMessage = self.message
                            await self.stop()
                            return
                        }
                        throw LiveTvFailure(code: "stream_failed")
                    }
                } catch {
                    guard self.serial == expected else { return }
                    let failureCode = (error as? LiveTvFailure)?.code ?? ""
                    if ["codec_unsupported", "source_format_changed"].contains(failureCode),
                       !compatibilityRetry {
                        self.message = failureCode == "source_format_changed"
                            ? "The broadcast changed format. Selecting a fresh route once…"
                            : "The original route was rejected. Retrying once with a compatible conversion…"
                        self.surfaceMessage = self.message
                        do { try await self.stopChecked() }
                        catch {
                            self.message = "Cleanup is unconfirmed; retry Stop before opening another channel."
                            self.surfaceMessage = self.message
                            return
                        }
                        if failureCode == "codec_unsupported" {
                            api.retryCompatibility(LiveTvCompatibility(
                                failedVideo: true, failedAudio: true, failedContainer: true
                            ))
                        }
                        await self.watch(channel, compatibilityRetry: true)
                        return
                    }
                    self.message = error.localizedDescription
                    self.surfaceMessage = self.message
                    await self.stop()
                    return
                }
            }
        }
    }

    /// Where the next guide poll lands, in seconds from now.
    ///
    /// **The owner's clock wins whenever it exists.** A document that carries
    /// `next_refresh_at` is polled on that — `unavailable` ones included. The
    /// plan specified a flat `guide_poll_unavailable_s` for an unavailable
    /// guide; now that the owner serves `next_refresh_at` on unavailable
    /// answers too, an owner that knows when it comes back is a better answer
    /// than a constant, and that constant is the fallback for a document that
    /// says nothing. (A deliberate, recorded deviation from §3.16, shared with
    /// the web and Android reducers.)
    ///
    /// **The clamp guards the wire, not the contract.** Only the
    /// `next_refresh_at` branch is clamped, because only it reads a number off
    /// the network: a far-future answer must not park the grid for hours, and
    /// one far in the past must not spin the loop. The two early returns are
    /// contract constants handed back verbatim — clamping those would make the
    /// client disagree with the value the shared fixture pins, which is a worse
    /// failure than the one it would prevent.
    ///
    /// **The floor is applied last, so the floor wins.** If `guide_poll_min_s`
    /// and `guide_poll_ceiling_s` ever crossed, clamping ceiling-last would
    /// return a delay *below* the floor and poll a struggling owner harder than
    /// the contract allows — the dangerous direction. Every number comes from
    /// `tests/playback/player-input-contract.json` `live.timings`.
    static func guidePollSeconds(nextRefreshAt: Int?, freshness: String, now: Int) -> Int {
        let ceiling = LiveTvInputRouting.guidePollCeilingSeconds
        let floor = LiveTvInputRouting.guidePollMinSeconds
        guard let next = nextRefreshAt else {
            return freshness == "unavailable" ? LiveTvInputRouting.guidePollUnavailableSeconds : floor
        }
        // Bound the gap before adding to it: `next` is off the wire and
        // `gap + after` would otherwise overflow on a value near `Int.max`.
        let difference = next.subtractingReportingOverflow(now)
        let gap = difference.overflow ? 0 : min(max(difference.partialValue, -ceiling), ceiling)
        return max(min(gap + LiveTvInputRouting.guidePollAfterNextRefreshSeconds, ceiling), floor)
    }

    private func startGuideRefresh(_ loading: UUID) {
        guideRefresh?.cancel()
        guideRefresh = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self, self.loadId == loading, let api = self.api else { return }
                // A guide that will not load leaves a working page: the rows
                // fall back to number and callsign and nothing else changes.
                // Ask for at least what the grid draws. The default window is
                // `now - 3600 … now - 3600 + guide_hours`, so with the common
                // 4-hour setting the right-hand columns were permanently empty
                // even where the owner had data.
                let current = Int(Date().timeIntervalSince1970)
                let from = current - current % LiveTvGridMetrics.slotSeconds
                // A read that failed carries exactly the information an
                // `unavailable` document with no `next_refresh_at` carries —
                // the owner has nothing to give and has not said when it will
                // — so it is paced the same way, and all three clients agree.
                // The floor would poll a struggling owner twice as hard for no
                // extra information.
                var seconds = LiveTvInputRouting.guidePollUnavailableSeconds
                if let fetched = try? await api.guide(
                    from: from, hours: LiveTvGridMetrics.requestedHours) {
                    guard self.loadId == loading else { return }
                    self.guide = fetched
                    seconds = Self.guidePollSeconds(
                        nextRefreshAt: fetched.nextRefreshAt,
                        freshness: fetched.freshness,
                        now: Int(Date().timeIntervalSince1970))
                }
                do { try await Task.sleep(nanoseconds: UInt64(seconds) * 1_000_000_000) } catch { return }
            }
        }
    }

    func airing(_ channel: LiveTvChannel, now: Int = Int(Date().timeIntervalSince1970)) -> LiveTvAiring {
        LiveTvGuideReducer.airing(guide, channelId: channel.id, now: now)
    }

    func expireSourceFormats(now: Int) {
        func fresh(_ channel: LiveTvChannel) -> LiveTvChannel {
            guard let observed = channel.sourceFormat?.observedAt else { return channel }
            let observedSeconds = Int(observed)
            let programmeEnd = guide?.channels.first(where: { $0.id == channel.id })?
                .programmes.first(where: {
                    $0.start <= observedSeconds && $0.end > observedSeconds
                })?.end
            let ttlEnd = observedSeconds > Int.max - 20 * 60
                ? Int.max : observedSeconds + 20 * 60
            let expiry = programmeEnd.map { min(ttlEnd, $0) } ?? ttlEnd
            return now < expiry ? channel : channel.removingSourceFormat()
        }
        channels = channels.map(fresh)
        if let watching { self.watching = fresh(watching) }
    }

    /// A held channel key is one tuner start, not ten. The contract's 350 ms of
    /// stillness is the whole guardrail against a channel-surf storm.
    /// A coalesced tune. Every repeat inside the contract's window replaces
    /// the last, so a held direction or a fast run along the neighbour strip
    /// costs one tuner start rather than one per press.
    func requestChannel(_ channel: LiveTvChannel) {
        channelChange?.cancel()
        channelChange = Task { @MainActor [weak self] in
            let delay = UInt64(LiveTvInputRouting.channelCoalesceMilliseconds) * 1_000_000
            do { try await Task.sleep(nanoseconds: delay) } catch { return }
            guard let self, !Task.isCancelled else { return }
            await self.watch(channel)
        }
    }

    static func playerFailure(_ error: Error?) -> LiveTvFailure {
        let error = error as NSError?
        let decodeCodes = [AVError.Code.decodeFailed.rawValue, AVError.Code.decoderNotFound.rawValue]
        return LiveTvFailure(code: error?.domain == AVFoundationErrorDomain && decodeCodes.contains(error?.code ?? 0)
                             ? "codec_unsupported" : "stream_failed")
    }

    /// Pure classification kept separate from the debounce so tests can cover
    /// every AVPlayer reason without constructing a player item.
    static func waitingDecision(
        status: AVPlayer.TimeControlStatus,
        reason: AVPlayer.WaitingReason?
    ) -> Bool {
        status == .waitingToPlayAtSpecifiedRate && reason == .toMinimizeStalls
    }

    /// The observation seam: production KVO and tests enter through the same
    /// debounce, while neither needs to manufacture a fake AVPlayer.
    func applyTimeControl(
        status: AVPlayer.TimeControlStatus,
        reason: AVPlayer.WaitingReason?
    ) {
        let expected = serial
        waitingDebounce?.cancel()
        waitingDebounce = nil
        guard Self.waitingDecision(status: status, reason: reason) else {
            waiting = false
            return
        }
        waitingDebounce = Task { @MainActor [weak self] in
            do { try await Task.sleep(nanoseconds: 350_000_000) } catch { return }
            guard !Task.isCancelled, let self, self.serial == expected else { return }
            self.waiting = true
        }
    }

    private func sampleLiveEdge(item: AVPlayerItem, position: Double) {
        guard position.isFinite else {
            behindEdgeSeconds = nil
            bufferedSeconds = nil
            return
        }
        let seekable = item.seekableTimeRanges.compactMap { value -> Double? in
            let range = value.timeRangeValue
            let end = CMTimeGetSeconds(CMTimeRangeGetEnd(range))
            return end.isFinite ? end : nil
        }
        behindEdgeSeconds = seekable.max().map { max(0, $0 - position) }

        let loaded = item.loadedTimeRanges.compactMap { value -> (Double, Double)? in
            let range = value.timeRangeValue
            let start = CMTimeGetSeconds(range.start)
            let end = CMTimeGetSeconds(CMTimeRangeGetEnd(range))
            guard start.isFinite, end.isFinite else { return nil }
            return (start, end)
        }
        guard !loaded.isEmpty else {
            bufferedSeconds = nil
            return
        }
        bufferedSeconds = loaded.first(where: { $0.0 <= position && position < $0.1 })
            .map { max(0, $0.1 - position) } ?? 0
    }

    func togglePause() {
        guard playing else { return }
        paused.toggle()
        if paused {
            player.pause()
            pausedAt = Date()
            message = "Paused. The tuner is released after 30 seconds without playback; resuming has no rewind guarantee."
            surfaceMessage = message
        } else {
            player.play()
            pausedAt = nil
            message = "Playing live"
            surfaceMessage = nil
        }
    }

    func stop(clearProfile: Bool = false) async {
        if clearProfile { loadId = UUID(); channels = [] }
        do { try await stopChecked() }
        catch { message = "Cleanup is unconfirmed. Retry Stop before starting another channel." }
    }

    private func stopChecked() async throws {
        serial += 1
        detach()
        busy = false
        endAudioSession()
        try await lease?.stop()
    }

    private func beginAudioSession() {
        guard !ownsAudioSession else { return }
        activateAudioSession()
        ownsAudioSession = true
    }

    private func endAudioSession() {
        guard ownsAudioSession else { return }
        ownsAudioSession = false
        deactivateAudioSession()
    }

    private func detach() {
        heartbeat?.cancel()
        heartbeat = nil
        channelChange?.cancel()
        channelChange = nil
        timeControlObservation?.invalidate()
        timeControlObservation = nil
        waitingDebounce?.cancel()
        waitingDebounce = nil
        waiting = false
        behindEdgeSeconds = nil
        bufferedSeconds = nil
        pausedAt = nil
        attachedAt = nil
        watching = nil
        status = nil
        delivery = nil
        player.pause()
        player.replaceCurrentItem(with: nil)
        title = nil
        playing = false
        paused = false
    }
}

/// How the browse region is drawn. Persisted, because the choice between
/// surfing a list and planning off a grid is a habit rather than a mode.
enum LiveTvBrowseView: String, CaseIterable, Identifiable {
    case list
    case guide
    case recordings

    var id: Self { self }
    var label: String {
        switch self {
        case .list: return "On now"
        case .guide: return "Guide"
        case .recordings: return "Recordings"
        }
    }

    // `plurx.`-prefixed, like every other key in SettingsStore. (The plan
    // wrote this as a bare `liveTvView`; that would have been the only
    // unprefixed key in the client, so it is spelled the house way here.)
    private static let defaultsKey = "plurx.liveTvView"

    static func persisted(_ defaults: UserDefaults = .standard) -> Self {
        guard let raw = defaults.string(forKey: defaultsKey), let view = Self(rawValue: raw)
        else { return .list }
        return view
    }

    func persist(_ defaults: UserDefaults = .standard) {
        defaults.set(rawValue, forKey: Self.defaultsKey)
    }
}

/// One grid's drawn geometry. Passed in rather than read from the statics, so
/// the slot width can follow the screen on a television without the phone's
/// fixed columns moving.
struct LiveTvGridDimensions: Equatable {
    let slotWidth: Double
    let rowHeight: Double
    let channelColumnWidth: Double
}

/// Half-hour geometry, shared by the phone grid and the tests.
enum LiveTvGridMetrics {
    static let slotSeconds = 1800
    #if os(iOS)
    static let rowHeight: Double = 56
    static let channelColumnWidth: Double = 128
    static let visibleSlots = 8
    static let horizontalInset: Double = 16
    /// The phone grid scrolls horizontally, so its column is a fixed size
    /// rather than a share of a width it does not have.
    static func pxPerSlot(contentWidth: Double) -> Double { 160 }
    #else
    static let rowHeight: Double = 74
    static let channelColumnWidth: Double = 200
    static let visibleSlots = 4
    /// The grid draws 8 pt of padding on each side of its own content, so the
    /// slots have `contentWidth - 16` to share with the channel column.
    static let horizontalInset: Double = 16
    /// Slot width follows the screen: a hard-coded 300 left 35% of a 1920 pt
    /// screen empty and could never fit two hours.
    static func pxPerSlot(contentWidth: Double) -> Double {
        max(120, (contentWidth - horizontalInset - channelColumnWidth) / Double(visibleSlots))
    }
    #endif

    static func dimensions(contentWidth: Double) -> LiveTvGridDimensions {
        LiveTvGridDimensions(
            slotWidth: pxPerSlot(contentWidth: contentWidth),
            rowHeight: rowHeight,
            channelColumnWidth: channelColumnWidth
        )
    }

    /// The hours the client must ask for to fill `visibleSlots`, plus the
    /// hour of backfill the server prepends and one for the partial slot.
    static var requestedHours: Int { 6 }

    static func window(now: Int) -> LiveTvGuideWindow {
        let start = now - now % slotSeconds
        return window(start: start)
    }

    static func window(start: Int) -> LiveTvGuideWindow {
        return LiveTvGuideWindow(start: start, end: start + visibleSlots * slotSeconds)
    }
}

/// One explicit scale for the whole screen. tvOS resolves the semantic styles
/// two to two and a half times larger than iOS does — `.subheadline` is 38 pt
/// and `.title2` 57 pt there — which is what made channel rows three feet
/// long. The iOS values below are exactly what the phone drew before, so the
/// phone does not move.
enum LiveTvType {
    #if os(tvOS)
    static let title = Font.system(size: 30, weight: .semibold)
    static let primary = Font.system(size: 22, weight: .semibold)
    static let cell = Font.system(size: 22)
    static let secondary = Font.system(size: 20)
    static let tertiary = Font.system(size: 18)
    static let badge = Font.system(size: 16, weight: .bold)
    static let eyebrow = Font.system(size: 14, weight: .bold)
    static let chip = Font.system(size: 18, weight: .bold)
    static let surfaceTitle = Font.system(size: 46, weight: .semibold)
    static let surfaceBody = Font.system(size: 24)
    static let surfaceButton = Font.system(size: 24, weight: .semibold)
    static let surfaceMono = Font.system(size: 20, weight: .medium, design: .monospaced)
    static let surfaceEyebrow = Font.system(size: 22, weight: .medium, design: .monospaced)
    static let surfaceHint = Font.system(size: 18, design: .monospaced)
    #else
    static let title = Font.headline
    static let primary = Font.subheadline.weight(.semibold)
    static let cell = Font.caption
    static let secondary = Font.caption
    static let tertiary = Font.caption2
    static let badge = Font.system(size: 9, weight: .bold)
    static let eyebrow = Font.caption2.weight(.bold)
    static let chip = Font.system(size: 10.5, weight: .bold)
    #endif
}

struct LiveSurfaceChip: Equatable, Identifiable {
    enum Kind: String {
        case live, behind, signal, method, audio, clock
    }

    let kind: Kind
    let text: String
    var id: Kind { kind }
}

struct LiveTvAccessEventFacts: Equatable {
    let observedBitrate: Double
    let droppedFrames: Int
    let stalls: Int
}

struct LiveTvPlayerFacts: Equatable {
    let behindEdgeSeconds: Double?
    let bufferedSeconds: Double?
    let observedBitrate: Double?
    let droppedFrames: Int?
    let stalls: Int?
    let hasAccessEvents: Bool
    let attachedAt: Date?
    let asOf: Date

    static func capture(
        item: AVPlayerItem?,
        behindEdgeSeconds: Double?,
        bufferedSeconds: Double?,
        attachedAt: Date?,
        asOf: Date = Date()
    ) -> Self {
        let events = item?.accessLog()?.events.map {
            LiveTvAccessEventFacts(
                observedBitrate: $0.observedBitrate,
                droppedFrames: $0.numberOfDroppedVideoFrames,
                stalls: $0.numberOfStalls
            )
        } ?? []
        return from(
            events: events,
            behindEdgeSeconds: behindEdgeSeconds,
            bufferedSeconds: bufferedSeconds,
            attachedAt: attachedAt,
            asOf: asOf
        )
    }

    static func from(
        events: [LiveTvAccessEventFacts],
        behindEdgeSeconds: Double?,
        bufferedSeconds: Double?,
        attachedAt: Date?,
        asOf: Date
    ) -> Self {
        func sumKnown(_ values: [Int]) -> Int? {
            let known = values.filter { $0 >= 0 }
            return known.isEmpty ? nil : known.reduce(0, +)
        }
        let rate = events.last?.observedBitrate
        return Self(
            behindEdgeSeconds: behindEdgeSeconds,
            bufferedSeconds: bufferedSeconds,
            observedBitrate: rate.flatMap { $0 >= 0 && $0.isFinite ? $0 : nil },
            droppedFrames: sumKnown(events.map(\.droppedFrames)),
            stalls: sumKnown(events.map(\.stalls)),
            hasAccessEvents: !events.isEmpty,
            attachedAt: attachedAt,
            asOf: asOf
        )
    }
}

struct LiveTvStreamInfoRow: Equatable, Identifiable {
    enum Section: String, CaseIterable {
        case programme = "PROGRAMME"
        case channel = "CHANNEL"
        case delivery = "DELIVERY"
        case signal = "SIGNAL"
        case player = "PLAYER"
    }

    let section: Section
    let label: String
    let value: String
    var percent: Int? = nil
    var id: String { "\(section.rawValue)-\(label)" }
}

private let liveTvClock: DateFormatter = {
    let formatter = DateFormatter()
    formatter.timeStyle = .short
    formatter.dateStyle = .none
    return formatter
}()

func liveTvTime(_ unix: Int) -> String {
    liveTvClock.string(from: Date(timeIntervalSince1970: TimeInterval(unix)))
}

func liveTvTechnicalSummary(_ channel: LiveTvChannel, status: LiveTvStatus?, delivery: LiveTvDelivery? = nil) -> String {
    var facts = [String]()
    facts.append((status?.delivery ?? delivery)?.playbackMethod ?? "Playback method unavailable")
    if let source = channel.sourceFormatDescription { facts.append("Source: \(source)") }
    if let value = status?.signal?.strengthPercent { facts.append("strength \(value)%") }
    if let value = status?.signal?.qualityPercent { facts.append("quality \(value)%") }
    if let value = status?.signal?.symbolQualityPercent { facts.append("symbol \(value)%") }
    return facts.joined(separator: " · ")
}

struct LiveTvFormatBadges: View {
    let channel: LiveTvChannel

    var body: some View {
        if !channel.formatBadges.isEmpty {
            HStack(spacing: 4) {
                ForEach(channel.formatBadges, id: \.self) { badge in
                    Text(badge)
                        .font(LiveTvType.badge)
                        .foregroundStyle(Palette.muted)
                        .padding(.horizontal, 5).padding(.vertical, 2)
                        .overlay(Capsule().stroke(Palette.outline))
                }
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Source format: \(channel.formatBadges.joined(separator: ", "))")
        }
    }
}

struct LiveTvTechnicalDetails: View {
    let channel: LiveTvChannel
    let status: LiveTvStatus?
    var delivery: LiveTvDelivery? = nil

    private var plan: LiveTvDelivery? { status?.delivery ?? delivery }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            if let source = channel.sourceFormatDescription {
                detailRow("Source", source)
            }
            if let observed = channel.sourceFormat?.observedAt {
                detailRow(
                    "Observed",
                    Date(timeIntervalSince1970: TimeInterval(observed)).formatted(date: .abbreviated, time: .shortened)
                )
            }
            detailRow("Method", plan?.playbackMethod ?? "Playback method unavailable")
            if let plan {
                detailRow("Video", plan.videoDescription)
                detailRow("Audio", plan.audioDescription)
                detailRow("Stream", "HLS · \(plan.packaging.uppercased())")
                if plan.videoAction == "encode", let encoder = status?.encoder, encoder != "pending" {
                    detailRow("Encoder", encoder)
                }
            }
            if let signal = status?.signal {
                VStack(alignment: .leading, spacing: 5) {
                    Text("SIGNAL").font(.system(size: 9, weight: .bold)).foregroundStyle(Palette.muted)
                    HStack(spacing: 10) {
                        signalMeter("Strength", signal.strengthPercent)
                        signalMeter("Quality", signal.qualityPercent)
                        signalMeter("Symbol", signal.symbolQualityPercent)
                    }
                }
            }
        }
        .padding(.top, 7)
    }

    private func detailRow(_ label: String, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            Text(label.uppercased()).font(.system(size: 9, weight: .bold))
                .foregroundStyle(Palette.muted).frame(width: 54, alignment: .leading)
            Text(value).font(.caption).foregroundStyle(Palette.muted)
        }
    }

    @ViewBuilder private func signalMeter(_ label: String, _ value: Int?) -> some View {
        if let value {
            VStack(alignment: .leading, spacing: 2) {
                Text("\(label) \(value)%").font(.caption2).foregroundStyle(Palette.muted)
                ProgressView(value: Double(value), total: 100).tint(Palette.accent)
            }
        }
    }
}

#if os(tvOS)
struct LiveTvStreamInfoPanel: View {
    let programme: LiveTvAiring
    let channel: LiveTvChannel
    let status: LiveTvStatus?
    let delivery: LiveTvDelivery?
    let player: LiveTvPlayerFacts
    let onClose: () -> Void
    @FocusState private var closeFocused: Bool

    static func rows(
        programme: LiveTvAiring,
        channel: LiveTvChannel,
        status: LiveTvStatus?,
        delivery: LiveTvDelivery?,
        player: LiveTvPlayerFacts
    ) -> [LiveTvStreamInfoRow] {
        var rows = [LiveTvStreamInfoRow]()
        func append(
            _ section: LiveTvStreamInfoRow.Section,
            _ label: String,
            _ value: String?,
            percent: Int? = nil
        ) {
            guard let value, !value.isEmpty else { return }
            rows.append(LiveTvStreamInfoRow(
                section: section, label: label, value: value, percent: percent
            ))
        }

        if let current = programme.now {
            append(.programme, "title", current.title)
            let episode = [current.episode, current.episodeTitle]
                .compactMap { $0 }.joined(separator: " · ")
            append(.programme, "episode", episode)
            let minutes = max(0, (current.end - Int(player.asOf.timeIntervalSince1970)) / 60)
            append(
                .programme,
                "airing",
                "\(liveTvTime(current.start))–\(liveTvTime(current.end)) · \(minutes) min left"
            )
            append(
                .programme,
                "next",
                programme.next.map { "\(liveTvTime($0.start)) · \($0.title)" }
            )
            append(.programme, "synopsis", current.synopsis)
            let aired = [
                current.originalAirDate.map { "first aired \($0)" },
                current.filters?.joined(separator: " · "),
            ].compactMap { $0 }.filter { !$0.isEmpty }.joined(separator: " · ")
            append(.programme, "aired", aired)
        }

        append(
            .channel,
            "channel",
            [
                channel.guideNumber,
                channel.guideName,
                channel.pictureClass,
                channel.favorite ? "favorite on the tuner" : nil,
            ].compactMap { $0 }.joined(separator: " · ")
        )
        append(.channel, "source", channel.sourceFormatDescription)
        append(
            .channel,
            "observed",
            channel.sourceFormat.map {
                Date(timeIntervalSince1970: TimeInterval($0.observedAt))
                    .formatted(date: .abbreviated, time: .shortened)
            }
        )

        let plan = status?.delivery ?? delivery
        if let plan {
            append(.delivery, "method", plan.playbackMethod)
            var video = plan.videoDescription
            if plan.videoAction == "encode",
               let encoder = status?.encoder,
               encoder != "pending" {
                video += " · \(encoder)"
            }
            if let owner = status?.ownerNodeId, !owner.isEmpty {
                video += " on \(owner)"
            }
            append(.delivery, "video", video)
            append(.delivery, "audio", plan.audioDescription)
            append(.delivery, "stream", "HLS · \(plan.packaging.uppercased())")
        }

        if let signal = status?.signal {
            if let strength = signal.strengthPercent {
                append(.signal, "strength", "\(strength)%", percent: strength)
            }
            if let quality = signal.qualityPercent {
                append(.signal, "quality", "\(quality)%", percent: quality)
            }
            if let symbol = signal.symbolQualityPercent {
                append(.signal, "symbol", "\(symbol)%", percent: symbol)
            }
        }

        let live = [
            player.behindEdgeSeconds.map {
                String(format: "%.1f s behind the edge", $0)
            },
            player.bufferedSeconds.map {
                String(format: "%.1f s buffered", $0)
            },
        ].compactMap { $0 }.joined(separator: " · ")
        append(.player, "behind the edge · buffered", live)

        if player.hasAccessEvents {
            var facts = [String]()
            if let rate = player.observedBitrate {
                facts.append(String(format: "%.1f Mb/s observed", rate / 1_000_000))
            }
            facts.append(player.droppedFrames.map {
                "\($0) dropped frames"
            } ?? "unknown dropped frames")
            facts.append(player.stalls.map {
                "\($0) stalls"
            } ?? "unknown stalls")
            append(.player, "rate", facts.joined(separator: " · "))
        }

        var session = [String]()
        if let owner = status?.ownerNodeId, !owner.isEmpty {
            session.append("owner \(owner)")
        }
        if let attached = player.attachedAt {
            let minutes = max(0, Int(player.asOf.timeIntervalSince(attached)) / 60)
            session.append("\(minutes) min")
        }
        append(.player, "session", session.joined(separator: " · "))
        return rows
    }

    private var rows: [LiveTvStreamInfoRow] {
        Self.rows(
            programme: programme,
            channel: channel,
            status: status,
            delivery: delivery,
            player: player
        )
    }

    private func infoColumn(
        _ sections: [LiveTvStreamInfoRow.Section]
    ) -> some View {
        VStack(alignment: .leading, spacing: 24) {
            ForEach(sections, id: \.self) { section in
                VStack(alignment: .leading, spacing: 11) {
                    Text(section.rawValue)
                        .font(.system(size: 17, weight: .bold, design: .monospaced))
                        .foregroundStyle(Palette.accent)
                        .tracking(1.7)
                    ForEach(rows.filter { $0.section == section }) { row in
                        infoRow(row)
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private func infoRow(_ row: LiveTvStreamInfoRow) -> some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline, spacing: 20) {
                Text(row.label)
                    .font(.system(size: 18, weight: .medium, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.36))
                    .frame(width: 150, alignment: .leading)
                Text(row.value)
                    .font(.system(size: 22))
                    .lineLimit(2)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            if let percent = row.percent {
                ProgressView(value: Double(percent), total: 100)
                    .tint(Palette.accent)
                    .frame(height: 6)
                    .padding(.leading, 170)
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 30) {
            HStack {
                Text("Stream info · as of \(player.asOf.formatted(date: .omitted, time: .shortened))")
                    .font(.system(size: 34, weight: .semibold))
                Spacer()
                Button(action: onClose) {
                    Label("Close", systemImage: "xmark")
                }
                .buttonStyle(LiveSurfacePillStyle())
                .focusEffectDisabled()
                .focused($closeFocused)
            }
            HStack(alignment: .top, spacing: 54) {
                infoColumn([.programme, .channel])
                infoColumn([.delivery, .signal, .player])
            }
        }
        .padding(.vertical, 40)
        .padding(.horizontal, 44)
        .frame(width: 1240, height: 984, alignment: .topLeading)
        .foregroundStyle(.white)
        .background(
            Palette.playerChrome.opacity(0.96),
            in: RoundedRectangle(cornerRadius: 22, style: .continuous)
        )
        .onAppear {
            Task { @MainActor in
                await Task.yield()
                closeFocused = true
            }
        }
    }
}
#endif

/// One channel row: chip, number and callsign, what is on with a bar to its
/// end, and when it ends. A protected channel is dimmed, never hidden.
///
/// Lists are tall and narrow. The row is a fixed three-column grid —
/// chip · text · trailing — with no `Spacer` pushing glyphs to a far edge,
/// because in a 620 pt column that edge is three feet away on a television.
struct LiveTvChannelRow: View {
    let channel: LiveTvChannel
    let airing: LiveTvAiring
    let selected: Bool

    #if os(tvOS)
    private let chipWidth: CGFloat = 84
    private let chipHeight: CGFloat = 40
    private let rowHeight: CGFloat = 72
    private let columnGap: CGFloat = 14
    private let barHeight: CGFloat = 3
    private let cornerRadius: CGFloat = 10
    private var stationName: String { channel.guideName }
    #else
    private let chipWidth: CGFloat = 52
    private let chipHeight: CGFloat = 32
    private let rowHeight: CGFloat = 78
    private let columnGap: CGFloat = 10
    private let barHeight: CGFloat = 3
    private let cornerRadius: CGFloat = 8
    private var stationName: String { String(channel.guideName.prefix(5)) }
    #endif

    private var endsAt: String? { airing.now.map { "until \(liveTvTime($0.end))" } }

    var body: some View {
        HStack(alignment: .center, spacing: columnGap) {
            Text(stationName)
                .font(LiveTvType.chip)
                .foregroundStyle(Palette.muted)
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .frame(width: chipWidth, height: chipHeight)
                .background(Palette.surfaceHi)
                .clipShape(RoundedRectangle(cornerRadius: 6))
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(channel.title).font(LiveTvType.primary).lineLimit(1)
                    if selected {
                        Text("● LIVE")
                            .font(LiveTvType.eyebrow)
                            .foregroundStyle(Palette.accent)
                    }
                    LiveTvFormatBadges(channel: channel)
                }
                if !channel.watchable {
                    Text("Protected · not playable")
                        .font(LiveTvType.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                } else if let now = airing.now {
                    Text(now.title).font(LiveTvType.secondary).lineLimit(1)
                    LiveTvProgressLine(value: airing.progress ?? 0, height: barHeight)
                    #if os(iOS)
                    Text([endsAt, airing.next.map { "Next: \($0.title)" }]
                        .compactMap { $0 }.joined(separator: " · "))
                        .font(LiveTvType.tertiary).foregroundStyle(Palette.muted).lineLimit(1)
                    #endif
                } else {
                    Text("No programme information")
                        .font(LiveTvType.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            VStack(alignment: .trailing, spacing: 2) {
                if channel.favorite {
                    Image(systemName: "star.fill").foregroundStyle(Palette.accent)
                }
                if !channel.watchable {
                    Image(systemName: "lock").foregroundStyle(Palette.muted)
                }
                #if os(tvOS)
                if let endsAt {
                    Text(endsAt).font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                }
                #endif
            }
            .fixedSize()
        }
        .padding(.horizontal, columnGap)
        .frame(minHeight: rowHeight)
        .background(selected ? Palette.surfaceHi : Color.clear,
                    in: RoundedRectangle(cornerRadius: cornerRadius))
        .overlay {
            if selected {
                RoundedRectangle(cornerRadius: cornerRadius).stroke(Palette.accent, lineWidth: 1)
            }
        }
        .opacity(channel.watchable ? 1 : 0.55)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

/// A flat progress line. `ProgressView` on tvOS draws a inset capsule with its
/// own generous padding, which is a bar in a row that has 72 pt to spend.
struct LiveTvProgressLine: View {
    let value: Double
    var height: CGFloat = 3

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(Palette.outline)
                Capsule().fill(Palette.accent)
                    .frame(width: proxy.size.width * min(max(value, 0), 1))
            }
        }
        .frame(height: height)
    }
}

#if os(tvOS)
/// A television list row owns its focus contrast. The platform's default
/// white plate hid the channel text and stretched across the full List while
/// the accent tint turned the page actions into blank red capsules.
private struct LiveTvChannelButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    fileprivate struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused

        var body: some View {
            // The row owns its own padding and its playing ring; the style
            // owns focus alone, so a 620 pt column stays a 620 pt column.
            configuration.label
                .foregroundStyle(Palette.onBg)
                .background(
                    isFocused ? Palette.surface : Color.clear,
                    in: RoundedRectangle(cornerRadius: 10, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .stroke(isFocused ? Palette.onBg : .clear,
                                lineWidth: isFocused ? 3 : 0)
                }
                .scaleEffect(isFocused ? 1.012 : (configuration.isPressed ? 0.99 : 1))
                .animation(.easeOut(duration: 0.12), value: isFocused)
        }
    }
}

/// Programme cells need an explicit focus ring. A plain tvOS button still
/// participates in focus, but gives the viewer no stable visual target; the
/// details pane changing somewhere else then makes a correct move look random.
private struct LiveTvGuideButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    fileprivate struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused

        var body: some View {
            configuration.label
                .foregroundStyle(Palette.onBg)
                .overlay {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .stroke(isFocused ? Palette.accent : .clear,
                                lineWidth: isFocused ? 4 : 0)
                }
                .scaleEffect(isFocused ? 1.018 : (configuration.isPressed ? 0.99 : 1))
                .shadow(color: Palette.accent.opacity(isFocused ? 0.28 : 0), radius: 10)
                .animation(.easeOut(duration: 0.1), value: isFocused)
        }
    }
}
#endif

#if os(tvOS)
/// The picture is a focus target now — Select on it is Fullscreen, which is
/// what let "Return to live" leave the toolbar. It needs a ring of its own:
/// a plain button around an AVPlayerLayer shows no focus at all.
private struct LiveTvPictureButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> Body {
        Body(configuration: configuration)
    }

    fileprivate struct Body: View {
        let configuration: ButtonStyle.Configuration
        @Environment(\.isFocused) private var isFocused

        var body: some View {
            configuration.label
                .overlay {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .stroke(isFocused ? Palette.accent : .clear, lineWidth: isFocused ? 4 : 0)
                }
                .animation(.easeOut(duration: 0.12), value: isFocused)
        }
    }
}
#endif

private struct LiveTvGuideScrollOriginKey: PreferenceKey {
    static var defaultValue: CGPoint = .zero
    static func reduce(value: inout CGPoint, nextValue: () -> CGPoint) {
        value = nextValue()
    }
}

/// Earlier / Now / Later. Three chips in the grid's own header rather than
/// three more full-height buttons in the page toolbar.
struct LiveTvGuidePaging {
    let canEarlier: Bool
    let canLater: Bool
    let earlier: () -> Void
    let now: () -> Void
    let later: () -> Void
}

/// The half-hour grid. One horizontal offset shared by every row, so the
/// channel column and the times cannot drift apart from the cells.
struct LiveTvGuideGrid: View {
    private struct FocusKey: Hashable {
        let channelId: String
        let programmeStart: Int?
        let channelHeader: Bool
        /// The header's paging chips. They live inside the grid, so they live
        /// inside the remote adapter, and `onMoveCommand` swallows every
        /// direction it receives — a focusable in here with no key of its own
        /// is a focus trap with no way out.
        var paging: Int? = nil
    }

    let layout: LiveTvGridLayout
    /// Drawn geometry. On a television the slot width is derived from the
    /// space the grid actually got, so two hours always fit the screen.
    let dimensions: LiveTvGridDimensions
    let slots: [Int]
    let playingChannelId: String?
    /// What the DVR knows about each airing, from the one read of the schedule
    /// and the reminders that each guide load makes. Empty while recording is
    /// off or before that read lands, which draws exactly the grid that
    /// existed before any of this — every mark is an overlay, so no row, slot
    /// or type size moves to make room for one.
    var marks = DvrMarks()
    /// Only the running-capture underline uses it; nothing in the geometry
    /// depends on the clock.
    var now = 0
    let onAiring: (LiveTvChannel) -> Void
    let onFuture: (LiveTvChannel, LiveTvProgramme) -> Void
    let restoreChannelId: String?
    let restoreProgrammeStart: Int?
    let restoreChannelHeader: Bool
    let restoreAnchorTime: Int?
    let onFocus: (LiveTvChannel, LiveTvProgramme?, Bool, Int?) -> Void
    let onToolbarBoundary: () -> Void
    let restoreRequest: Int
    let restoreAllowed: Bool
    let onFocusOwnershipChanged: (Bool) -> Void
    /// Earlier / Now / Later, drawn as chips in the header's channel column
    /// instead of three more buttons in the page toolbar.
    var paging: LiveTvGuidePaging? = nil
    @State private var scrollOrigin = CGPoint.zero
    @State private var anchorTime: Int?
    @State private var preserveAnchorForNextFocus = false
    @State private var focusCoordinator = LiveTvGuideFocusCoordinator()
    @FocusState private var focusedCell: FocusKey?
    private let headerHeight: CGFloat = 34

    var body: some View {
        ScrollView([.horizontal, .vertical]) {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 0) {
                    Color.clear.frame(width: dimensions.channelColumnWidth)
                    ForEach(slots, id: \.self) { at in
                        Color.clear
                            .frame(width: dimensions.slotWidth, alignment: .leading)
                    }
                }
                .frame(height: headerHeight)
                ForEach(layout.rows, id: \.channel.id) { row in
                    HStack(spacing: 0) {
                        Button { onAiring(row.channel) } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(row.channel.guideNumber).font(LiveTvType.secondary.weight(.semibold))
                                Text(row.channel.guideName).font(LiveTvType.badge)
                                    .foregroundStyle(Palette.muted).lineLimit(1)
                                LiveTvFormatBadges(channel: row.channel)
                            }
                            .padding(.horizontal, 6)
                            .frame(width: dimensions.channelColumnWidth,
                                   height: dimensions.rowHeight,
                                   alignment: .leading)
                        }
                        #if os(tvOS)
                        .buttonStyle(LiveTvGuideButtonStyle())
                        .focusEffectDisabled()
                        .focused(
                            $focusedCell,
                            equals: FocusKey(
                                channelId: row.channel.id,
                                programmeStart: nil,
                                channelHeader: true
                            )
                        )
                        #else
                        .buttonStyle(.plain)
                        #endif
                        .background(Palette.bg)
                        // The header is part of the vertical scroll content,
                        // so focus can reveal every row. Cancel only the
                        // horizontal content offset to keep the column pinned.
                        .offset(x: -scrollOrigin.x)
                        .zIndex(2)
                        ZStack(alignment: .topLeading) {
                            // An empty row is a channel with no guide data, not
                            // a channel that went away.
                            Color.clear.frame(width: layout.totalWidth,
                                              height: dimensions.rowHeight)
                            ForEach(row.cells, id: \.programme.id) { cell in
                                let mark = marks.mark(channelId: row.channel.id,
                                                      airingStart: cell.programme.start)
                                Button {
                                    // A cell's actions live in the programme
                                    // sheet, so every cell opens it — including
                                    // the one on air, which is otherwise the
                                    // only cell a viewer cannot record. The On
                                    // now list keeps the one-tap path to the
                                    // picture, and the sheet puts Watch first.
                                    //
                                    // This was `#if os(iOS)` and tvOS took the
                                    // other branch, so the television — the
                                    // surface the DVR acceptance starts from —
                                    // was the one place a viewer could not
                                    // record what they were watching.
                                    onFuture(row.channel, cell.programme)
                                } label: {
                                    Text(cell.programme.title)
                                        .font(LiveTvType.cell).lineLimit(1)
                                        .padding(.horizontal, 8)
                                        .frame(width: max(cell.width - 6, 24),
                                               height: dimensions.rowHeight - 10,
                                               alignment: .leading)
                                        .background(cell.airing ? Palette.surfaceHi : Palette.surface)
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 6).stroke(
                                                cell.airing && row.channel.id == playingChannelId
                                                    ? Palette.accent : Palette.outline,
                                                lineWidth: 1)
                                        )
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                        .dvrCellMark(mark, now: now)
                                }
                                #if os(tvOS)
                                .buttonStyle(LiveTvGuideButtonStyle())
                                .focusEffectDisabled()
                                .focused(
                                    $focusedCell,
                                    equals: FocusKey(
                                        channelId: row.channel.id,
                                        programmeStart: cell.programme.start,
                                        channelHeader: false
                                    )
                                )
                                #else
                                .buttonStyle(.plain)
                                #endif
                                .offset(x: cell.left + 3, y: 5)
                                .accessibilityLabel(
                                    "\(row.channel.title), \(cell.programme.title), "
                                    + "\(liveTvTime(cell.programme.start)) to \(liveTvTime(cell.programme.end))"
                                    + (mark.map { ", \($0.accessibilityDescription)" } ?? ""))
                            }
                            if row.cells.isEmpty {
                                Button { onAiring(row.channel) } label: {
                                    Text("No programme information · Watch live")
                                        .font(LiveTvType.cell).lineLimit(1)
                                        .padding(.horizontal, 8)
                                        .frame(width: max(layout.totalWidth - 6, 120),
                                               height: dimensions.rowHeight - 10,
                                               alignment: .leading)
                                        .background(Palette.surface)
                                        .overlay(RoundedRectangle(cornerRadius: 6).stroke(Palette.outline))
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                }
                                #if os(tvOS)
                                .buttonStyle(LiveTvGuideButtonStyle())
                                .focusEffectDisabled()
                                .focused(
                                    $focusedCell,
                                    equals: FocusKey(
                                        channelId: row.channel.id,
                                        programmeStart: nil,
                                        channelHeader: false
                                    )
                                )
                                #else
                                .buttonStyle(.plain)
                                #endif
                                .offset(x: 3, y: 5)
                            }
                        }
                    }
                    .frame(height: dimensions.rowHeight)
                }
            }
            .padding(.horizontal, 8)
            .background {
                GeometryReader { proxy in
                    Color.clear.preference(
                        key: LiveTvGuideScrollOriginKey.self,
                        value: proxy.frame(in: .named("live-tv-guide-scroll")).origin
                    )
                }
            }
            .overlay(alignment: .topLeading) {
                if let nowX = layout.nowX {
                    Rectangle().fill(.red).frame(width: 2)
                        .offset(x: dimensions.channelColumnWidth + nowX + 8)
                        .accessibilityHidden(true)
                }
            }
        }
        .coordinateSpace(name: "live-tv-guide-scroll")
        .onPreferenceChange(LiveTvGuideScrollOriginKey.self) { scrollOrigin = $0 }
        .overlay(alignment: .topLeading) {
            ZStack(alignment: .topLeading) {
                Palette.bg.frame(width: dimensions.channelColumnWidth + 8, height: headerHeight)
                HStack(spacing: 0) {
                    ForEach(slots, id: \.self) { at in
                        Text(liveTvTime(at))
                            .font(LiveTvType.tertiary).foregroundStyle(Palette.muted)
                            .padding(.leading, 6)
                            .frame(width: dimensions.slotWidth, height: headerHeight,
                                   alignment: .leading)
                            .overlay(alignment: .leading) {
                                Rectangle().fill(Palette.outline).frame(width: 1)
                            }
                    }
                }
                .offset(x: dimensions.channelColumnWidth + 8 + scrollOrigin.x)
                .frame(height: headerHeight)
                .clipped()
                if let paging {
                    HStack(spacing: 6) {
                        pagingChip("‹", index: 0, enabled: paging.canEarlier, action: paging.earlier)
                        pagingChip("Now", index: 1, enabled: true, action: paging.now)
                        pagingChip("›", index: 2, enabled: paging.canLater, action: paging.later)
                    }
                    .padding(.leading, 8)
                    .frame(width: dimensions.channelColumnWidth + 8, height: headerHeight,
                           alignment: .leading)
                }
            }
        }
        #if os(tvOS)
        .onChange(of: focusedCell) { _, target in
            // A focused paging chip is inside the grid but is not the grid
            // owning a cell: claiming ownership here would arm the restore
            // pass, which would then take the focus straight back off the chip.
            guard let target, target.paging == nil else {
                focusCoordinator.focusChanged(active: false)
                onFocusOwnershipChanged(false)
                return
            }
            focusCoordinator.focusChanged(active: true)
            onFocusOwnershipChanged(true)
            guard
                  let row = layout.rows.first(where: { $0.channel.id == target.channelId })
            else { return }
            let programme = target.programmeStart.flatMap { start in
                row.cells.first(where: { $0.programme.start == start })?.programme
            }
            if preserveAnchorForNextFocus {
                preserveAnchorForNextFocus = false
            } else if let start = target.programmeStart,
               let cell = row.cells.first(where: { $0.programme.start == start }) {
                anchorTime = cell.programme.start + max(1, cell.programme.end - cell.programme.start) / 2
            }
            onFocus(row.channel, programme, target.channelHeader, anchorTime)
        }
        .liveTvRemoteAdapter(
            .guide,
            state: { .temporaryGuide },
            apply: { outcome, input in
                guard outcome == .focusCell else { return false }
                return moveFocus(input)
            }
        )
        .onChange(of: restoreAllowed) { _, allowed in
            if !allowed { focusCoordinator.leave() }
        }
        // Content changes may reconcile the currently focused cell, but they
        // cannot create focus ownership. Only a new explicit request or a
        // grid that still owns focus receives a valid post-yield ticket.
        .task(id: gridRestoreIdentity) {
            guard let ticket = focusCoordinator.beginRestore(
                request: restoreRequest,
                ownerRequested: restoreAllowed
            ) else { return }
            await Task.yield()
            guard !Task.isCancelled,
                  focusCoordinator.permits(ticket, ownerRequested: restoreAllowed)
            else { return }
            restoreFocus()
        }
        #endif
    }

    @ViewBuilder private func pagingChip(
        _ label: String, index: Int, enabled: Bool, action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            Text(label)
                .font(LiveTvType.badge)
                .padding(.horizontal, 8)
                .frame(height: 24)
                .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 6))
        }
        #if os(tvOS)
        .buttonStyle(LiveTvGuideButtonStyle())
        .focusEffectDisabled()
        .focused($focusedCell, equals: FocusKey(
            channelId: "", programmeStart: nil, channelHeader: false, paging: index
        ))
        #else
        .buttonStyle(.plain)
        #endif
        .disabled(!enabled)
        .opacity(enabled ? 1 : 0.4)
    }

    #if os(tvOS)
    /// Returns whether the press was used. `onMoveCommand` consumes every
    /// direction it is given, so anything this declines is a dead press —
    /// which is why the chips are handled here rather than left to the engine.
    @discardableResult
    private func moveFocus(_ direction: LiveTvContractInput) -> Bool {
        guard let current = focusedCell else { return false }
        if let chip = current.paging { return movePagingFocus(chip, direction) }
        let position = LiveTvGuideFocusPosition(
            channelId: current.channelId,
            programmeStart: current.programmeStart,
            channelHeader: current.channelHeader,
            anchorTime: anchorTime
        )
        let effects = focusCoordinator.move(
            layout: layout,
            current: position,
            direction: direction,
            fallbackAnchor: slots.first ?? 0
        )
        for effect in effects {
            switch effect {
            case .focus(let next):
            anchorTime = next.anchorTime
            preserveAnchorForNextFocus = true
            focusedCell = FocusKey(
                channelId: next.channelId,
                programmeStart: next.programmeStart,
                channelHeader: next.channelHeader
            )
            case .clearGrid:
                focusedCell = nil
            case .focusToolbar:
                onToolbarBoundary()
            }
        }
        return true
    }

    /// Left and right walk the three chips; up hands the press to the toolbar
    /// and down enters the grid, exactly as a cell in the first row would.
    private func movePagingFocus(_ index: Int, _ direction: LiveTvContractInput) -> Bool {
        switch direction {
        case .left where index > 0:
            focusedCell = FocusKey(channelId: "", programmeStart: nil,
                                   channelHeader: false, paging: index - 1)
        case .right where index < 2:
            focusedCell = FocusKey(channelId: "", programmeStart: nil,
                                   channelHeader: false, paging: index + 1)
        case .up:
            onToolbarBoundary()
        case .down, .right:
            focusGridCandidate()
        case .left:
            // Already on the first chip: the channel column's left edge is the
            // page edge, so there is nowhere to go and nothing to hand back.
            break
        default:
            return false
        }
        return true
    }

    private var gridContentIdentity: String {
        let rows = layout.rows.map { row in
            "\(row.channel.id):\(row.cells.map { String($0.programme.start) }.joined(separator: ","))"
        }.joined(separator: "|")
        return "\(rows)#\(slots.map(String.init).joined(separator: ","))"
    }

    private var gridRestoreIdentity: String {
        "\(restoreRequest)#\(restoreAllowed)#\(gridContentIdentity)"
    }

    private func restoreFocus() {
        guard restoreAllowed else { return }
        focusGridCandidate()
    }

    private func focusGridCandidate() {
        guard let row = restoreChannelId.flatMap({ id in
            layout.rows.first(where: { $0.channel.id == id })
        }) ?? layout.rows.first else { return }
        anchorTime = restoreAnchorTime
        if restoreChannelHeader {
            focusedCell = FocusKey(channelId: row.channel.id, programmeStart: nil, channelHeader: true)
            return
        }
        let candidate = restoreProgrammeStart.flatMap { start in
            row.cells.first(where: { $0.programme.start == start })
        } ?? restoreAnchorTime.flatMap { anchor in
            row.cells.first(where: { $0.programme.start <= anchor && anchor < $0.programme.end })
                ?? row.cells.min {
                    abs(($0.programme.start + $0.programme.end) / 2 - anchor)
                        < abs(($1.programme.start + $1.programme.end) / 2 - anchor)
                }
        } ?? row.cells.first
        focusedCell = FocusKey(
            channelId: row.channel.id,
            programmeStart: candidate?.programme.start,
            channelHeader: false
        )
    }
    #endif
}

struct LiveTvView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    let onLeave: () -> Void
    @ObservedObject private var live = LiveTvPlayerController.shared
    /// One authenticated profile cache shared with root Recordings. The app
    /// shell owns its foreground polling lifecycle.
    @ObservedObject private var dvr = DvrController.shared
    @StateObject private var pictureInPicture = PictureInPictureController()
    @State private var query = ""
    @State private var fullscreen = false
    @State private var muted = false
    @State private var browse = LiveTvBrowseView.persisted()
    @AppStorage("plurx.liveTvLayout") private var tvLayoutRaw = TvLiveLayout.guidePreview.rawValue
    @State private var favoritesOnly = false
    @State private var hideProtected = false
    @State private var showingSearch = false
    @State private var detail: LiveTvProgramme?
    @State private var detailChannel: LiveTvChannel?
    @State private var overlayVisible = true
    @State private var temporaryGuide = false
    @State private var showingTouchGuide = false
    @State private var showingTouchRecordings = false
    @State private var showingInfo = false
    @State private var showingDvrActivity = false
    @State private var streamInfoPlayer: LiveTvPlayerFacts?
    @State private var showingMore = false
    @State private var showingLayout = false
    @State private var mobileGuideGrid = SettingsStore().liveTvMobileGuideUsesGrid
    @State private var scheduleChannelId: String?
    @State private var guideWindowStart = {
        let current = Int(Date().timeIntervalSince1970)
        return current - current % LiveTvGridMetrics.slotSeconds
    }()
    @State private var focusedGuideChannelId: String?
    @State private var focusedGuideProgrammeStart: Int?
    @State private var focusedGuideChannelHeader = false
    @State private var guideAnchorTime: Int?
    @State private var tvFocusedChannelId: String?
    @State private var overlayGeneration = 0
    @State private var onScreen = false
    /// A channel a local reminder's *Watch* action named while the app was
    /// elsewhere. Held until the lineup has loaded, because tuning a channel
    /// the page has never heard of does nothing at all.
    @State private var pendingReminderChannelId: String?
    #if os(iOS)
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    #if os(tvOS)
    /// The one focusable thing on the ten-foot surface while the overlay is
    /// hidden. Its only job is to exist, so the remote has somewhere to send
    /// a press that the routing table can then decide.
    private enum FocusTarget: Hashable {
        case reveal, guide, channels, recordings, play, info, layout, more
    }
    @FocusState private var focusedControl: FocusTarget?
    @FocusState private var focusedChannelId: String?
    @State private var guideFocusRequest = 0
    @State private var guideFocusRequested = false
    @State private var channelFocusRequest = 0
    @State private var channelFocusRequested = false
    @State private var channelFocusCoordinator = LiveTvFocusRestoreCoordinator()
    #endif
    @State private var now = Int(Date().timeIntervalSince1970)

    private let tick = Timer.publish(every: 30, on: .main, in: .common).autoconnect()

    init(onLeave: @escaping () -> Void = {}) {
        self.onLeave = onLeave
    }

    private var visibleChannels: [LiveTvChannel] {
        LiveTvGuideReducer.filter(
            channels: live.channels,
            guide: live.guide,
            options: LiveTvChannelFilter(query: query, favoritesOnly: favoritesOnly,
                                         hideProtected: hideProtected),
            now: now)
    }

    private var tvLayout: TvLiveLayout {
        get { TvLiveLayout(rawValue: tvLayoutRaw) ?? .guidePreview }
        nonmutating set { tvLayoutRaw = newValue.rawValue }
    }

    static func liveSurfaceChips(
        status: LiveTvStatus?,
        delivery: LiveTvDelivery?,
        behindEdge: Double?,
        paused: Date?,
        now: Date = Date()
    ) -> [LiveSurfaceChip] {
        var chips = [LiveSurfaceChip(kind: .live, text: "LIVE")]
        if let behindEdge {
            let behind = if let paused {
                "paused · \(Self.livePauseDuration(from: paused, to: now))"
            } else {
                String(format: "%.1f s behind the edge", max(0, behindEdge))
            }
            chips.append(LiveSurfaceChip(kind: .behind, text: behind))
        }
        if let signal = status?.signal {
            var facts = [String]()
            if let strength = signal.strengthPercent {
                let bars = String(repeating: "▮", count: min(5, max(0, strength / 20)))
                facts.append([bars, "\(strength)% signal"].filter { !$0.isEmpty }.joined(separator: " "))
            }
            if let quality = signal.qualityPercent { facts.append("\(quality)% quality") }
            if !facts.isEmpty {
                chips.append(LiveSurfaceChip(kind: .signal, text: facts.joined(separator: " · ")))
            }
        }
        if let delivery {
            let output = delivery.output
            let method = delivery.videoAction == "copy"
                ? "direct \(output.videoCodec)"
                : "transcode \(output.height)p \(output.videoCodec)"
            chips.append(LiveSurfaceChip(kind: .method, text: method))
            let channels = switch output.audioChannels {
            case 1: "mono"
            case 2: "stereo"
            case 6: "5.1"
            default: "\(output.audioChannels) ch"
            }
            let action = delivery.audioAction == "copy" ? "copied" : "transcoded"
            chips.append(LiveSurfaceChip(
                kind: .audio,
                text: "\(output.audioCodec) \(channels) \(action)"
            ))
        }
        chips.append(LiveSurfaceChip(
            kind: .clock,
            text: now.formatted(date: .omitted, time: .shortened)
        ))
        return chips
    }

    private static func livePauseDuration(from start: Date, to end: Date) -> String {
        let seconds = max(0, Int(end.timeIntervalSince(start)))
        return String(format: "%d:%02d", seconds / 60, seconds % 60)
    }

    static func liveProgressText(airing: LiveTvAiring, now: Int) -> [String] {
        guard let programme = airing.now else { return [] }
        var values = [
            liveTvTime(programme.start),
            liveTvTime(programme.end),
            "\(max(0, (programme.end - now) / 60)) min left",
        ]
        if let next = airing.next {
            values.append("Next \(liveTvTime(next.start)) · \(next.title)")
        }
        return values
    }

    /// Leaving the tab or backgrounding the app releases the tuner — unless
    /// picture-in-picture is running or about to be, which is the one case
    /// where the video is still on screen and its tuner is still in use.
    ///
    /// `isActive` alone is not enough on either side of that question.
    /// Automatic PiP starts on *background*, and iOS publishes `.inactive`
    /// first, so at the `.inactive` edge the flag is still false and the old
    /// test released the tuner before PiP could ever start — killing the exact
    /// case the milestone exists for. `isStarting` covers that gap.
    private var mayRelease: Bool { !pictureInPicture.isActive && !pictureInPicture.isStarting }

    private func showStreamInfo() {
        streamInfoPlayer = LiveTvPlayerFacts.capture(
            item: live.player.currentItem,
            behindEdgeSeconds: live.behindEdgeSeconds,
            bufferedSeconds: live.bufferedSeconds,
            attachedAt: live.attachedAt
        )
        showingInfo = true
    }

    var body: some View {
        GeometryReader { geometry in
          VStack(spacing: 0) {
            #if os(tvOS)
            liveToolbar
            tvBrowseRegion(in: geometry)
            #else
            if horizontalSizeClass == .regular {
                liveToolbar
                let wide = geometry.size.width >= 850
                let layout = wide
                    ? AnyLayout(HStackLayout(alignment: .top, spacing: 20))
                    : AnyLayout(VStackLayout(spacing: 16))
                layout {
                    tabletWatchPanel
                        .frame(width: wide ? geometry.size.width * 0.57 : nil,
                               height: wide ? nil : geometry.size.height * 0.48)
                    touchChannelList
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
                .padding(.horizontal, 16)
                .searchable(text: $query, isPresented: $showingSearch,
                            prompt: "Number, name, or what is on")
            } else {
                // A fullscreen cover owns the only attached player surface.
                if live.playing && !fullscreen {
                    phonePicture(in: geometry)
                    phoneCaption
                }
                liveToolbar
                browseRegion
            }
            #endif
        }
        .padding(.horizontal, liveContentInset)
        .padding(.bottom, liveContentInset)
        // The status banner used to own a whole band above the content: a
        // three-line centred card, roughly 90 pt of the page. What is durable
        // about it — the channel count — is in the toolbar now, and the rest
        // is one muted line along the bottom of the content it describes.
        .safeAreaInset(edge: .bottom, spacing: 0) { statusLine }
        .overlay(alignment: .bottomLeading) { reminderOverlay }
        }
        .navigationTitle("Live TV")
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { phoneNavigationActions }
        #endif
        .background(Palette.bg)

        .task { await live.load(origin: model.origin, token: Session.shared.token) }
        .task { await dvr.load(origin: model.origin, token: Session.shared.token) }
        // One read of the schedule and the reminders per guide load, and none
        // in between. A plan changes when somebody changes it — every mutation
        // re-reads for itself — so a page that polled would spend a
        // household's evening asking a question it already had the answer to.
        .onChange(of: live.guide?.fetchedAt) { _, _ in
            Task { await dvr.refresh() }
        }
        .onChange(of: browse) { _, _ in dvr.clearMessage() }
        .onChange(of: detail) { _, _ in dvr.clearMessage() }
        .onReceive(tick) { _ in
            now = Int(Date().timeIntervalSince1970)
            live.expireSourceFormats(now: now)
            Task { await dvr.refreshDue() }
        }
        .onDisappear {
            onScreen = false
            if !fullscreen && mayRelease { Task { await live.stop() } }
        }
        // Dismissing the cover is `exit` — leave the presentation. It is not
        // `stop`. Releasing here meant "Exit" cost a full tuner re-acquisition
        // to get back to the channel you were on two seconds earlier, and made
        // the tvOS `back → exit` row destructive.
        .fullScreenCover(isPresented: $fullscreen) {
            fullscreenSurface
                .sheet(item: $detail) { programme in programmeDetail(programme) }
                .sheet(isPresented: $showingDvrActivity) {
                    NavigationStack { DvrCaptureActivityView() }
                }
        }
        #if os(iOS)
        .sheet(isPresented: $showingTouchRecordings) {
            NavigationStack {
                DvrRecordingsPanel(dvr: dvr, now: now)
                    .navigationTitle("Recordings")
                    .toolbar { Button("Close") { showingTouchRecordings = false } }
            }
        }
        .sheet(isPresented: $showingTouchGuide) {
            touchGuidePanel { showingTouchGuide = false }
                .sheet(item: $detail) { programme in programmeDetail(programme) }
        }
        #endif
        .sheet(item: rootProgrammeDetail) { programme in
            programmeDetail(programme)
        }
        .sheet(isPresented: rootDvrActivity) {
            NavigationStack { DvrCaptureActivityView() }
        }
        .sheet(isPresented: $showingInfo) {
            #if os(tvOS)
            if let channel = live.watching, let player = streamInfoPlayer {
                LiveTvStreamInfoPanel(
                    programme: live.airing(channel, now: Int(player.asOf.timeIntervalSince1970)),
                    channel: channel,
                    status: live.status,
                    delivery: live.delivery,
                    player: player,
                    onClose: { showingInfo = false }
                )
            }
            #else
            if let channel = live.watching {
                LiveTvTechnicalDetails(channel: channel, status: live.status, delivery: live.delivery)
                    .padding(48)
                    .frame(minWidth: 420, minHeight: 260, alignment: .topLeading)
                    .background(Palette.bg)
            }
            #endif
        }
        .sheet(isPresented: $showingLayout) { layoutPanel }
        .sheet(isPresented: $showingMore) { morePanel }
        #if os(tvOS)
        .sheet(isPresented: $showingSearch) {
            VStack(alignment: .leading, spacing: 28) {
                Text("Find a channel").font(LiveTvType.title)
                TextField("Number, name, or what is on", text: $query)
                Button("Done") { showingSearch = false }
                    .buttonStyle(TVReadableButtonStyle(prominent: true))
                    .focusEffectDisabled()
            }
            .padding(70)
            .background(Palette.bg)
        }
        #endif
        .onChange(of: scenePhase) { _, phase in
            // Narrowed, not removed: entering PiP backgrounds the app, and the
            // old rule would have killed the exact case PiP exists for. Only
            // a full background decides — `.inactive` is a notification banner
            // or an app-switcher peek, and it is also the edge automatic PiP
            // has not yet crossed.
            if phase == .background && mayRelease { Task { await live.stop() } }
        }
        // The other half of the PiP rule, and the one that was missing: when
        // PiP ends and this screen is no longer on show, nothing else will ever
        // release the tuner. Without this the lease was renewed forever by the
        // heartbeat and the household's only tuner was held by an app
        // displaying nothing.
        .onChange(of: pictureInPicture.isActive) { _, active in
            if !active && !onScreen && live.playing { Task { await live.stop() } }
        }
        #if os(tvOS)
        .onChange(of: focusedControl) { _, target in
            if target != nil { cancelBrowseFocusRestoration() }
        }
        #endif
        #if os(iOS)
        // A reminder's *Watch* action arrives while this page may not yet have
        // a lineup, so the channel is held rather than tuned straight away.
        .onReceive(NotificationCenter.default.publisher(for: .plurxReminderWatch)) { _ in
            claimReminderChannel()
        }
        .onChange(of: live.channels.map(\.id)) { _, _ in tuneReminderChannel() }
        .onChange(of: pendingReminderChannelId) { _, _ in tuneReminderChannel() }
        .onChange(of: horizontalSizeClass) { _, _ in normalizeTabletBrowse() }
        .onChange(of: browse) { _, _ in normalizeTabletBrowse() }
        #endif
        .onChange(of: fullscreen) { _, presented in
            if !presented { temporaryGuide = false }
        }
        .onAppear {
            onScreen = true
            #if os(iOS)
            normalizeTabletBrowse()
            // The action may have arrived before this page existed to hear it.
            claimReminderChannel()
            #endif
        }
    }

    #if os(iOS)
    private func claimReminderChannel() {
        guard let channelId = ReminderWatchRequest.take() else { return }
        pendingReminderChannelId = channelId
    }

    private func tuneReminderChannel() {
        guard let wanted = pendingReminderChannelId,
              let channel = live.channels.first(where: { $0.id == wanted })
        else { return }
        pendingReminderChannelId = nil
        selectAiring(channel)
    }
    #endif

    private var rootProgrammeDetail: Binding<LiveTvProgramme?> {
        Binding(
            get: { fullscreen || showingTouchGuide ? nil : detail },
            set: { detail = $0 }
        )
    }

    private var rootDvrActivity: Binding<Bool> {
        Binding(get: { !fullscreen && showingDvrActivity }, set: { showingDvrActivity = $0 })
    }

    private var liveInputState: LiveTvInputState {
        if temporaryGuide { return .temporaryGuide }
        if showingMore || showingLayout { return .menu }
        if showingInfo { return .streamInfo }
        if detail != nil { return .programmeDetails }
        return overlayVisible ? .fullscreenControls : .fullscreenHidden
    }

    /// The one message that is a standing fact rather than something to read:
    /// while a channel is playing normally there is nothing to say. Everything
    /// else — including every cleanup and every failure — stays on screen for
    /// as long as it is true, because a four-second toast cannot repeat itself
    /// when the same failure happens twice.
    static func isSteadyStateMessage(_ message: String) -> Bool {
        message == "Playing live" || message == "Playing live · no recording or rewind"
    }

    /// Both lines when both are true. A recording refusal and an unconfirmed
    /// tuner cleanup are different facts about different things, and hiding
    /// either behind the other is how a viewer ends up acting on neither.
    private var statusText: String? {
        let tuner = live.message
        let tunerText = tuner.isEmpty || LiveTvView.isSteadyStateMessage(tuner) ? nil : tuner
        let parts = [dvr.message, tunerText].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// The phone's picture is full-bleed, so the page cannot own a horizontal
    /// inset; every other band pays for its own. A television keeps the
    /// platform padding it always had.
    private var liveContentInset: CGFloat {
        #if os(tvOS)
        return 28
        #else
        return 0
        #endif
    }

    /// The due reminder, lower-left, on the page and on the fullscreen cover
    /// alike. Only the buttons are interactive: it claims no key, so the
    /// remote means exactly what the routing table says it means whether or
    /// not a reminder happens to be showing.
    @ViewBuilder private var reminderOverlay: some View {
        if let reminder = dvr.current {
            ReminderOverlay(
                reminder: reminder,
                now: now,
                watch: {
                    Task { await dvr.ack(reminder) }
                    guard let channel = live.channels.first(where: { $0.id == reminder.channelId })
                    else { return }
                    selectAiring(channel)
                },
                record: {
                    Task {
                        await dvr.record(channelId: reminder.channelId,
                                         airingStart: reminder.airingStart)
                        await dvr.ack(reminder)
                    }
                },
                dismiss: { Task { await dvr.ack(reminder) } },
                expire: { dvr.silence(reminder) }
            )
            .padding(16)
        }
    }

    @ViewBuilder private var statusLine: some View {
        if let statusText {
            Text(statusText)
                .font(LiveTvType.tertiary)
                .foregroundStyle(Palette.muted)
                .lineLimit(2)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 12).padding(.vertical, 6)
                .background(Palette.surface.opacity(0.94))
                .accessibilityIdentifier("live-tv-status")
        }
    }

    private var toolbarSummary: String {
        let shown = visibleChannels.count
        let total = live.channels.count
        var parts = [shown == total ? "\(total) channels" : "\(shown) of \(total) channels"]
        if live.playing { parts.append("1 tuner in use") }
        if let recordings = dvr.indicatorLabel { parts.append(recordings) }
        if let guide = live.guide, guide.freshness != "fresh" {
            parts.append(guide.freshness == "stale" ? "guide is stale" : "no guide data")
        }
        return parts.joined(separator: " · ")
    }

    private var layoutPanel: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Live TV layout").font(LiveTvType.title)
            // Two entries, three stored values: `channel_browser` still
            // decodes and still round-trips, it just renders as Preview.
            ForEach(TvLiveLayout.offered) { layout in
                Button {
                    tvLayout = layout
                    showingLayout = false
                } label: {
                    HStack {
                        Text(layout.label)
                        Spacer()
                        if layout == tvLayout.presented { Image(systemName: "checkmark") }
                    }
                }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: layout == tvLayout.presented))
                .focusEffectDisabled()
                #endif
            }
            Button("Close") { showingLayout = false }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .focusEffectDisabled()
                #endif
        }
        .padding(48)
        .frame(minWidth: 420, minHeight: 300, alignment: .topLeading)
        .background(Palette.bg)
    }

    private var morePanel: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Live TV").font(LiveTvType.title)
            Button("Layout") {
                showingMore = false
                showingLayout = true
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            .focusEffectDisabled()
            #endif
            Button("Refresh channels") {
                showingMore = false
                Task { await live.load(origin: model.origin, token: Session.shared.token) }
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            .focusEffectDisabled()
            #endif
            Button(hideProtected ? "Show protected" : "Hide protected") {
                hideProtected.toggle()
                showingMore = false
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: hideProtected))
            .focusEffectDisabled()
            #endif
            if live.playing {
                Button(muted ? "Unmute" : "Mute") {
                    muted.toggle()
                    live.player.isMuted = muted
                    showingMore = false
                }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: muted))
                .focusEffectDisabled()
                #endif
                Button("Stop") { showingMore = false; Task { await live.stop() } }
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: false))
                    .focusEffectDisabled()
                    #endif
                Button("Return to browser") { showingMore = false; fullscreen = false }
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: false))
                    .focusEffectDisabled()
                    #endif
            }
            Button("Leave Live TV") {
                showingMore = false
                Task { await live.stop(); onLeave() }
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            .focusEffectDisabled()
            #endif
            Button("Close") { showingMore = false }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: true))
                .focusEffectDisabled()
                #endif
        }
        .padding(48)
        .frame(minWidth: 420, minHeight: 360, alignment: .topLeading)
        .background(Palette.bg)
    }

    /// Television keeps its remote toolbar; touch gives the view selector and
    /// status separate rows so neither compresses the other on narrow screens.
    private var liveToolbar: some View {
        #if os(tvOS)
        HStack(spacing: 12) {
            HStack(spacing: 0) {
                segment("On now", active: browse == .list) { requestChannelFocus() }
                    .focused($focusedControl, equals: .channels)
                segment("Guide", active: browse == .guide) { requestGuideFocus() }
                    .focused($focusedControl, equals: .guide)
                segment(recordingsLabel, active: browse == .recordings) { showRecordings() }
                    .focused($focusedControl, equals: .recordings)
            }
            .overlay(RoundedRectangle(cornerRadius: 10).stroke(Palette.outline, lineWidth: 1))
            Text(toolbarSummary).font(LiveTvType.secondary).foregroundStyle(Palette.muted)
            Spacer(minLength: 8)
            Button { favoritesOnly.toggle(); requestChannelFocus() } label: {
                Label("Favorites", systemImage: favoritesOnly ? "star.fill" : "star")
            }
            .buttonStyle(TVReadableButtonStyle(prominent: favoritesOnly, compact: true))
            .focusEffectDisabled()
            Button { showingSearch = true } label: {
                Label(query.isEmpty ? "Search" : "Search: \(query)", systemImage: "magnifyingglass")
            }
            .buttonStyle(TVReadableButtonStyle(prominent: !query.isEmpty, compact: true))
            .focusEffectDisabled()
            Button { showingLayout = true } label: {
                Label("Layout", systemImage: "rectangle.3.group")
            }
            .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
            .focusEffectDisabled()
            .focused($focusedControl, equals: .layout)
            Button { showingMore = true } label: {
                Label("More", systemImage: "ellipsis")
            }
            .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
            .focusEffectDisabled()
            .focused($focusedControl, equals: .more)
        }
        .frame(height: 48)
        .padding(.horizontal, 12)
        #else
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                Menu {
                    Button("On now") { setTouchBrowse(.list, favorites: false) }
                    Button("Guide") { setTouchBrowse(.guide, favorites: false) }
                    Button("Favorites") { setTouchBrowse(.list, favorites: true) }
                    Button("Recordings") { showRecordings() }
                } label: {
                    Label(touchBrowseTitle, systemImage: "line.3.horizontal.decrease")
                        .font(.subheadline.weight(.semibold))
                        .padding(.horizontal, 12).padding(.vertical, 10)
                        .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 10))
                }
                .accessibilityLabel("Live TV view, \(touchBrowseTitle)")
                Spacer(minLength: 8)
                Button { showingDvrActivity = true } label: {
                    Label("Recording activity", systemImage: "record.circle")
                        .font(.subheadline)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Text(toolbarSummary)
                .font(.footnote).foregroundStyle(Palette.muted)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 12).padding(.vertical, 8)
        #endif
    }

    #if os(iOS)
    private func normalizeTabletBrowse() {
        guard horizontalSizeClass == .regular else { return }
        switch browse {
        case .guide:
            browse = .list
            showingTouchGuide = true
        case .recordings:
            browse = .list
            showingTouchRecordings = true
        case .list: break
        }
    }

    private var touchBrowseTitle: String {
        switch browse {
        case .recordings: return "Recordings"
        case .guide: return "Guide"
        case .list: return favoritesOnly ? "Favorites" : "On now"
        }
    }

    private func setTouchBrowse(_ view: LiveTvBrowseView, favorites: Bool) {
        if view == .guide && horizontalSizeClass == .regular {
            showingTouchGuide = true
            return
        }
        favoritesOnly = favorites
        browse = view
        browse.persist()
    }
    #endif

    /// The conflict count rides on the chip. A viewer whose Thursday has two
    /// programmes and one tuner should learn that from the toolbar, not from
    /// an empty file the next morning.
    private var recordingsLabel: String {
        if let label = dvr.indicatorLabel { return label }
        return dvr.conflicts > 0 ? "Recordings · \(dvr.conflicts)" : "Recordings"
    }

    /// Exact-airing state wins. A different airing on the watched channel is
    /// only described as channel context (head/tail padding); it never lends
    /// its red badge or title to the programme currently on screen.
    private func recordingContext(channel: LiveTvChannel,
                                  programme: LiveTvProgramme?) -> String? {
        if let programme,
           let active = dvr.activeRecording(channelId: channel.id, airingStart: programme.start) {
            return active.displayState
        }
        if let programme,
           let planned = dvr.recording(channelId: channel.id, airingStart: programme.start) {
            return planned.stopping ? "Stopping" : planned.state.label
        }
        guard let active = dvr.overview?.active.first(where: {
            $0.channelId == channel.id && $0.captureStart <= now && now < $0.captureEnd
        }) else { return nil }
        if now < active.airingStart {
            return "Recording early padding for \(active.title)"
        }
        if now >= active.airingEnd {
            return "Recording \(active.title) · end padding · until \(liveTvTime(active.captureEnd))"
        }
        return nil
    }

    private func showRecordings() {
        #if os(iOS)
        if horizontalSizeClass == .regular {
            showingTouchRecordings = true
            return
        }
        #endif
        browse = .recordings
        browse.persist()
        #if os(tvOS)
        // The grid and the list both restore focus when they believe they own
        // it. Leaving either armed would take the focus straight back off the
        // page that just opened.
        guideFocusRequested = false
        channelFocusRequested = false
        channelFocusCoordinator.leave()
        focusedChannelId = nil
        #endif
    }

    private func segment(_ label: String, active: Bool, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Text(label)
                #if os(tvOS)
                .font(LiveTvType.primary)
                .padding(.horizontal, 20)
                .frame(height: 48)
                #else
                .font(.system(size: 13, weight: .semibold))
                .padding(.horizontal, 12)
                .frame(height: 34)
                #endif
                .background(active ? Palette.surfaceHi : Color.clear)
        }
        #if os(tvOS)
        .buttonStyle(LiveTvGuideButtonStyle())
        .focusEffectDisabled()
        #else
        .buttonStyle(.plain)
        .foregroundStyle(active ? Palette.onBg : Palette.muted)
        #endif
    }

    #if os(iOS)
    /// Full-bleed 16:9, with the chips and the two actions on the picture
    /// rather than in a six-line bar underneath it.
    @ViewBuilder private func phonePicture(in geometry: GeometryProxy) -> some View {
        let airing = live.watching.map { live.airing($0, now: now) } ?? .none
        PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                      pgsOverlay: nil, allowsPictureInPicture: true)
            .frame(maxWidth: .infinity)
            .frame(height: min(geometry.size.width * 9 / 16, geometry.size.height * 0.4))
            .overlay(alignment: .topLeading) {
                VStack(alignment: .leading, spacing: 5) {
                    Text("● LIVE · \(live.watching?.guideNumber ?? "") \(live.watching?.guideName ?? "")")
                    if let channel = live.watching,
                       let context = recordingContext(channel: channel, programme: airing.now) {
                        Label(context, systemImage: "record.circle.fill")
                    }
                }
                .font(.system(size: 11, weight: .bold))
                .foregroundStyle(.white)
                .padding(.horizontal, 8).padding(.vertical, 4)
                .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 7))
                .padding(10)
            }
            .overlay(alignment: .topTrailing) {
                HStack(spacing: 14) {
                    if pictureInPicture.isSupported {
                        Button { pictureInPicture.toggle() } label: {
                            Image(systemName: "pip").frame(width: 30, height: 30)
                        }
                    }
                    Button { fullscreen = true } label: {
                        Image(systemName: "arrow.up.left.and.arrow.down.right")
                            .frame(width: 30, height: 30)
                    }
                }
                .buttonStyle(.plain)
                .foregroundStyle(.white)
                .padding(10)
            }
            .overlay(alignment: .bottom) {
                LiveTvProgressLine(value: airing.progress ?? 0, height: 3)
            }
    }

    /// One 56 pt caption line where the six-line `nowBar` used to be.
    private var phoneCaption: some View {
        let channel = live.watching
        let airing = channel.map { live.airing($0, now: now) } ?? .none
        let detail = [
            airing.now.map { "\(liveTvTime($0.start))–\(liveTvTime($0.end))" },
            airing.now.flatMap { $0.end > now ? "\(max(0, ($0.end - now) / 60)) min left" : nil },
            channel.flatMap { recordingContext(channel: $0, programme: airing.now) },
            airing.next.map { "Next: \($0.title)" }
        ].compactMap { $0 }.joined(separator: " · ")
        return HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text(airing.now?.title ?? live.title ?? "Live television")
                    .font(.system(size: 15, weight: .semibold)).lineLimit(1)
                Text(detail).font(.system(size: 12)).foregroundStyle(Palette.muted).lineLimit(1)
            }
            Spacer(minLength: 4)
            Button { muted.toggle(); live.player.isMuted = muted } label: {
                Image(systemName: muted ? "speaker.slash" : "speaker.wave.2")
                    .frame(width: 32, height: 32)
            }
            Button { showingInfo = true } label: {
                Image(systemName: "info.circle").frame(width: 32, height: 32)
            }
            Button { Task { await live.stop() } } label: {
                Image(systemName: "stop.circle").frame(width: 32, height: 32)
            }
        }
        .buttonStyle(.plain)
        .frame(height: 56)
        .padding(.horizontal, 12)
    }

    @ToolbarContentBuilder private var phoneNavigationActions: some ToolbarContent {
        ToolbarItem(placement: .topBarTrailing) {
            Button { showingSearch = true } label: { Image(systemName: "magnifyingglass") }
        }
        ToolbarItem(placement: .topBarTrailing) {
            Menu {
                Button("Refresh channels") {
                    Task { await live.load(origin: model.origin, token: Session.shared.token) }
                }
                Button(hideProtected ? "Show protected" : "Hide protected") { hideProtected.toggle() }
                if live.message.contains("Cleanup is unconfirmed") {
                    Button("Retry cleanup") { Task { await live.stop() } }
                }
                Button("Leave Live TV") { Task { await live.stop(); onLeave() } }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
        }
    }
    #endif

    #if os(iOS)
    /// The touch browse region. Television routes to `tvBrowseRegion`, so this
    /// is compiled for iOS alone rather than kept alive with tvOS branches
    /// nothing reaches.
    @ViewBuilder private var browseRegion: some View {
        browseContent
            // On the whole region, not on the On now list: the nav-bar icon
            // sets `showingSearch` from either tab, and a `.searchable` that
            // exists in only one of them leaves the flag stuck true and the
            // field silently absent.
            .searchable(text: $query, isPresented: $showingSearch,
                        prompt: "Number, name, or what is on")
    }

    @ViewBuilder private var browseContent: some View {
        switch browse {
        case .recordings:
            DvrRecordingsPanel(dvr: dvr, now: now)
                .padding(.horizontal, 12)
        case .list:
            touchChannelList
        case .guide:
            if verticalSizeClass == .regular && horizontalSizeClass == .compact && !mobileGuideGrid {
                mobileSchedule
            } else {
                VStack(spacing: 8) {
                    if verticalSizeClass == .regular && horizontalSizeClass == .compact {
                        Button("Selected channel schedule") {
                            mobileGuideGrid = false
                            SettingsStore().liveTvMobileGuideUsesGrid = false
                        }
                        .font(.system(size: 13, weight: .semibold))
                    }
                    guideGrid
                }
            }
        }
    }

    private var touchChannelList: some View {
        List(visibleChannels) { channel in
                Button {
                    selectAiring(channel)
                } label: {
                    LiveTvChannelRow(channel: channel, airing: live.airing(channel, now: now),
                                     selected: live.watching?.id == channel.id)
                }
                .disabled(!channel.watchable || live.busy)
                .buttonStyle(.plain)
                .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
                .listRowBackground(Color.clear)
            }
            .listStyle(.plain)
    }

    private var tabletWatchPanel: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if live.playing && !fullscreen {
                    PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                                  pgsOverlay: nil, allowsPictureInPicture: true)
                        .aspectRatio(16 / 9, contentMode: .fit)
                        .clipShape(RoundedRectangle(cornerRadius: 14))
                } else if !live.playing {
                    ContentUnavailableView("Choose a channel", systemImage: "tv",
                        description: Text("Watch live television and browse what is on alongside the picture."))
                        .frame(maxWidth: .infinity)
                        .aspectRatio(16 / 9, contentMode: .fit)
                        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14))
                }
                if let channel = live.watching {
                    let airing = live.airing(channel, now: now)
                    Text("LIVE · \(channel.title)").font(.subheadline.weight(.semibold))
                        .foregroundStyle(Palette.accent)
                    Text(airing.now?.title ?? live.title ?? "Live television")
                        .font(.title2.bold()).fixedSize(horizontal: false, vertical: true)
                    if let programme = airing.now {
                        Text("\(liveTvTime(programme.start))–\(liveTvTime(programme.end))")
                            .font(.subheadline).foregroundStyle(Palette.muted)
                        LiveTvProgressLine(value: airing.progress ?? 0, height: 3)
                        if let context = recordingContext(channel: channel, programme: programme) {
                            Button { showingDvrActivity = true } label: {
                                Label(context, systemImage: "record.circle.fill")
                            }
                        }
                        programmeActions(channel, programme)
                    }
                    HStack {
                        Button { fullscreen = true; overlayVisible = true } label: {
                            Label("Fullscreen", systemImage: "arrow.up.left.and.arrow.down.right")
                        }
                        Button { showingTouchGuide = true } label: {
                            Label("Guide", systemImage: "rectangle.split.3x3")
                        }
                        Menu("More") {
                            Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                            if pictureInPicture.isSupported {
                                Button("Picture in picture") { pictureInPicture.toggle() }
                            }
                            Button("Stream information") { showingInfo = true }
                            Button("Stop") { Task { await live.stop() } }
                        }
                    }.buttonStyle(.bordered)
                    if let next = airing.next {
                        Button { detailChannel = channel; detail = next } label: {
                            VStack(alignment: .leading, spacing: 6) {
                                Text("UP NEXT · \(liveTvTime(next.start))").font(.caption.bold())
                                Text(next.title).font(.headline)
                            }.frame(maxWidth: .infinity, alignment: .leading).padding(16)
                                .background(Palette.surface, in: RoundedRectangle(cornerRadius: 12))
                        }.buttonStyle(.plain)
                    }
                }
            }.padding(.bottom, 24)
        }
    }

    private func touchGuidePanel(onClose: @escaping () -> Void) -> some View {
        VStack(spacing: 12) {
            HStack {
                Text("Guide").font(.headline)
                Spacer()
                Menu("Guide time") {
                    Button("Earlier") { pageGuide(by: -1) }.disabled(!canPageGuide(by: -1))
                    Button("Now") { returnGuideToNow() }
                    Button("Later") { pageGuide(by: 1) }.disabled(!canPageGuide(by: 1))
                }
                Button("Close", action: onClose)
            }
            guideGrid
        }.padding(16).background(Palette.bg)
    }

    private var guideGrid: some View {
        GeometryReader { geometry in
            LiveTvGuideGrid(
                layout: LiveTvGuideReducer.gridLayout(
                    guide: live.guide, channels: visibleChannels,
                    window: LiveTvGridMetrics.window(start: guideWindowStart), now: now,
                    slotSeconds: LiveTvGridMetrics.slotSeconds,
                    pxPerSlot: LiveTvGridMetrics.pxPerSlot(contentWidth: Double(geometry.size.width))),
                dimensions: LiveTvGridMetrics.dimensions(contentWidth: Double(geometry.size.width)),
                slots: LiveTvGuideReducer.gridSlots(
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    slotSeconds: LiveTvGridMetrics.slotSeconds),
                playingChannelId: live.watching?.id,
                marks: dvr.marks,
                now: now,
                onAiring: selectAiring,
                onFuture: { channel, programme in detailChannel = channel; detail = programme },
                restoreChannelId: focusedGuideChannelId,
                restoreProgrammeStart: focusedGuideProgrammeStart,
                restoreChannelHeader: focusedGuideChannelHeader,
                restoreAnchorTime: guideAnchorTime,
                onFocus: rememberGuideFocus,
                onToolbarBoundary: {},
                restoreRequest: 0,
                restoreAllowed: false,
                onFocusOwnershipChanged: { _ in })
        }
    }

    private var mobileSchedule: some View {
        let channel = scheduleChannelId.flatMap { id in visibleChannels.first { $0.id == id } }
            ?? live.watching
            ?? visibleChannels.first
        let programmes = channel.flatMap { selected in
            live.guide?.channels.first { $0.id == selected.id }?.programmes
        } ?? []
        return VStack(spacing: 0) {
            HStack {
                Picker("Channel", selection: Binding(
                    get: { channel?.id ?? "" },
                    set: { scheduleChannelId = $0 }
                )) {
                    ForEach(visibleChannels) { Text($0.title).tag($0.id) }
                }
                Spacer()
                Button("Grid") {
                    mobileGuideGrid = true
                    SettingsStore().liveTvMobileGuideUsesGrid = true
                }
                .font(.system(size: 13, weight: .semibold))
            }
            .frame(height: 44)
            .padding(.horizontal, 12)
            if programmes.isEmpty, let channel {
                Button("No programme information · Watch live") {
                    if channel.watchable { selectAiring(channel) }
                }
                .disabled(!channel.watchable)
            }
            List(programmes) { programme in
                Button {
                    // The same rule as the grid: every row opens the sheet,
                    // because the sheet is where a programme's actions live on
                    // the phone and Watch is the first of them.
                    guard let channel else { return }
                    detailChannel = channel
                    detail = programme
                } label: {
                    HStack(spacing: 10) {
                        Text(liveTvTime(programme.start))
                            .font(.system(size: 12)).foregroundStyle(Palette.muted)
                            .frame(width: 62, alignment: .leading)
                        Text(programme.title).font(.system(size: 14)).lineLimit(1)
                        Spacer(minLength: 4)
                        if let mark = channel.flatMap({
                            dvr.marks.mark(channelId: $0.id, airingStart: programme.start)
                        }) {
                            DvrCellMarkView(mark: mark)
                        }
                        if programme.start <= now && programme.end > now {
                            Text("NOW").font(.system(size: 10, weight: .bold))
                                .foregroundStyle(Palette.accent)
                        }
                    }
                    .frame(height: 44)
                }
                .buttonStyle(.plain)
                .listRowInsets(EdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 12))
            }
            .listStyle(.plain)
        }
        .onAppear { if scheduleChannelId == nil { scheduleChannelId = channel?.id } }
    }
    #endif

    #if os(tvOS)
    private var focusedTvChannel: LiveTvChannel? {
        tvFocusedChannelId.flatMap { id in visibleChannels.first { $0.id == id } }
            ?? live.watching
            ?? visibleChannels.first
    }

    /// Lists are tall and narrow; grids are wide. On now is a 620 pt column
    /// beside a large picture; Guide is a short stage over a full-width grid.
    @ViewBuilder private func tvBrowseRegion(in geometry: GeometryProxy) -> some View {
        let contentWidth = max(640, Double(geometry.size.width) - 2 * Double(liveContentInset))
        let contentHeight = max(320, Double(geometry.size.height) - 48 - 20)
        switch tvLayout.presented {
        case _ where browse == .recordings:
            // Full width, and under the toolbar rather than beside a picture:
            // a schedule is a list of rows with long titles and long reasons,
            // and the 620 pt column the channel list wants would cut them.
            DvrRecordingsPanel(dvr: dvr, now: now)
        case .guideOverlay:
            overPicturePanel(contentWidth: contentWidth, contentHeight: contentHeight, rows: 5)
        case .guidePreview, .channelBrowser:
            if browse == .guide {
                VStack(spacing: 14) {
                    guideStage(contentWidth: contentWidth)
                    tvBrowseContent(contentWidth: contentWidth)
                }
            } else {
                HStack(alignment: .top, spacing: 24) {
                    tvChannelList.frame(width: LiveTvView.tvListColumnWidth)
                    onNowDetail(contentWidth: contentWidth, contentHeight: contentHeight)
                }
            }
        }
    }

    static let tvListColumnWidth: CGFloat = 620

    /// The picture and everything known about what is on it, to the right of
    /// the list. The picture takes the width the list does not.
    private func onNowDetail(contentWidth: Double, contentHeight: Double) -> some View {
        let pictureWidth = max(420, contentWidth - Double(LiveTvView.tvListColumnWidth) - 24)
        let pictureHeight = min(pictureWidth * 9 / 16, max(220, contentHeight - 120))
        return VStack(alignment: .leading, spacing: 12) {
            livePicture(width: pictureWidth, height: pictureHeight)
            focusedProgrammeDetails(synopsisLines: 2, eyebrow: false)
            if let channel = focusedTvChannel { channelSchedule(channel) }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    /// A 302 pt stage: 16:9 picture on the left, what is focused on the right.
    private func guideStage(contentWidth: Double) -> some View {
        HStack(alignment: .top, spacing: 24) {
            livePicture(width: 537, height: 302)
            focusedProgrammeDetails(synopsisLines: 3, eyebrow: true)
                .frame(maxWidth: .infinity, alignment: .topLeading)
        }
        .frame(height: 302)
    }

    /// Fullscreen video with one opaque panel along the bottom. The panel
    /// never auto-hides: it is the layout, not the overlay.
    private func overPicturePanel(
        contentWidth: Double, contentHeight: Double, rows: Int
    ) -> some View {
        let height = min(contentWidth * 9 / 16, contentHeight)
        return ZStack(alignment: .bottom) {
            livePicture(width: height * 16 / 9, height: height)
            VStack(alignment: .leading, spacing: 10) {
                HStack(spacing: 12) {
                    Text(overPictureHeadline).font(LiveTvType.primary).lineLimit(1)
                    Spacer(minLength: 8)
                    Button { favoritesOnly.toggle(); requestChannelFocus() } label: {
                        Label("Favorites", systemImage: favoritesOnly ? "star.fill" : "star")
                    }
                    .buttonStyle(TVReadableButtonStyle(prominent: favoritesOnly, compact: true))
                    .focusEffectDisabled()
                    Button("Close") { tvLayout = .guidePreview }
                        .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                        .focusEffectDisabled()
                }
                tvBrowseContent(contentWidth: max(640, contentWidth - 40), rows: rows)
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
            .frame(height: 520)
            .background(Palette.bg)
        }
    }

    private var overPictureHeadline: String {
        let channel = focusedTvChannel
        let airing = channel.map { live.airing($0, now: now) } ?? .none
        return [
            "Guide",
            channel.map { "Focused: \($0.guideNumber) \($0.guideName)" },
            airing.now?.title,
            airing.now.map { liveTvTime($0.start) }
        ].compactMap { $0 }.joined(separator: " · ")
    }

    @ViewBuilder private func livePicture(width: Double, height: Double) -> some View {
        Button { fullscreen = true } label: {
            ZStack {
                if live.playing && !fullscreen {
                    PlayerSurface(
                        player: live.player,
                        pictureInPicture: pictureInPicture,
                        pgsOverlay: nil,
                        allowsPictureInPicture: true
                    )
                } else {
                    ZStack {
                        Palette.surface
                        VStack(spacing: 8) {
                            Image(systemName: "tv")
                            Text("Select a channel to watch live").font(LiveTvType.secondary)
                        }
                        .foregroundStyle(Palette.muted)
                    }
                }
            }
            .frame(width: width, height: height)
            .clipped()
            .overlay(alignment: .topLeading) {
                if let watching = live.watching, live.playing {
                    HStack(spacing: 8) {
                        Text(watching.guideName)
                            .font(LiveTvType.badge)
                            .padding(.horizontal, 8).padding(.vertical, 3)
                            .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 6))
                        Text(watching.title).font(LiveTvType.secondary)
                        Text("LIVE").font(LiveTvType.eyebrow).foregroundStyle(.white)
                            .padding(.horizontal, 7).padding(.vertical, 2)
                            .background(.red, in: Capsule())
                    }
                    .padding(12)
                    if let context = recordingContext(channel: watching,
                                                      programme: live.airing(watching, now: now).now) {
                        Label(context, systemImage: "record.circle.fill")
                            .font(LiveTvType.secondary)
                            .foregroundStyle(.white)
                            .padding(.horizontal, 8).padding(.vertical, 4)
                            .background(.black.opacity(0.65), in: Capsule())
                            .padding(.leading, 12).padding(.top, 48)
                    }
                }
            }
            .overlay(alignment: .bottomTrailing) {
                if live.playing {
                    Text("Select · Fullscreen")
                        .font(LiveTvType.tertiary)
                        .foregroundStyle(.white.opacity(0.75))
                        .padding(12)
                }
            }
            .overlay(alignment: .bottom) {
                if live.playing, let channel = live.watching {
                    LiveTvProgressLine(value: live.airing(channel, now: now).progress ?? 0, height: 4)
                }
            }
        }
        .buttonStyle(LiveTvPictureButtonStyle())
        .focusEffectDisabled()
        .disabled(!live.playing)
    }

    private func focusedProgrammeDetails(synopsisLines: Int, eyebrow: Bool) -> some View {
        let channel = focusedTvChannel
        let airing = channel.map { live.airing($0, now: now) } ?? .none
        let programme = if browse == .guide,
                           let channel,
                           channel.id == focusedGuideChannelId,
                           let start = focusedGuideProgrammeStart {
            live.guide?.channels.first(where: { $0.id == channel.id })?
                .programmes.first(where: { $0.start == start }) ?? airing.now
        } else {
            airing.now
        }
        let meta = [
            programme.map { "\(liveTvTime($0.start))–\(liveTvTime($0.end))" },
            programme.flatMap { $0.end > now ? "\(max(0, ($0.end - now) / 60)) min left" : nil }
        ].compactMap { $0 }.joined(separator: " · ")
        return VStack(alignment: .leading, spacing: 6) {
            if eyebrow, let channel {
                Text("FOCUSED · \(channel.guideNumber) \(channel.guideName)"
                     + (programme.map { " · \(liveTvTime($0.start))–\(liveTvTime($0.end))" } ?? ""))
                    .font(LiveTvType.eyebrow).foregroundStyle(Palette.accent).lineLimit(1)
            }
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                Text(programme?.title ?? channel?.guideName ?? "Live TV")
                    .font(LiveTvType.title).lineLimit(1)
                if !eyebrow && !meta.isEmpty {
                    Text(meta).font(LiveTvType.secondary).foregroundStyle(Palette.muted).lineLimit(1)
                }
            }
            if let synopsis = programme?.synopsis {
                Text(synopsis).font(LiveTvType.secondary).lineLimit(synopsisLines)
            }
            if let channel, let context = recordingContext(channel: channel, programme: programme) {
                Label(context, systemImage: "record.circle.fill")
                    .font(LiveTvType.secondary).foregroundStyle(Palette.accent)
            }
            if let channel { LiveTvFormatBadges(channel: channel) }
            // Reached the way anything above the grid is reached: up from the
            // top row hands focus to the toolbar, and down from there walks
            // back through these before entering the cells again.
            if let channel, let programme { programmeActions(channel, programme) }
            if eyebrow, let footer = guideStageFooter {
                Text(footer).font(LiveTvType.tertiary).foregroundStyle(Palette.muted).lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private var guideStageFooter: String? {
        guard let watching = live.watching else { return nil }
        let airing = live.airing(watching, now: now)
        return [
            "Now playing: \(airing.now?.title ?? watching.title)",
            airing.now.flatMap { $0.end > now ? "\(max(0, ($0.end - now) / 60)) min left" : nil },
            "Select the picture to watch fullscreen"
        ].compactMap { $0 }.joined(separator: " · ")
    }

    @ViewBuilder private func tvBrowseContent(contentWidth: Double, rows: Int? = nil) -> some View {
        if visibleChannels.isEmpty {
            Button("Clear filters") { query = ""; favoritesOnly = false; hideProtected = false }
                .buttonStyle(TVReadableButtonStyle(prominent: true, compact: true))
                .focusEffectDisabled()
        } else if browse == .guide {
            LiveTvGuideGrid(
                layout: LiveTvGuideReducer.gridLayout(
                    guide: live.guide,
                    channels: visibleChannels,
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    now: now,
                    slotSeconds: LiveTvGridMetrics.slotSeconds,
                    pxPerSlot: LiveTvGridMetrics.pxPerSlot(contentWidth: contentWidth)
                ),
                dimensions: LiveTvGridMetrics.dimensions(contentWidth: contentWidth),
                slots: LiveTvGuideReducer.gridSlots(
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    slotSeconds: LiveTvGridMetrics.slotSeconds
                ),
                playingChannelId: live.watching?.id,
                marks: dvr.marks,
                now: now,
                onAiring: selectAiring,
                onFuture: { channel, programme in detailChannel = channel; detail = programme },
                restoreChannelId: focusedGuideChannelId,
                restoreProgrammeStart: focusedGuideProgrammeStart,
                restoreChannelHeader: focusedGuideChannelHeader,
                restoreAnchorTime: guideAnchorTime,
                onFocus: rememberGuideFocus,
                onToolbarBoundary: {
                    guideFocusRequested = false
                    focusedControl = .guide
                },
                restoreRequest: guideFocusRequest,
                restoreAllowed: guideFocusRequested && browse == .guide,
                onFocusOwnershipChanged: { active in guideFocusRequested = active },
                paging: LiveTvGuidePaging(
                    canEarlier: canPageGuide(by: -1),
                    canLater: canPageGuide(by: 1),
                    earlier: { pageGuide(by: -1) },
                    now: { returnGuideToNow() },
                    later: { pageGuide(by: 1) }
                )
            )
            // No fixed frame: the grid takes the height it is given. A frame
            // of `rows * rowHeight + 54` overflowed whatever was left and put
            // the last rows off the bottom of the screen.
            .frame(maxHeight: rows.map { CGFloat($0) * LiveTvGridMetrics.rowHeight + 34 } ?? .infinity)
        } else {
            tvChannelList
        }
    }

    private var tvChannelList: some View {
        // `ScrollView` + `LazyVStack` rather than `List`: a tvOS List row
        // stretches to the container and fights a fixed-width column, and the
        // 620 pt width is the whole point of the arrangement.
        ScrollViewReader { scroll in
        ScrollView {
            LazyVStack(spacing: 4) {
                ForEach(visibleChannels) { channel in
                    Button { selectAiring(channel) } label: {
                        LiveTvChannelRow(
                            channel: channel,
                            airing: live.airing(channel, now: now),
                            selected: live.watching?.id == channel.id
                        )
                    }
                    .buttonStyle(LiveTvChannelButtonStyle())
                    .focusEffectDisabled()
                    .focused($focusedChannelId, equals: channel.id)
                    .disabled(!channel.watchable || live.busy)
                    .id(channel.id)
                }
            }
        }
        .onChange(of: focusedChannelId) { _, channelId in
            channelFocusCoordinator.focusChanged(active: channelId != nil)
            channelFocusRequested = channelId != nil
            if let channelId { tvFocusedChannelId = channelId }
        }
        .task(id: channelFocusIdentity) {
            guard let ticket = channelFocusCoordinator.beginRestore(
                request: channelFocusRequest,
                ownerRequested: channelFocusRequested && browse == .list
            ), let target = nearestVisibleChannel(to: tvFocusedChannelId) else { return }
            tvFocusedChannelId = target.id
            // A `LazyVStack` only registers `.focused` for rows it has built,
            // so setting the binding to a row outside the built window leaves
            // the page with nothing focused at all. `List` did this for us.
            scroll.scrollTo(target.id, anchor: .center)
            await Task.yield()
            guard !Task.isCancelled,
                  browse == .list,
                  channelFocusCoordinator.permits(
                    ticket,
                    ownerRequested: channelFocusRequested
                  )
            else { return }
            focusedChannelId = target.id
        }
        }
    }

    private var channelFocusIdentity: String {
        "\(channelFocusRequest)#\(channelFocusRequested)#\(browse.rawValue)"
            + "#\(tvLayout.rawValue)#\(visibleChannels.map(\.id).joined(separator: ","))"
    }

    private func requestGuideFocus() {
        browse = .guide
        channelFocusRequested = false
        channelFocusCoordinator.leave()
        focusedChannelId = nil
        focusedControl = nil
        guideFocusRequest &+= 1
        guideFocusRequested = true
    }

    private func requestChannelFocus() {
        browse = .list
        guideFocusRequested = false
        focusedControl = nil
        channelFocusRequest &+= 1
        channelFocusRequested = true
    }

    private func cancelBrowseFocusRestoration() {
        guideFocusRequested = false
        channelFocusRequested = false
        channelFocusCoordinator.leave()
    }

    private func nearestVisibleChannel(to previousId: String?) -> LiveTvChannel? {
        if let previousId, let retained = visibleChannels.first(where: { $0.id == previousId }) {
            return retained
        }
        guard let first = visibleChannels.first else { return nil }
        guard let previousId,
              let oldIndex = live.channels.firstIndex(where: { $0.id == previousId })
        else { return first }
        return visibleChannels.min { left, right in
            let leftIndex = live.channels.firstIndex(where: { $0.id == left.id }) ?? 0
            let rightIndex = live.channels.firstIndex(where: { $0.id == right.id }) ?? 0
            return abs(leftIndex - oldIndex) < abs(rightIndex - oldIndex)
        }
    }

    private func channelSchedule(_ channel: LiveTvChannel) -> some View {
        let programmes = live.guide?.channels.first { $0.id == channel.id }?.programmes ?? []
        return VStack(alignment: .leading, spacing: 4) {
            Text("UP NEXT").font(LiveTvType.badge).foregroundStyle(Palette.muted)
            ForEach(programmes.filter { $0.end > now }.prefix(3)) { programme in
                HStack(spacing: 8) {
                    Text(liveTvTime(programme.start)).foregroundStyle(Palette.muted)
                    Text(programme.title).lineLimit(1)
                }
                .font(LiveTvType.tertiary)
            }
        }
    }
    #endif

    private func rememberGuideFocus(
        _ channel: LiveTvChannel,
        _ programme: LiveTvProgramme?,
        _ channelHeader: Bool,
        _ anchor: Int?
    ) {
        focusedGuideChannelId = channel.id
        focusedGuideProgrammeStart = programme?.start
        focusedGuideChannelHeader = channelHeader
        guideAnchorTime = anchor
        #if os(tvOS)
        tvFocusedChannelId = channel.id
        #endif
    }

    private var guidePageSpan: Int {
        LiveTvGridMetrics.visibleSlots * LiveTvGridMetrics.slotSeconds
    }

    private func guidePageStart(by delta: Int) -> Int {
        let proposed = guideWindowStart + delta * guidePageSpan
        guard let available = live.guide?.window else { return guideWindowStart }
        let latest = max(available.start, available.end - guidePageSpan)
        return min(max(proposed, available.start), latest)
    }

    private func canPageGuide(by delta: Int) -> Bool {
        guidePageStart(by: delta) != guideWindowStart
    }

    private func pageGuide(by delta: Int) {
        guideWindowStart = guidePageStart(by: delta)
    }

    private func returnGuideToNow() {
        let current = now - now % LiveTvGridMetrics.slotSeconds
        guard let available = live.guide?.window else {
            guideWindowStart = current
            return
        }
        let latest = max(available.start, available.end - guidePageSpan)
        guideWindowStart = min(max(current, available.start), latest)
    }

    /// Watch · Record · Record series · Remind me, for one guide cell.
    ///
    /// The same four wherever a programme can be acted on, so a viewer who
    /// learns them from the sofa does not have to learn different ones on the
    /// phone. Each of the last three reads the state the marks are already
    /// drawn from, which is why Record can offer to take a scheduled airing
    /// back off the plan without another read.
    @ViewBuilder private func programmeActions(
        _ channel: LiveTvChannel, _ programme: LiveTvProgramme
    ) -> some View {
        #if os(iOS)
        // Two rows on the phone. Four controls and a "Watch at 10:30 PM" label
        // do not fit one 320 pt line, and a row that overflows silently loses
        // its last button off the edge of the sheet.
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                watchAction(channel, programme)
                recordAction(channel, programme)
            }
            HStack(spacing: 8) {
                recordSeriesAction(channel, programme)
                remindAction(channel, programme)
            }
        }
        #else
        HStack(spacing: 8) {
            watchAction(channel, programme)
            recordAction(channel, programme)
            recordSeriesAction(channel, programme)
            remindAction(channel, programme)
        }
        #endif
    }

    private func watchAction(
        _ channel: LiveTvChannel, _ programme: LiveTvProgramme
    ) -> some View {
        let future = programme.start > now
        return DvrActionButton(
            label: future ? "Watch at \(liveTvTime(programme.start))" : "Watch",
            prominent: true
        ) {
            // Watching something that has not started is a promise to come
            // back, and a promise to come back is exactly a reminder.
            if future {
                Task { await dvr.remind(channelId: channel.id, airingStart: programme.start) }
            } else {
                detail = nil
                selectAiring(channel)
            }
        }
        .disabled(!channel.watchable)
    }

    private func recordAction(
        _ channel: LiveTvChannel, _ programme: LiveTvProgramme
    ) -> some View {
        let scheduled = dvr.recording(channelId: channel.id, airingStart: programme.start)
        let planned = scheduled.map { $0.state.pending } ?? false
        return DvrActionButton(label: planned ? "Don't record" : "Record") {
            Task {
                if let scheduled, planned { await dvr.delete(scheduled) }
                else { await dvr.record(channelId: channel.id, airingStart: programme.start) }
            }
        }
    }

    private func recordSeriesAction(
        _ channel: LiveTvChannel, _ programme: LiveTvProgramme
    ) -> some View {
        DvrActionButton(label: "Record series") {
            Task { await dvr.recordSeries(channelId: channel.id, airingStart: programme.start) }
        }
    }

    private func remindAction(
        _ channel: LiveTvChannel, _ programme: LiveTvProgramme
    ) -> some View {
        let reminder = dvr.reminder(channelId: channel.id, airingStart: programme.start)
        return DvrActionButton(label: reminder == nil ? "Remind me" : "Forget reminder") {
            Task {
                if let reminder { await dvr.forget(reminder) }
                else { await dvr.remind(channelId: channel.id, airingStart: programme.start) }
            }
        }
        .disabled(programme.start <= now)
    }

    private func selectAiring(_ channel: LiveTvChannel) {
        #if os(iOS)
        if showingTouchGuide {
            // Close the guide into the retained inline player. A cover cannot
            // be presented by a root that is already presenting this sheet.
            showingTouchGuide = false
            if !live.playing || live.watching?.id != channel.id {
                Task { await live.watch(channel) }
            }
            return
        }
        #endif
        if live.playing && live.watching?.id == channel.id {
            fullscreen = true
        } else {
            Task { await live.watch(channel) }
        }
    }

    #if os(tvOS)
    private var liveTelemetryStrip: some View {
        let chips = Self.liveSurfaceChips(
            status: live.status,
            delivery: live.status?.delivery ?? live.delivery,
            behindEdge: live.behindEdgeSeconds,
            paused: live.pausedAt
        )
        return HStack(spacing: 12) {
            ForEach(chips.filter { [.live, .behind].contains($0.kind) }) {
                liveSurfaceChip($0)
            }
            Spacer()
            ForEach(chips.filter { ![.live, .behind].contains($0.kind) }) {
                liveSurfaceChip($0)
            }
        }
        .padding(.horizontal, 80)
        .padding(.top, 48)
    }

    private func liveSurfaceChip(_ chip: LiveSurfaceChip) -> some View {
        HStack(spacing: 8) {
            if chip.kind == .live {
                Circle()
                    .fill(live.paused ? .white.opacity(0.36) : Palette.accent)
                    .frame(width: 10, height: 10)
                    .shadow(
                        color: Palette.accent.opacity(live.paused ? 0 : 0.65),
                        radius: 7
                    )
            }
            Text(chip.text).font(LiveTvType.surfaceMono)
        }
        .foregroundStyle(chip.kind == .live && !live.paused ? Palette.accent : .white.opacity(0.72))
        .padding(.horizontal, 16)
        .frame(height: 42)
        .background(
            Palette.playerChrome.opacity(0.62),
            in: RoundedRectangle(cornerRadius: 10, style: .continuous)
        )
    }

    @ViewBuilder
    private var liveWaitingTile: some View {
        if live.waiting && !live.paused {
            VStack(spacing: 20) {
                ProgressView().controlSize(.large)
                Text("Catching up to live")
                    .font(.system(size: 28, weight: .semibold))
                if let behind = live.behindEdgeSeconds {
                    Text(String(format: "%.1f s behind the edge", behind))
                        .font(LiveTvType.surfaceMono)
                        .foregroundStyle(.white.opacity(0.6))
                }
            }
            .foregroundStyle(.white)
            .padding(.horizontal, 44)
            .padding(.vertical, 34)
            .background(
                .ultraThinMaterial,
                in: RoundedRectangle(cornerRadius: 20, style: .continuous)
            )
        }
    }

    @ViewBuilder
    private var livePausedGlyph: some View {
        if live.paused {
            Circle()
                .fill(Palette.playerChrome.opacity(0.72))
                .frame(width: 132, height: 132)
                .overlay {
                    Image(systemName: "pause.fill")
                        .font(.system(size: 64, weight: .semibold))
                        .foregroundStyle(.white)
                }
        }
    }

    private func liveIdentityRow(
        channel: LiveTvChannel?,
        airing: LiveTvAiring
    ) -> some View {
        HStack(spacing: 28) {
            if let channel {
                Text(channel.guideNumber)
                    .font(.system(size: 40, weight: .bold, design: .monospaced))
                    .lineLimit(1)
                    .minimumScaleFactor(0.7)
                    .frame(width: 112, height: 112)
                    .background(
                        Palette.playerChrome.opacity(0.72),
                        in: RoundedRectangle(cornerRadius: 16, style: .continuous)
                    )
            }
            VStack(alignment: .leading, spacing: 6) {
                if let channel {
                    Text(
                        [channel.guideNumber, channel.guideName, channel.pictureClass]
                            .compactMap { $0 }
                            .joined(separator: " · ")
                    )
                    .font(LiveTvType.surfaceEyebrow)
                    .foregroundStyle(.white.opacity(0.6))
                    .lineLimit(1)
                }
                Text(airing.now?.title ?? live.title ?? "Live television")
                    .font(LiveTvType.surfaceTitle)
                    .lineLimit(1)
                if let channel,
                   let context = recordingContext(channel: channel, programme: airing.now) {
                    Label(context, systemImage: "record.circle.fill")
                        .font(LiveTvType.surfaceBody)
                        .foregroundStyle(Palette.accent)
                }
                let detail = [
                    airing.now?.episode,
                    airing.now?.episodeTitle,
                    airing.now?.synopsis,
                ].compactMap { $0 }.joined(separator: " · ")
                if !detail.isEmpty {
                    Text(detail)
                        .font(LiveTvType.surfaceBody)
                        .foregroundStyle(.white.opacity(0.6))
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
    }

    @ViewBuilder
    private func liveProgressRow(_ airing: LiveTvAiring) -> some View {
        if airing.now != nil {
            let values = Self.liveProgressText(airing: airing, now: now)
            HStack(spacing: 18) {
                Text(values[0])
                GeometryReader { geometry in
                    ZStack(alignment: .leading) {
                        Capsule().fill(.white.opacity(0.18))
                        Capsule()
                            .fill(Palette.accent)
                            .frame(width: geometry.size.width * (airing.progress ?? 0))
                    }
                }
                .frame(height: 6)
                Text(values[1])
                Text(values[2])
                    .foregroundStyle(.white.opacity(0.6))
                if values.count == 4 {
                    Text("·").foregroundStyle(.white.opacity(0.36))
                    Text(values[3])
                        .foregroundStyle(.white.opacity(0.6))
                        .lineLimit(1)
                }
            }
            .font(LiveTvType.surfaceMono)
        }
    }

    private func liveBottomBand(
        channel: LiveTvChannel?,
        airing: LiveTvAiring
    ) -> some View {
        VStack(alignment: .leading, spacing: 28) {
            liveIdentityRow(channel: channel, airing: airing)
            liveProgressRow(airing)
            if let message = live.surfaceMessage {
                Text(message)
                    .font(LiveTvType.surfaceMono)
                    .foregroundStyle(.white.opacity(0.6))
                    .lineLimit(2)
            }
            if dvr.indicatorLabel != nil {
                Button { showingDvrActivity = true } label: {
                    Label("Recording activity", systemImage: "record.circle")
                }
                .buttonStyle(LiveSurfacePillStyle())
                .focusEffectDisabled()
            }
            liveSurfaceButtons
        }
        .padding(.horizontal, 80)
        .padding(.bottom, 64)
        .foregroundStyle(.white)
    }

    private var liveSurfaceButtons: some View {
        HStack(spacing: 16) {
            Button { live.togglePause() } label: {
                Label(
                    live.paused ? "Play live" : "Pause",
                    systemImage: live.paused ? "play.fill" : "pause.fill"
                )
            }
            .focused($focusedControl, equals: .play)
            Button {
                temporaryGuide = true
                overlayGeneration &+= 1
                requestGuideFocus()
            } label: {
                Label("Guide", systemImage: "rectangle.grid.3x2")
            }
            .focused($focusedControl, equals: .guide)
            Button { requestChannelFocus(); fullscreen = false } label: {
                Label("Channels", systemImage: "list.bullet")
            }
            .focused($focusedControl, equals: .channels)
            Button { showStreamInfo() } label: {
                Label("Info", systemImage: "info.circle")
            }
            .focused($focusedControl, equals: .info)
            Button { showingMore = true } label: {
                Label("More", systemImage: "ellipsis")
            }
            .focused($focusedControl, equals: .more)
            Spacer()
            Text("MENU hides · PLAY/PAUSE \(live.paused ? "resumes" : "pauses")")
                .font(LiveTvType.surfaceHint)
                .foregroundStyle(.white.opacity(0.36))
        }
        .buttonStyle(LiveSurfacePillStyle())
        .focusEffectDisabled()
    }
    #endif

    /// Fullscreen is a surface, not a bare element: channel and programme in
    /// the corner, a strip of neighbours along the bottom, and a progress bar —
    /// all auto-hiding after the contract's four seconds while playing.
    private var fullscreenSurface: some View {
        let channel = live.watching
        let airing = channel.map { live.airing($0, now: now) } ?? .none
        let visible = visibleChannels
        return ZStack {
            Color.black.ignoresSafeArea()
            PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                          pgsOverlay: nil, allowsPictureInPicture: true)
                .ignoresSafeArea()
            #if os(tvOS)
            // tvOS delivers move/exit/playPause commands only to the focused
            // view and its ancestors, and `PlayerSurfaceView` refuses focus.
            // Without this layer the overlay's own four-second auto-hide
            // removed the last focusable view from the cover and the surface
            // went permanently deaf — every `hidden → reveal` row of the
            // contract unreachable, with no way back to any chrome. The finite
            // player solves it the same way (PlayerView.swift's `.reveal`).
            Color.clear
                .contentShape(Rectangle())
                .focusable(!overlayVisible && !temporaryGuide)
                .focused($focusedControl, equals: FocusTarget.reveal)
                .accessibilityHidden(true)
                // `liveInputState`, never a hardcoded `.fullscreenHidden`.
                // This layer takes every direction through `onMoveCommand`,
                // so while it holds focus the engine never sees one - and
                // `focusable(false)` does not relocate focus until the next
                // focus update, so it can hold focus for a moment after the
                // overlay is back. Asserting the hidden state made that
                // moment permanent: every direction routed to `reveal`, which
                // re-showed an overlay that was already up. Reporting the
                // real state routes it to `focusControl` instead, which
                // returns false and lets the framework move focus off.
                .liveTvRemoteAdapter(
                    .revealSurface,
                    state: { liveInputState },
                    apply: { outcome, _ in applyLiveOutcome(outcome) }
                )
            #endif
            #if os(tvOS)
            liveWaitingTile
            livePausedGlyph
            if overlayVisible && !temporaryGuide {
                ZStack(alignment: .top) {
                    LinearGradient(
                        colors: [.black.opacity(0.76), .clear],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .frame(height: 190)
                    liveTelemetryStrip
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)

                ZStack(alignment: .bottom) {
                    LinearGradient(
                        colors: [.clear, .black.opacity(0.92)],
                        startPoint: .top,
                        endPoint: .bottom
                    )
                    .frame(height: 520)
                    liveBottomBand(channel: channel, airing: airing)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
            }
            #else
            if overlayVisible {
                VStack {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(airing.now?.title ?? live.title ?? "Live television")
                                .font(LiveTvType.title)
                            Text(channel?.title ?? "").font(.caption)
                            if let channel {
                                Text(liveTvTechnicalSummary(channel, status: live.status, delivery: live.delivery))
                                    .font(.caption2).opacity(0.78)
                            }
                            if let now = airing.now {
                                Text("\(liveTvTime(now.start))–\(liveTvTime(now.end))"
                                     + (airing.next.map { " · Next: \($0.title)" } ?? ""))
                                    .font(.caption2).opacity(0.85)
                            }
                            if let channel,
                               let context = recordingContext(channel: channel, programme: airing.now) {
                                Button { showingDvrActivity = true } label: {
                                    Label(context, systemImage: "record.circle.fill")
                                }
                                .font(.caption.weight(.semibold))
                            }
                        }
                        Spacer()
                        HStack {
                            Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                            if pictureInPicture.isSupported { Button("PiP") { pictureInPicture.toggle() } }
                            Button("Guide") { temporaryGuide = true; overlayGeneration &+= 1 }
                            Button("Exit") { temporaryGuide = false; fullscreen = false }
                        }
                    }
                    Spacer()
                    VStack(alignment: .leading, spacing: 8) {
                        ProgressView(value: airing.progress ?? 0).tint(.white)
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 8) {
                                ForEach(visible) { entry in
                                    // Through the coalescer, not straight to
                                    // `watch`: running along the strip must be
                                    // one tuner start, not one per card.
                                    Button { live.requestChannel(entry) } label: {
                                        VStack(alignment: .leading, spacing: 2) {
                                            Text(entry.title).font(.caption.weight(.semibold))
                                            Text(live.airing(entry, now: now).now?.title ?? "—")
                                                .font(.caption2).opacity(0.8).lineLimit(1)
                                        }
                                        .frame(width: 150, alignment: .leading)
                                        .padding(8)
                                        .background(entry.id == channel?.id ? .white.opacity(0.18) : .black.opacity(0.5))
                                        .clipShape(RoundedRectangle(cornerRadius: 6))
                                    }
                                    .buttonStyle(.plain)
                                    .disabled(!entry.watchable)
                                }
                            }
                        }
                    }
                }
                .padding()
                .foregroundStyle(.white)
            }
            #endif
            #if os(tvOS)
            if temporaryGuide {
                // The identical panel the Over picture layout draws, so the
                // grid a viewer meets from fullscreen is the grid they know.
                GeometryReader { geometry in
                    VStack {
                        Spacer()
                        VStack(alignment: .leading, spacing: 10) {
                            HStack(spacing: 12) {
                                Text(overPictureHeadline).font(LiveTvType.primary).lineLimit(1)
                                Spacer(minLength: 8)
                                Button("Close") {
                                    temporaryGuide = false
                                    guideFocusRequested = false
                                    focusedControl = .guide
                                }
                                    .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                                    .focusEffectDisabled()
                            }
                            tvBrowseContent(
                                contentWidth: max(640, Double(geometry.size.width) - 40),
                                rows: 5
                            )
                        }
                        .padding(20)
                        .frame(height: 520)
                        .background(Palette.bg)
                    }
                }
                .transition(.move(edge: .bottom))
            }
            #else
            if temporaryGuide {
                GeometryReader { geometry in
                    VStack {
                        Spacer(minLength: 0)
                        touchGuidePanel {
                            temporaryGuide = false
                            overlayGeneration &+= 1
                        }
                        .frame(height: max(220, geometry.size.height * 0.6))
                        .clipShape(RoundedRectangle(cornerRadius: 16))
                    }.padding(12)
                }
            }
            #endif
        }
        .contentShape(Rectangle())
        #if os(tvOS)
        .liveTvRemoteAdapter(
            .root,
            state: { liveInputState },
            apply: { outcome, _ in applyLiveOutcome(outcome) }
        )
        #else
        .onTapGesture {
            // The touch surface's whole contract: a tap toggles the chrome.
            guard !temporaryGuide else { return }
            overlayVisible.toggle()
            overlayGeneration &+= 1
        }
        #endif
        .task(id: overlayGeneration) {
            guard overlayVisible, !temporaryGuide, !showingInfo, !showingMore, !showingLayout,
                  detail == nil, live.playing, !live.paused else { return }
            try? await Task.sleep(nanoseconds: LiveTvInputRouting.overlayAutoHideNanoseconds)
            guard !Task.isCancelled, !temporaryGuide, !showingInfo, !showingMore, !showingLayout,
                  detail == nil, live.playing, !live.paused else { return }
            overlayVisible = false
        }
        #if os(tvOS)
        .onAppear { focusedControl = overlayVisible ? .play : .reveal }
        .onChange(of: overlayVisible) { _, visible in
            if visible {
                // Deferred like the hide direction below, and for the same
                // reason: the buttons are inserted by this very update, and
                // tvOS drops a `@FocusState` aimed at a view that does not
                // exist yet. Undeferred, the assignment was dropped, focus
                // stayed on the reveal layer, and because it had not changed
                // the bounce in `onChange(of: focusedControl)` never ran.
                focusedControl = nil
                Task { @MainActor in
                    await Task.yield()
                    guard fullscreen, overlayVisible else { return }
                    focusedControl = .play
                }
            } else {
                focusedControl = nil
                Task { @MainActor in
                    await Task.yield()
                    guard fullscreen, !overlayVisible, !temporaryGuide,
                          !showingInfo, !showingMore, !showingLayout, detail == nil
                    else { return }
                    focusedControl = .reveal
                }
            }
        }
        .onChange(of: focusedControl) { _, target in
            if overlayVisible { overlayGeneration &+= 1 }
            if overlayVisible, target == .reveal { focusedControl = .play }
        }
        .onChange(of: temporaryGuide) { _, up in
            overlayGeneration &+= 1
            if !up, fullscreen { focusedControl = overlayVisible ? .play : .reveal }
        }
        #endif
        .onChange(of: showingInfo) { _, visible in
            overlayGeneration &+= 1
            #if os(tvOS)
            guard !visible, fullscreen, overlayVisible else { return }
            focusedControl = nil
            Task { @MainActor in
                await Task.yield()
                guard fullscreen, overlayVisible, !temporaryGuide,
                      !showingInfo, !showingMore, !showingLayout, detail == nil
                else { return }
                focusedControl = .play
            }
            #endif
        }
        .onChange(of: showingMore) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: showingLayout) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: live.paused) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: live.playing) { _, playing in
            if !playing && !live.busy {
                fullscreen = false
                temporaryGuide = false
            }
        }
        // Outside the remote adapter on purpose: a press that lands on one of
        // the reminder's buttons is the button's, and the routing table never
        // sees it. Back and Menu still leave the cover through the platform.
        .overlay(alignment: .bottomLeading) { reminderOverlay }
    }

    /// Every ten-foot press lands here, already decided by the shared table.
    /// The one ruling this must preserve: a direction on a hidden overlay only
    /// reveals it — it never changes channel behind a picture nobody can see.
    private func applyLiveOutcome(_ outcome: LiveTvInputOutcome) -> Bool {
        switch outcome {
        case .reveal:
            overlayVisible = true
            overlayGeneration &+= 1
            return true
        case .hide:
            overlayVisible = false
            return true
        case .toggleChrome:
            overlayVisible.toggle()
            overlayGeneration &+= 1
            return true
        case .togglePlay:
            live.togglePause()
            return true
        case .returnBrowser:
            fullscreen = false
            temporaryGuide = false
            return true
        case .closePanel:
            temporaryGuide = false
            showingInfo = false
            showingMore = false
            showingLayout = false
            detail = nil
            #if os(tvOS)
            focusedControl = .guide
            #endif
            return true
        case .exit:
            Task { await live.stop(); onLeave() }
            return true
        case .activate, .delegate, .focusControl, .focusCell, .focusPanel,
             .channelUp, .channelDown, .stripPrev, .stripNext, .tune, .ignore:
            // Not reachable on the ten-foot surface; the table says so and the
            // framework owns delegated focus and button activation exactly once.
            return false
        }
    }

    /// A programme, and what can be done about it. On the phone this is where
    /// a guide cell's actions live, which is why the cell on air opens it too:
    /// otherwise the one programme a viewer most wants to keep would be the
    /// one cell with no way to record it.
    private func programmeDetail(_ programme: LiveTvProgramme) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(programme.title).font(LiveTvType.title)
            Text("\(detailChannel?.title ?? "") · \(liveTvTime(programme.start))–\(liveTvTime(programme.end))"
                 + (programme.episode.map { " · \($0)" } ?? ""))
                .font(.caption).foregroundStyle(Palette.muted)
            if let episodeTitle = programme.episodeTitle {
                Text(episodeTitle).font(LiveTvType.primary)
            }
            if let synopsis = programme.synopsis { Text(synopsis).font(LiveTvType.secondary) }
            if let filters = programme.filters, !filters.isEmpty {
                Text(filters.joined(separator: " · ")).font(.caption2).foregroundStyle(Palette.muted)
            }
            if let channel = detailChannel {
                if let context = recordingContext(channel: channel, programme: programme) {
                    Label(context, systemImage: "record.circle.fill")
                        .font(.caption.weight(.semibold)).foregroundStyle(Palette.accent)
                }
                programmeActions(channel, programme)
            }
            if let message = dvr.message {
                Text(message).font(.caption2).foregroundStyle(Palette.muted).lineLimit(3)
            }
            Spacer()
        }
        .padding()
        .background(Palette.bg)
    }
}
