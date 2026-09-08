import AVKit
import SwiftUI

/// Kept for the app lifetime: leaving and reopening a tab cannot forget an
/// unconfirmed cleanup or a lost start response's expiry barrier.
@MainActor
final class LiveTvPlayerController: ObservableObject {
    static let shared = LiveTvPlayerController()

    /// A controller wired to stub requests, for tests that need to count what
    /// actually reached the server rather than assert a constant.
    static func testing(requests: LiveTvRequests) -> LiveTvPlayerController {
        let controller = LiveTvPlayerController()
        controller.lease = LiveTvLease(requests: requests)
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
            channels = lineup.channels
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
                if let fetched = try? await api.guide(
                    hours: LiveTvGridGeometry.requestedHours) {
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
        try await lease?.stop()
    }

    private func detach() {
        heartbeat?.cancel()
        heartbeat = nil
        channelChange?.cancel()
        channelChange = nil
        watching = nil
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
    #else
    static let pxPerSlot: Double = 240
    #endif
    static let rowHeight: Double = 56
    static let channelColumnWidth: Double = 128
    static let visibleSlots = 8
    /// The hours the client must ask for to fill `visibleSlots`, plus the
    /// hour of backfill the server prepends and one for the partial slot.
    static var requestedHours: Int { (visibleSlots * slotSeconds) / 3600 + 2 }

    static func window(now: Int) -> LiveTvGuideWindow {
        let start = now - now % slotSeconds
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

/// One channel row: chip, number and callsign, what is on with a bar to its
/// end, and what is next. A protected channel is dimmed, never hidden.
struct LiveTvChannelRow: View {
    let channel: LiveTvChannel
    let airing: LiveTvAiring
    let selected: Bool

    var body: some View {
        HStack(alignment: .top, spacing: 10) {
            Text(String(channel.guideName.prefix(5)))
                .font(.caption2.weight(.semibold))
                .frame(width: 52, height: 30)
                .background(Palette.surfaceHi)
                .clipShape(RoundedRectangle(cornerRadius: 4))
            VStack(alignment: .leading, spacing: 3) {
                Text(channel.title).font(.subheadline.weight(.semibold))
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

/// The half-hour grid. One horizontal offset shared by every row, so the
/// channel column and the times cannot drift apart from the cells.
struct LiveTvGuideGrid: View {
    let layout: LiveTvGridLayout
    let slots: [Int]
    let playingChannelId: String?
    let onAiring: (LiveTvChannel) -> Void
    let onFuture: (LiveTvChannel, LiveTvProgramme) -> Void

    var body: some View {
        ScrollView([.horizontal, .vertical]) {
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 0) {
                    Color.clear.frame(width: LiveTvGridMetrics.channelColumnWidth)
                    ForEach(slots, id: \.self) { at in
                        Text(liveTvTime(at))
                            .font(.caption2).foregroundStyle(Palette.muted)
                            .frame(width: LiveTvGridMetrics.pxPerSlot, alignment: .leading)
                    }
                }
                .padding(.bottom, 4)
                ForEach(layout.rows, id: \.channel.id) { row in
                    HStack(spacing: 0) {
                        VStack(alignment: .leading, spacing: 1) {
                            Text(row.channel.guideNumber).font(.caption.weight(.semibold))
                            Text(row.channel.guideName).font(.caption2).foregroundStyle(Palette.muted)
                        }
                        .frame(width: LiveTvGridMetrics.channelColumnWidth, alignment: .leading)
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
                                .buttonStyle(.plain)
                                .disabled(!row.channel.watchable && cell.airing)
                                .offset(x: cell.left, y: 4)
                                .accessibilityLabel(
                                    "\(row.channel.title), \(cell.programme.title), "
                                    + "\(liveTvTime(cell.programme.start)) to \(liveTvTime(cell.programme.end))")
                            }
                        }
                    }
                    .frame(height: LiveTvGridMetrics.rowHeight)
                }
            }
            .padding(.horizontal, 8)
            .overlay(alignment: .topLeading) {
                if let nowX = layout.nowX {
                    Rectangle().fill(.red).frame(width: 2)
                        .offset(x: LiveTvGridMetrics.channelColumnWidth + nowX + 8)
                        .accessibilityHidden(true)
                }
            }
        }
    }
}

struct LiveTvView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.scenePhase) private var scenePhase
    @ObservedObject private var live = LiveTvPlayerController.shared
    @StateObject private var pictureInPicture = PictureInPictureController()
    @State private var query = ""
    @State private var fullscreen = false
    @State private var muted = false
    @State private var browse = LiveTvBrowseView.persisted()
    @State private var favoritesOnly = false
    @State private var hideProtected = false
    @State private var detail: LiveTvProgramme?
    @State private var detailChannel: LiveTvChannel?
    @State private var overlayVisible = true
    @State private var overlayGeneration = 0
    @State private var onScreen = false
    #if os(iOS)
    @Environment(\.verticalSizeClass) private var verticalSizeClass
    #endif
    #if os(tvOS)
    /// The one focusable thing on the ten-foot surface while the overlay is
    /// hidden. Its only job is to exist, so the remote has somewhere to send
    /// a press that the routing table can then decide.
    private enum FocusTarget: Hashable { case reveal }
    @FocusState private var focusedControl: FocusTarget?
    #endif
    @State private var now = Int(Date().timeIntervalSince1970)

    private let tick = Timer.publish(every: 30, on: .main, in: .common).autoconnect()

    private var visibleChannels: [LiveTvChannel] {
        LiveTvGuideReducer.filter(
            channels: live.channels,
            guide: live.guide,
            options: LiveTvChannelFilter(query: query, favoritesOnly: favoritesOnly,
                                         hideProtected: hideProtected),
            now: now)
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
            if live.playing && !fullscreen {
                VStack(spacing: 8) {
                    PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                                  pgsOverlay: nil, allowsPictureInPicture: true)
                        .frame(maxWidth: .infinity)
                        .frame(height: min(geometry.size.width * 9 / 16, geometry.size.height * 0.4))
                    nowBar
                }
            }
            Text(live.message).font(.callout).foregroundStyle(Palette.muted)
                .accessibilityIdentifier("live-tv-status")
            HStack {
                Button("Refresh channels") { Task { await live.load(origin: model.origin, token: Session.shared.token) } }
                Button("Stop / retry cleanup") { Task { await live.stop() } }
            }
            filterBar
            browseRegion
        }
        .padding()
        }
        .navigationTitle("Live TV")
        .background(Palette.bg)
        .task { await live.load(origin: model.origin, token: Session.shared.token) }
        .onReceive(tick) { _ in now = Int(Date().timeIntervalSince1970) }
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
        .onAppear {
            onScreen = true
            #if os(tvOS)
            // The ten-foot surface is always fullscreen. There is no inline
            // player on a television; the milestone said so and the code shipped
            // the phone layout with a "Fullscreen" button in front of it.
            if live.playing { fullscreen = true }
            #endif
        }
        #if os(tvOS)
        .onChange(of: live.playing) { _, playing in if playing { fullscreen = true } }
        #endif
        #if os(iOS)
        // Rotating to landscape is the phone's fullscreen gesture. Compact
        // height is the honest test — it covers every phone in landscape and
        // no iPad in a split view.
        .onChange(of: verticalSizeClass) { _, height in
            if live.playing && height == .compact { fullscreen = true }
            else if height == .regular { fullscreen = false }
        }
        #endif
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
            HStack {
                // `ButtonToggleStyle` has no tvOS availability, and this
                // file is compiled for both targets — the plain switch style
                // is correct on the ten-foot surface anyway.
                #if os(iOS)
                Toggle("Favorites", isOn: $favoritesOnly).toggleStyle(.button)
                Toggle("Hide protected", isOn: $hideProtected).toggleStyle(.button)
                #else
                Toggle("Favorites", isOn: $favoritesOnly)
                Toggle("Hide protected", isOn: $hideProtected)
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
        // A focus-navigable half-hour grid is a milestone of its own on each
        // ten-foot platform; until then the television gets the list, which the
        // focus engine already handles, and the per-channel schedule inside the
        // overlay. Plan §5 non-goal 7.
        #if os(tvOS)
        let browse = LiveTvBrowseView.list
        #endif
        switch browse {
        case .list:
            List(visibleChannels) { channel in
                Button {
                    Task { await live.watch(channel) }
                } label: {
                    LiveTvChannelRow(channel: channel, airing: live.airing(channel, now: now),
                                     selected: live.watching?.id == channel.id)
                }
                .disabled(!channel.watchable || live.busy)
            }
            .searchable(text: $query, prompt: "Number, name, or what is on")
        case .guide:
            LiveTvGuideGrid(
                layout: LiveTvGuideReducer.gridLayout(
                    guide: live.guide, channels: visibleChannels,
                    window: LiveTvGridMetrics.window(now: now), now: now,
                    slotSeconds: LiveTvGridMetrics.slotSeconds,
                    pxPerSlot: LiveTvGridMetrics.pxPerSlot),
                slots: LiveTvGuideReducer.gridSlots(
                    window: LiveTvGridMetrics.window(now: now),
                    slotSeconds: LiveTvGridMetrics.slotSeconds),
                playingChannelId: live.watching?.id,
                onAiring: { channel in Task { await live.watch(channel) } },
                onFuture: { channel, programme in detailChannel = channel; detail = programme })
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
            HStack {
                Button(live.paused ? "Play live" : "Pause") { live.togglePause() }
                Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                if pictureInPicture.isSupported {
                    Button(pictureInPicture.isActive ? "Leave PiP" : "PiP") { pictureInPicture.toggle() }
                }
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
            #endif
            if overlayVisible {
                VStack {
                    HStack(alignment: .top) {
                        VStack(alignment: .leading, spacing: 3) {
                            Text(airing.now?.title ?? live.title ?? "Live television")
                                .font(.title3.weight(.semibold))
                            Text(channel?.title ?? "").font(.caption)
                            if let now = airing.now {
                                Text("\(liveTvTime(now.start))–\(liveTvTime(now.end))"
                                     + (airing.next.map { " · Next: \($0.title)" } ?? ""))
                                    .font(.caption2).opacity(0.85)
                            }
                        }
                        Spacer()
                        HStack {
                            Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                            if pictureInPicture.isSupported {
                                Button("PiP") { pictureInPicture.toggle() }
                            }
                            Button("Exit") { fullscreen = false }
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
        }
        .contentShape(Rectangle())
        #if os(tvOS)
        .liveTvRemoteAdapter(state: { overlayVisible ? .overlay : .hidden },
                             apply: { outcome, _ in applyLiveOutcome(outcome) })
        #else
        .onTapGesture {
            // The touch surface's whole contract: a tap toggles the chrome.
            overlayVisible.toggle()
            overlayGeneration &+= 1
        }
        #endif
        .task(id: overlayGeneration) {
            guard overlayVisible, live.playing, !live.paused else { return }
            try? await Task.sleep(nanoseconds: LiveTvInputRouting.overlayAutoHideNanoseconds)
            guard !Task.isCancelled, live.playing, !live.paused else { return }
            overlayVisible = false
        }
        #if os(tvOS)
        // Focus must land on the reveal layer whenever the overlay is not
        // there to hold it, or the next press goes nowhere.
        .onAppear { focusedControl = .reveal }
        .onChange(of: overlayVisible) { _, visible in
            if !visible { focusedControl = .reveal }
        }
        #endif
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
        case .exit:
            fullscreen = false
            return true
        case .activate, .focusRow:
            // The channel list inside the overlay is preview-then-commit, and
            // the focus engine owns both halves: moving focus is `focus_row`
            // and the button's own action is `activate`.
            overlayGeneration &+= 1
            return true
        case .channelUp, .channelDown, .stripPrev, .stripNext, .tune, .ignore:
            // Not reachable on the ten-foot surface; the table says so and the
            // switch stays exhaustive so a future row cannot land silently.
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
