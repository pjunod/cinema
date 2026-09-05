import AVKit
import SwiftUI

/// Kept for the app lifetime: leaving and reopening a tab cannot forget an
/// unconfirmed cleanup or a lost start response's expiry barrier.
@MainActor
final class LiveTvPlayerController: ObservableObject {
    static let shared = LiveTvPlayerController()
    @Published private(set) var channels: [LiveTvChannel] = []
    @Published private(set) var message = "Choose a channel to watch live."
    @Published private(set) var title: String?
    @Published private(set) var busy = false
    @Published private(set) var playing = false
    @Published private(set) var paused = false
    let player = AVPlayer()
    private var api: LiveTvAPI?
    private var lease: LiveTvLease?
    private var profileOrigin: String?
    private var profileToken: String?
    private var serial = 0
    private var loadId = UUID()
    private var heartbeat: Task<Void, Never>?

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
            message = lineup.freshness == "stale"
                ? "Cached lineup (\(lineup.ageSeconds)s old). Starting a channel requires a fresh tuner check."
                : "\(channels.count) channels. DRM-protected channels cannot be played."
        } catch {
            if loadId == loading { message = error.localizedDescription }
        }
    }

    func watch(_ channel: LiveTvChannel) async {
        guard channel.watchable, let lease, let api, !busy else { return }
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
        player.pause()
        player.replaceCurrentItem(with: nil)
        title = nil
        playing = false
        paused = false
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

    private var visibleChannels: [LiveTvChannel] {
        live.channels.filter { query.isEmpty || $0.title.localizedCaseInsensitiveContains(query) }
    }

    var body: some View {
        GeometryReader { geometry in
          VStack(spacing: 12) {
            if live.playing {
                VStack {
                    Text("LIVE · \(live.title ?? "Television")").font(.headline)
                    PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                                  pgsOverlay: nil, allowsPictureInPicture: false)
                        .frame(maxWidth: .infinity)
                        .frame(height: min(geometry.size.width * 9 / 16, geometry.size.height * 0.4))
                    HStack {
                        Button(live.paused ? "Play live" : "Pause") { live.togglePause() }
                        Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                        Button("Fullscreen") { fullscreen = true }
                        Button("Stop") { Task { await live.stop() } }
                    }
                }
            }
            Text(live.message).font(.callout).foregroundStyle(Palette.muted)
                .accessibilityIdentifier("live-tv-status")
            HStack {
                Button("Refresh channels") { Task { await live.load(origin: model.origin, token: Session.shared.token) } }
                Button("Stop / retry cleanup") { Task { await live.stop() } }
            }
            List(visibleChannels) { channel in
                Button {
                    Task { await live.watch(channel) }
                } label: {
                    HStack {
                        VStack(alignment: .leading) {
                            Text(channel.title)
                            Text(channel.watchable ? (channel.favorite ? "Favorite · Watch live" : "Watch live") : "Protected · DRM unsupported")
                                .font(.caption).foregroundStyle(Palette.muted)
                        }
                        Spacer()
                        Image(systemName: channel.watchable ? "play.circle" : "lock")
                    }
                }
                .disabled(!channel.watchable || live.busy)
            }
            .searchable(text: $query, prompt: "Channel number or name")
        }
        .padding()
        }
        .navigationTitle("Live TV")
        .background(Palette.bg)
        .task { await live.load(origin: model.origin, token: Session.shared.token) }
        .onDisappear { if !fullscreen { Task { await live.stop() } } }
        .fullScreenCover(isPresented: $fullscreen, onDismiss: { Task { await live.stop() } }) {
            VStack {
                Text("LIVE · \(live.title ?? "Television")").font(.headline)
                PlayerSurface(player: live.player, pictureInPicture: pictureInPicture,
                              pgsOverlay: nil, allowsPictureInPicture: false)
                Text(live.message).font(.caption)
                HStack {
                    Button(live.paused ? "Play live" : "Pause") { live.togglePause() }
                    Button(muted ? "Unmute" : "Mute") { muted.toggle(); live.player.isMuted = muted }
                    Button("Stop and close") { fullscreen = false }
                }
            }
            .padding()
            .background(.black)
            .onChange(of: live.playing) { _, playing in if !playing { fullscreen = false } }
        }
        .onChange(of: scenePhase) { _, phase in
            if phase != .active { Task { await live.stop() } }
        }
    }
}
