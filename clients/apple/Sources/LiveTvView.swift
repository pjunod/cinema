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
    /// The guide is a second, independent read. It never gates the lineup and
    /// never gates a start: a page that waited on it would be a page that
    /// cannot tune while a guide host is slow.
    @Published private(set) var guide: LiveTvGuide?
    @Published private(set) var watching: LiveTvChannel?
    @Published private(set) var status: LiveTvStatus?
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
        } catch {
            if loadId == loading { message = error.localizedDescription }
        }
    }

    func watch(_ channel: LiveTvChannel) async {
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
            let item = AVPlayerItem(url: try api.playlistURL(info.sessionId))
            item.preferredForwardBufferDuration = 12
            player.replaceCurrentItem(with: item)
            // Live TV owns a separate AVPlayer from finite-media playback, so
            // it must establish the same playback audio session itself. The
            // default category follows the iPhone silent switch: video moves,
            // but the AAC track is inaudible.
            beginAudioSession()
            title = channel.title
            watching = channel
            playing = true
            player.play()
            message = "Playing live · no recording or rewind"
            heartbeat = Task { @MainActor [weak self] in
                var progress = LiveTvPlaybackWatchdog()
                while !Task.isCancelled {
                    do { try await Task.sleep(nanoseconds: 5_000_000_000) } catch { return }
                    guard let self, self.serial == expected else { return }
                    do {
                        if item.status == .failed { throw Self.playerFailure(item.error) }
                        let position = self.player.currentTime().seconds
                        if !self.paused && progress.observe(position: position) {
                            try await api.keepalive(info.sessionId)
                            guard self.serial == expected else { return }
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
                            self.expireSourceFormats(now: Int(Date().timeIntervalSince1970))
                        } else if progress.expired {
                            if self.paused {
                                self.message = "Paused for 30 seconds. The tuner was released; select a channel to resume live."
                                await self.stop()
                                return
                            }
                            throw LiveTvFailure(code: "stream_failed")
                        }
                    } catch {
                        guard self.serial == expected else { return }
                        self.message = error.localizedDescription
                        await self.stop()
                        return
                    }
                }
            }
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

    /// Twenty minutes, matching the owner's own refresh cadence. Kept on the
    /// controller rather than in `.task {}` on a view, for the same reason the
    /// heartbeat is: leaving a tab must not forget the guide.
    static let guideRefreshNanoseconds: UInt64 = 20 * 60 * 1_000_000_000

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
                if let fetched = try? await api.guide(
                    from: from, hours: LiveTvGridMetrics.requestedHours) {
                    guard self.loadId == loading else { return }
                    self.guide = fetched
                }
                do { try await Task.sleep(nanoseconds: Self.guideRefreshNanoseconds) } catch { return }
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

    func togglePause() {
        guard playing else { return }
        paused.toggle()
        if paused {
            player.pause()
            message = "Paused. The tuner is released after 30 seconds without playback; resuming has no rewind guarantee."
        } else {
            player.play()
            message = "Playing live · no recording or rewind"
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
        watching = nil
        status = nil
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

    var id: Self { self }
    var label: String { self == .list ? "On now" : "Guide" }

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

/// Half-hour geometry, shared by the phone grid and the tests.
enum LiveTvGridMetrics {
    static let slotSeconds = 1800
    #if os(iOS)
    static let pxPerSlot: Double = 160
    static let rowHeight: Double = 56
    static let channelColumnWidth: Double = 128
    static let visibleSlots = 8
    #else
    static let pxPerSlot: Double = 300
    static let rowHeight: Double = 82
    static let channelColumnWidth: Double = 210
    static let visibleSlots = 3
    #endif
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

private let liveTvClock: DateFormatter = {
    let formatter = DateFormatter()
    formatter.timeStyle = .short
    formatter.dateStyle = .none
    return formatter
}()

func liveTvTime(_ unix: Int) -> String {
    liveTvClock.string(from: Date(timeIntervalSince1970: TimeInterval(unix)))
}

func liveTvTechnicalSummary(_ channel: LiveTvChannel, status: LiveTvStatus?) -> String {
    var facts = [String]()
    if let source = channel.sourceFormatDescription { facts.append(source) }
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
                        .font(.system(size: 9, weight: .bold))
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

    private var delivery: String? {
        guard let status else { return nil }
        let encoder = status.encoder == "pending" ? "" : " · \(status.encoder.uppercased()) encoder"
        return "H.264 · \(status.outputHeight)p · AAC\(encoder)"
    }

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
            if let delivery { detailRow("Playing", delivery) }
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

/// One channel row: chip, number and callsign, what is on with a bar to its
/// end, and what is next. A protected channel is dimmed, never hidden.
struct LiveTvChannelRow: View {
    let channel: LiveTvChannel
    let airing: LiveTvAiring
    let selected: Bool

    private var stationName: String {
        #if os(tvOS)
        channel.guideName
        #else
        String(channel.guideName.prefix(5))
        #endif
    }

    private var stationWidth: CGFloat {
        #if os(tvOS)
        116
        #else
        52
        #endif
    }

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(stationName)
                .font(.caption2.weight(.semibold))
                .lineLimit(1)
                .minimumScaleFactor(0.7)
                .frame(width: stationWidth, height: 30)
                .background(Palette.surfaceHi)
                .clipShape(RoundedRectangle(cornerRadius: 4))
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 5) {
                    Text(channel.title).font(.subheadline.weight(.semibold))
                    LiveTvFormatBadges(channel: channel)
                }
                if !channel.watchable {
                    Text("Protected channel · not playable")
                        .font(.caption).foregroundStyle(Palette.muted)
                } else if let now = airing.now {
                    Text(now.title).font(.caption).lineLimit(1)
                    ProgressView(value: airing.progress ?? 0)
                        .tint(Palette.accent)
                        .frame(maxWidth: 220)
                    Text(airing.next.map { "\(liveTvTime(now.end)) · Next: \($0.title)" } ?? liveTvTime(now.end))
                        .font(.caption2).foregroundStyle(Palette.muted).lineLimit(1)
                } else {
                    Text("No programme information")
                        .font(.caption).foregroundStyle(Palette.muted)
                }
            }
            Spacer(minLength: 4)
            VStack(spacing: 4) {
                if channel.favorite { Image(systemName: "star.fill").foregroundStyle(Palette.accent) }
                Image(systemName: channel.watchable ? "play.circle" : "lock")
            }
        }
        .padding(.vertical, 4)
        .opacity(channel.watchable ? 1 : 0.55)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(selected ? [.isSelected] : [])
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
            configuration.label
                .foregroundStyle(Palette.onBg)
                .padding(.horizontal, 18)
                .padding(.vertical, 9)
                .background(
                    Palette.surface.opacity(isFocused ? 1 : 0.45),
                    in: RoundedRectangle(cornerRadius: 12, style: .continuous)
                )
                .overlay {
                    RoundedRectangle(cornerRadius: 12, style: .continuous)
                        .stroke(isFocused ? Palette.accent : Palette.outline,
                                lineWidth: isFocused ? 3 : 1)
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

private struct LiveTvGuideScrollOriginKey: PreferenceKey {
    static var defaultValue: CGPoint = .zero
    static func reduce(value: inout CGPoint, nextValue: () -> CGPoint) {
        value = nextValue()
    }
}

/// The half-hour grid. One horizontal offset shared by every row, so the
/// channel column and the times cannot drift apart from the cells.
struct LiveTvGuideGrid: View {
    private struct FocusKey: Hashable {
        let channelId: String
        let programmeStart: Int?
        let channelHeader: Bool
    }

    let layout: LiveTvGridLayout
    let slots: [Int]
    let playingChannelId: String?
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
                    Color.clear.frame(width: LiveTvGridMetrics.channelColumnWidth)
                    ForEach(slots, id: \.self) { at in
                        Color.clear
                            .frame(width: LiveTvGridMetrics.pxPerSlot, alignment: .leading)
                    }
                }
                .frame(height: headerHeight)
                ForEach(layout.rows, id: \.channel.id) { row in
                    HStack(spacing: 0) {
                        Button { onAiring(row.channel) } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(row.channel.guideNumber).font(.caption.weight(.semibold))
                                Text(row.channel.guideName).font(.caption2).foregroundStyle(Palette.muted)
                                LiveTvFormatBadges(channel: row.channel)
                            }
                            .frame(width: LiveTvGridMetrics.channelColumnWidth,
                                   height: LiveTvGridMetrics.rowHeight,
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
                                              height: LiveTvGridMetrics.rowHeight)
                            ForEach(row.cells, id: \.programme.id) { cell in
                                Button {
                                    if cell.airing { onAiring(row.channel) }
                                    else { onFuture(row.channel, cell.programme) }
                                } label: {
                                    Text(cell.programme.title)
                                        .font(.caption).lineLimit(1)
                                        .padding(.horizontal, 6)
                                        .frame(width: max(cell.width - 4, 24),
                                               height: LiveTvGridMetrics.rowHeight - 8,
                                               alignment: .leading)
                                        .background(cell.airing ? Palette.surfaceHi : Palette.surface)
                                        .overlay(
                                            RoundedRectangle(cornerRadius: 5).stroke(
                                                cell.airing && row.channel.id == playingChannelId
                                                    ? Palette.accent : Palette.outline,
                                                lineWidth: 1)
                                        )
                                        .clipShape(RoundedRectangle(cornerRadius: 5))
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
                                .offset(x: cell.left, y: 4)
                                .accessibilityLabel(
                                    "\(row.channel.title), \(cell.programme.title), "
                                    + "\(liveTvTime(cell.programme.start)) to \(liveTvTime(cell.programme.end))")
                            }
                            if row.cells.isEmpty {
                                Button { onAiring(row.channel) } label: {
                                    Text("No programme information · Watch live")
                                        .font(.caption).lineLimit(1)
                                        .padding(.horizontal, 8)
                                        .frame(width: max(layout.totalWidth - 4, 120),
                                               height: LiveTvGridMetrics.rowHeight - 8,
                                               alignment: .leading)
                                        .background(Palette.surface)
                                        .overlay(RoundedRectangle(cornerRadius: 5).stroke(Palette.outline))
                                        .clipShape(RoundedRectangle(cornerRadius: 5))
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
                                .offset(x: 0, y: 4)
                            }
                        }
                    }
                    .frame(height: LiveTvGridMetrics.rowHeight)
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
                        .offset(x: LiveTvGridMetrics.channelColumnWidth + nowX + 8)
                        .accessibilityHidden(true)
                }
            }
        }
        .coordinateSpace(name: "live-tv-guide-scroll")
        .onPreferenceChange(LiveTvGuideScrollOriginKey.self) { scrollOrigin = $0 }
        .overlay(alignment: .topLeading) {
            ZStack(alignment: .topLeading) {
                Palette.bg.frame(width: LiveTvGridMetrics.channelColumnWidth + 8, height: headerHeight)
                HStack(spacing: 0) {
                    ForEach(slots, id: \.self) { at in
                        Text(liveTvTime(at))
                            .font(.caption2).foregroundStyle(Palette.muted)
                            .frame(width: LiveTvGridMetrics.pxPerSlot, alignment: .leading)
                    }
                }
                .offset(x: LiveTvGridMetrics.channelColumnWidth + 8 + scrollOrigin.x)
                .frame(height: headerHeight)
                .clipped()

            }
        }
        #if os(tvOS)
        .onChange(of: focusedCell) { _, target in
            guard let target else {
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
                moveFocus(input)
                return true
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

    #if os(tvOS)
    private func moveFocus(_ direction: LiveTvContractInput) {
        guard let current = focusedCell else { return }
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
    @State private var showingInfo = false
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
    #if os(iOS)
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    #endif
    #if os(tvOS)
    /// The one focusable thing on the ten-foot surface while the overlay is
    /// hidden. Its only job is to exist, so the remote has somewhere to send
    /// a press that the routing table can then decide.
    private enum FocusTarget: Hashable { case reveal, guide, channels, play, info, layout, more }
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

    var body: some View {
        GeometryReader { geometry in
          VStack(spacing: 12) {
            // Not while the cover is up. Both surfaces would exist, both would
            // call `attach` with a different layer on every body pass, and
            // `attach` begins by detaching — so the 30-second tick alone was
            // enough to stop a running PiP, and two AVPlayerLayers cannot both
            // render one AVPlayer anyway.
            #if os(iOS)
            if live.playing && !fullscreen {
                VStack(spacing: 8) {
                    PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                                  pgsOverlay: nil, allowsPictureInPicture: true)
                        .frame(maxWidth: .infinity)
                        .frame(height: min(geometry.size.width * 9 / 16, geometry.size.height * 0.4))
                    nowBar
                }
            }
            #endif
            statusMessage
            filterBar
            #if os(tvOS)
            tvBrowseRegion(in: geometry)
            #else
            actionBar
            browseRegion
            #endif
        }
        .padding()
        }
        .navigationTitle("Live TV")
        .background(Palette.bg)
        .task { await live.load(origin: model.origin, token: Session.shared.token) }
        .onReceive(tick) { _ in
            now = Int(Date().timeIntervalSince1970)
            live.expireSourceFormats(now: now)
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
        }
        .sheet(item: $detail) { programme in
            programmeDetail(programme)
        }
        .sheet(isPresented: $showingInfo) {
            if let channel = live.watching {
                LiveTvTechnicalDetails(channel: channel, status: live.status)
                    .padding(48)
                    .frame(minWidth: 420, minHeight: 260, alignment: .topLeading)
                    .background(Palette.bg)
            }
        }
        .sheet(isPresented: $showingLayout) { layoutPanel }
        .sheet(isPresented: $showingMore) { morePanel }
        #if os(tvOS)
        .sheet(isPresented: $showingSearch) {
            VStack(alignment: .leading, spacing: 28) {
                Text("Find a channel").font(.title2.weight(.semibold))
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
        .onAppear { onScreen = true }
    }

    private var liveInputState: LiveTvInputState {
        if temporaryGuide { return .temporaryGuide }
        if showingMore || showingLayout { return .menu }
        if showingInfo { return .streamInfo }
        if detail != nil { return .programmeDetails }
        return overlayVisible ? .fullscreenControls : .fullscreenHidden
    }

    private var statusMessage: some View {
        Text(live.message)
            .font(.callout)
            .foregroundStyle(Palette.muted)
            .multilineTextAlignment(.center)
            .lineLimit(3)
            .frame(maxWidth: 1050)
            #if os(tvOS)
            .padding(.horizontal, 24)
            .padding(.vertical, 12)
            .background(Palette.surface.opacity(0.72),
                        in: RoundedRectangle(cornerRadius: 12, style: .continuous))
            #endif
            .accessibilityIdentifier("live-tv-status")
    }

    private var layoutPanel: some View {
        VStack(alignment: .leading, spacing: 18) {
            Text("Live TV layout").font(.title2.weight(.semibold))
            ForEach(TvLiveLayout.allCases) { layout in
                Button {
                    tvLayout = layout
                    showingLayout = false
                } label: {
                    HStack {
                        Text(layout.label)
                        Spacer()
                        if layout == tvLayout { Image(systemName: "checkmark") }
                    }
                }
                #if os(tvOS)
                .buttonStyle(TVReadableButtonStyle(prominent: layout == tvLayout))
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
            Text("Live TV").font(.title2.weight(.semibold))
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

    private var actionBar: some View {
        HStack(spacing: 16) {
            Button {
                Task { await live.load(origin: model.origin, token: Session.shared.token) }
            } label: {
                Label("Refresh channels", systemImage: "arrow.clockwise")
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            .focusEffectDisabled()
            #endif

            Button {
                Task { await live.stop() }
            } label: {
                Label("Retry cleanup", systemImage: "stop.circle")
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false))
            .focusEffectDisabled()
            #endif
        }
    }

    private var filterBar: some View {
        VStack(spacing: 8) {
            #if os(iOS)
            Picker("View", selection: $browse) {
                ForEach(LiveTvBrowseView.allCases) { view in Text(view.label).tag(view) }
            }
            .pickerStyle(.segmented)
            .onChange(of: browse) { _, view in view.persist() }
            #endif
            HStack(spacing: 16) {
                #if os(iOS)
                Toggle("Favorites", isOn: $favoritesOnly).toggleStyle(.button)
                Toggle("Hide protected", isOn: $hideProtected).toggleStyle(.button)
                #else
                Button("Guide") { requestGuideFocus() }
                    .buttonStyle(TVReadableButtonStyle(prominent: browse == .guide))
                    .focusEffectDisabled()
                    .focused($focusedControl, equals: .guide)
                Button("On now") { requestChannelFocus() }
                    .buttonStyle(TVReadableButtonStyle(prominent: browse == .list && !favoritesOnly))
                    .focusEffectDisabled()
                    .focused($focusedControl, equals: .channels)
                if browse == .guide {
                    Button("Earlier") { pageGuide(by: -1) }
                        .disabled(!canPageGuide(by: -1))
                        .buttonStyle(TVReadableButtonStyle(prominent: false))
                        .focusEffectDisabled()
                    Button("Now") { returnGuideToNow() }
                        .buttonStyle(TVReadableButtonStyle(prominent: false))
                        .focusEffectDisabled()
                    Button("Later") { pageGuide(by: 1) }
                        .disabled(!canPageGuide(by: 1))
                        .buttonStyle(TVReadableButtonStyle(prominent: false))
                        .focusEffectDisabled()
                }
                Button { favoritesOnly.toggle(); requestChannelFocus() } label: {
                    Label("Favorites", systemImage: favoritesOnly ? "star.fill" : "star")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: favoritesOnly))
                .focusEffectDisabled()
                Button { showingSearch = true } label: {
                    Label(query.isEmpty ? "Search" : "Search: \(query)", systemImage: "magnifyingglass")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: !query.isEmpty))
                .focusEffectDisabled()
                Button { showingLayout = true } label: {
                    Label("Layout", systemImage: "rectangle.3.group")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .focusEffectDisabled()
                .focused($focusedControl, equals: .layout)
                if live.playing {
                    Button("Return to live") { fullscreen = true }
                        .buttonStyle(TVReadableButtonStyle(prominent: true))
                        .focusEffectDisabled()
                }
                Button { showingMore = true } label: {
                    Label("More", systemImage: "ellipsis")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false))
                .focusEffectDisabled()
                .focused($focusedControl, equals: .more)
                #endif
                Spacer()
                if let guide = live.guide, guide.freshness != "fresh" {
                    Text(guide.freshness == "stale" ? "Guide is stale" : "No guide data")
                        .font(.caption2).foregroundStyle(Palette.muted)
                }
            }
        }
    }

    @ViewBuilder private var browseRegion: some View {
        switch browse {
        case .list:
            List(visibleChannels) { channel in
                Button {
                    selectAiring(channel)
                } label: {
                    LiveTvChannelRow(channel: channel, airing: live.airing(channel, now: now),
                                     selected: live.watching?.id == channel.id)
                }
                .disabled(!channel.watchable || live.busy)
                #if os(tvOS)
                .buttonStyle(LiveTvChannelButtonStyle())
                .focusEffectDisabled()
                .listRowBackground(Color.clear)
                #endif
            }
            #if os(iOS)
            .searchable(text: $query, prompt: "Number, name, or what is on")
            #endif
        case .guide:
            #if os(iOS)
            if verticalSizeClass == .regular && horizontalSizeClass == .compact && !mobileGuideGrid {
                mobileSchedule
            } else {
                VStack(spacing: 8) {
                    if verticalSizeClass == .regular && horizontalSizeClass == .compact {
                        Button("Selected channel schedule") {
                            mobileGuideGrid = false
                            SettingsStore().liveTvMobileGuideUsesGrid = false
                        }
                    }
                    guideGrid
                }
            }
            #else
            guideGrid
            #endif
        }
    }

    private var guideGrid: some View {
            LiveTvGuideGrid(
                layout: LiveTvGuideReducer.gridLayout(
                    guide: live.guide, channels: visibleChannels,
                    window: LiveTvGridMetrics.window(start: guideWindowStart), now: now,
                    slotSeconds: LiveTvGridMetrics.slotSeconds,
                    pxPerSlot: LiveTvGridMetrics.pxPerSlot),
                slots: LiveTvGuideReducer.gridSlots(
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    slotSeconds: LiveTvGridMetrics.slotSeconds),
                playingChannelId: live.watching?.id,
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

    #if os(iOS)
    private var mobileSchedule: some View {
        let channel = scheduleChannelId.flatMap { id in visibleChannels.first { $0.id == id } }
            ?? live.watching
            ?? visibleChannels.first
        let programmes = channel.flatMap { selected in
            live.guide?.channels.first { $0.id == selected.id }?.programmes
        } ?? []
        return VStack(spacing: 10) {
            HStack {
                Picker("Channel", selection: Binding(
                    get: { channel?.id ?? "" },
                    set: { scheduleChannelId = $0 }
                )) {
                    ForEach(visibleChannels) { Text($0.title).tag($0.id) }
                }
                Button("Grid") {
                    mobileGuideGrid = true
                    SettingsStore().liveTvMobileGuideUsesGrid = true
                }
            }
            if programmes.isEmpty, let channel {
                Button("No programme information · Watch live") {
                    if channel.watchable { selectAiring(channel) }
                }
                .disabled(!channel.watchable)
            }
            List(programmes) { programme in
                Button {
                    guard let channel else { return }
                    if programme.start <= now && programme.end > now { selectAiring(channel) }
                    else { detailChannel = channel; detail = programme }
                } label: {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(programme.title).font(.headline)
                        Text("\(liveTvTime(programme.start))–\(liveTvTime(programme.end))")
                            .font(.caption).foregroundStyle(Palette.muted)
                    }
                }
            }
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

    @ViewBuilder private func tvBrowseRegion(in geometry: GeometryProxy) -> some View {
        switch tvLayout {
        case .guidePreview:
            VStack(spacing: 14) {
                HStack(spacing: 18) {
                    focusedProgrammeDetails
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                    livePicture
                        .frame(width: geometry.size.width * 0.4)
                }
                .frame(height: geometry.size.height * 0.28)
                tvBrowseContent(rows: 7)
            }
        case .guideOverlay:
            ZStack(alignment: .bottom) {
                livePicture
                VStack(spacing: 8) {
                    focusedProgrammeDetails.frame(maxWidth: .infinity, alignment: .leading)
                    tvBrowseContent(rows: 4)
                }
                .padding(18)
                .frame(maxHeight: geometry.size.height * 0.5)
                .background(Palette.bg.opacity(0.96))
            }
        case .channelBrowser:
            if browse == .guide {
                VStack(spacing: 14) {
                    focusedProgrammeDetails.frame(maxWidth: .infinity, alignment: .leading)
                    tvBrowseContent(rows: 7)
                }
            } else {
                HStack(spacing: 22) {
                    tvChannelList
                        .frame(width: geometry.size.width * 0.34)
                    VStack(spacing: 14) {
                        livePicture
                        focusedProgrammeDetails.frame(maxWidth: .infinity, alignment: .leading)
                        if let channel = focusedTvChannel {
                            channelSchedule(channel)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder private var livePicture: some View {
        if live.playing && !fullscreen {
            PlayerSurface(
                player: live.player,
                pictureInPicture: pictureInPicture,
                pgsOverlay: nil,
                allowsPictureInPicture: true
            )
            .aspectRatio(16 / 9, contentMode: .fit)
            .overlay(alignment: .topLeading) {
                if let watching = live.watching {
                    Text("WATCHING \(watching.guideNumber)")
                        .font(.caption.weight(.bold))
                        .padding(.horizontal, 10).padding(.vertical, 6)
                        .background(.black.opacity(0.72), in: Capsule())
                        .padding(12)
                }
            }
        } else {
            ZStack {
                Palette.surface
                VStack(spacing: 8) {
                    Image(systemName: "tv")
                    Text("Select a channel to watch live")
                }
                .foregroundStyle(Palette.muted)
            }
            .aspectRatio(16 / 9, contentMode: .fit)
        }
    }

    private var focusedProgrammeDetails: some View {
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
        return VStack(alignment: .leading, spacing: 8) {
            Text(programme?.title ?? channel?.guideName ?? "Live TV")
                .font(.title2.weight(.semibold)).lineLimit(2)
            if let channel {
                Text(channel.title).font(.body).foregroundStyle(Palette.muted)
                LiveTvFormatBadges(channel: channel)
                if let programme {
                    Text("\(liveTvTime(programme.start))–\(liveTvTime(programme.end))")
                        .font(.callout).foregroundStyle(Palette.muted)
                    if let synopsis = programme.synopsis {
                        Text(synopsis).font(.callout).lineLimit(3)
                    }
                }
            }
        }
        .padding(16)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14))
    }

    @ViewBuilder private func tvBrowseContent(rows: Int) -> some View {
        if visibleChannels.isEmpty {
            Button("Clear filters") { query = ""; favoritesOnly = false; hideProtected = false }
                .buttonStyle(TVReadableButtonStyle(prominent: true))
                .focusEffectDisabled()
        } else if browse == .guide {
            LiveTvGuideGrid(
                layout: LiveTvGuideReducer.gridLayout(
                    guide: live.guide,
                    channels: visibleChannels,
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    now: now,
                    slotSeconds: LiveTvGridMetrics.slotSeconds,
                    pxPerSlot: LiveTvGridMetrics.pxPerSlot
                ),
                slots: LiveTvGuideReducer.gridSlots(
                    window: LiveTvGridMetrics.window(start: guideWindowStart),
                    slotSeconds: LiveTvGridMetrics.slotSeconds
                ),
                playingChannelId: live.watching?.id,
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
                onFocusOwnershipChanged: { active in guideFocusRequested = active }
            )
            .frame(height: CGFloat(rows) * LiveTvGridMetrics.rowHeight + 54)
        } else {
            tvChannelList
        }
    }

    private var tvChannelList: some View {
        List(visibleChannels) { channel in
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
            .listRowBackground(Color.clear)
        }
        .listStyle(.plain)
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
        return VStack(alignment: .leading, spacing: 6) {
            Text("UP NEXT").font(.caption.weight(.bold)).foregroundStyle(Palette.muted)
            ForEach(programmes.filter { $0.end > now }.prefix(3)) { programme in
                HStack {
                    Text(liveTvTime(programme.start)).foregroundStyle(Palette.muted)
                    Text(programme.title).lineLimit(1)
                }
                .font(.callout)
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

    private func selectAiring(_ channel: LiveTvChannel) {
        if live.playing && live.watching?.id == channel.id {
            fullscreen = true
        } else {
            Task { await live.watch(channel) }
        }
    }

    private var nowBar: some View {
        let channel = live.watching
        let airing = channel.map { live.airing($0, now: now) } ?? .none
        return VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(airing.now?.title ?? live.title ?? "Live television")
                    .font(.headline).lineLimit(1)
                Text("LIVE").font(.caption2.weight(.bold))
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(.red).foregroundStyle(.white)
                    .clipShape(Capsule())
                Spacer()
            }
            if let now = airing.now {
                ProgressView(value: airing.progress ?? 0).tint(Palette.accent)
                Text("\(liveTvTime(now.start))–\(liveTvTime(now.end))"
                     + (airing.next.map { " · Next: \($0.title)" } ?? ""))
                    .font(.caption).foregroundStyle(Palette.muted).lineLimit(1)
            }
            if let channel { LiveTvFormatBadges(channel: channel) }
            HStack {
                Button(live.paused ? "Play live" : "Pause") { live.togglePause() }
                Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                if pictureInPicture.isSupported {
                    Button(pictureInPicture.isActive ? "Leave PiP" : "PiP") { pictureInPicture.toggle() }
                }
                Button("Info") { showingInfo = true }
                Button("Fullscreen") { fullscreen = true }
                Button("Stop") { Task { await live.stop() } }
            }
        }
    }

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
                .focusable(true)
                .focused($focusedControl, equals: FocusTarget.reveal)
                .accessibilityHidden(true)
                .liveTvRemoteAdapter(
                    .revealSurface,
                    state: { .fullscreenHidden },
                    apply: { outcome, _ in applyLiveOutcome(outcome) }
                )
            #endif
            if overlayVisible {
                VStack {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(airing.now?.title ?? live.title ?? "Live television")
                                .font(.title3.weight(.semibold))
                            Text(channel?.title ?? "").font(.caption)
                            if let channel {
                                Text(liveTvTechnicalSummary(channel, status: live.status))
                                    .font(.caption2).opacity(0.78)
                            }
                            if let now = airing.now {
                                Text("\(liveTvTime(now.start))–\(liveTvTime(now.end))"
                                     + (airing.next.map { " · Next: \($0.title)" } ?? ""))
                                    .font(.caption2).opacity(0.85)
                            }
                        }
                        Spacer()
                        HStack {
                            #if os(tvOS)
                            Button("Guide") {
                                temporaryGuide = true
                                overlayGeneration &+= 1
                                requestGuideFocus()
                            }
                                .focused($focusedControl, equals: .guide)
                            Button("Channels") { requestChannelFocus(); fullscreen = false }
                                .focused($focusedControl, equals: .channels)
                            Button(live.paused ? "Play live" : "Pause") { live.togglePause() }
                                .focused($focusedControl, equals: .play)
                            Button("Info") { showingInfo = true }
                                .focused($focusedControl, equals: .info)
                            Button("More") { showingMore = true }
                            .focused($focusedControl, equals: .more)
                            #else
                            Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                            if pictureInPicture.isSupported { Button("PiP") { pictureInPicture.toggle() } }
                            Button("Exit") { fullscreen = false }
                            #endif
                        }
                        #if os(tvOS)
                        .buttonStyle(TVReadableButtonStyle(prominent: false))
                        .focusEffectDisabled()
                        #endif
                    }
                    Spacer()
                    VStack(alignment: .leading, spacing: 8) {
                        ProgressView(value: airing.progress ?? 0).tint(.white)
                        #if os(iOS)
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
                        #endif
                    }
                }
                .padding()
                .foregroundStyle(.white)
            }
            #if os(tvOS)
            if temporaryGuide {
                VStack {
                    Spacer()
                    VStack(alignment: .leading, spacing: 10) {
                        HStack {
                            Text("Guide").font(.title2.weight(.semibold))
                            Spacer()
                            Button("Close") {
                                temporaryGuide = false
                                guideFocusRequested = false
                                focusedControl = .guide
                            }
                                .buttonStyle(TVReadableButtonStyle(prominent: false))
                                .focusEffectDisabled()
                        }
                        tvBrowseContent(rows: 4)
                    }
                    .padding(22)
                    .frame(maxHeight: 520)
                    .background(Palette.bg.opacity(0.97))
                }
                .transition(.move(edge: .bottom))
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
            overlayVisible.toggle()
            overlayGeneration &+= 1
        }
        #endif
        .task(id: overlayGeneration) {
            guard overlayVisible, !temporaryGuide, !showingInfo, !showingMore, !showingLayout,
                  live.playing, !live.paused else { return }
            try? await Task.sleep(nanoseconds: LiveTvInputRouting.overlayAutoHideNanoseconds)
            guard !Task.isCancelled, !temporaryGuide, !showingInfo, !showingMore, !showingLayout,
                  live.playing, !live.paused else { return }
            overlayVisible = false
        }
        #if os(tvOS)
        // Focus must land on the reveal layer whenever the overlay is not
        // there to hold it, or the next press goes nowhere.
        .onAppear { focusedControl = overlayVisible ? .guide : .reveal }
        .onChange(of: overlayVisible) { _, visible in
            focusedControl = visible ? .guide : .reveal
        }
        .onChange(of: focusedControl) { _, _ in
            if overlayVisible { overlayGeneration &+= 1 }
        }
        #endif
        .onChange(of: temporaryGuide) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: showingInfo) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: showingMore) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: showingLayout) { _, _ in overlayGeneration &+= 1 }
        .onChange(of: live.paused) { _, _ in overlayGeneration &+= 1 }
        // Deliberately NOT `onChange(of: live.playing)`. `watch()` detaches
        // before it awaits the new lease, so `playing` goes false mid-tune —
        // which dismissed the cover, ran `onDismiss`, stopped the session that
        // was still being granted, and left the viewer on the inline page with
        // nothing playing. On tvOS that was the only tune path in the overlay,
        // so `activate → tune` never worked at all. Only a session that has
        // actually finished closes the surface.
        .onChange(of: live.busy) { _, busy in
            if !busy && !live.playing { fullscreen = false }
        }
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

    /// A future programme gets details and no actions. There is no DVR behind
    /// this, so offering "record" would be offering something that does not
    /// exist.
    private func programmeDetail(_ programme: LiveTvProgramme) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(programme.title).font(.title3.weight(.semibold))
            Text("\(detailChannel?.title ?? "") · \(liveTvTime(programme.start))–\(liveTvTime(programme.end))"
                 + (programme.episode.map { " · \($0)" } ?? ""))
                .font(.caption).foregroundStyle(Palette.muted)
            if let episodeTitle = programme.episodeTitle {
                Text(episodeTitle).font(.subheadline.weight(.medium))
            }
            if let synopsis = programme.synopsis { Text(synopsis).font(.callout) }
            if let filters = programme.filters, !filters.isEmpty {
                Text(filters.joined(separator: " · ")).font(.caption2).foregroundStyle(Palette.muted)
            }
            Text("Live only — plurx does not record.")
                .font(.caption2).foregroundStyle(Palette.muted)
            Spacer()
        }
        .padding()
        .background(Palette.bg)
    }
}
