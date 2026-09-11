import AVKit
import Foundation
import SwiftUI

extension Notification.Name {
    static let makeLibraryChannelFromItem = Notification.Name("plurx.makeLibraryChannelFromItem")
}

struct LibraryChannelCreationSeed {
    let itemId: Int
    let kind: String
    let title: String
}

// MARK: - Wire contract

enum LibraryChannelVisibility: String, Codable, CaseIterable, Identifiable {
    case personal
    case shared

    var id: String { rawValue }
    var label: String { self == .shared ? "Shared" : "Personal" }
}

enum LibraryChannelOrdering: String, Codable, CaseIterable, Identifiable {
    case balancedShuffle = "balanced_shuffle"
    case releaseOrder = "release_order"

    var id: String { rawValue }
    var label: String { self == .balancedShuffle ? "Balanced shuffle" : "Release order" }
}

struct LibraryChannelRecipe: Codable, Equatable {
    var version = 1
    var libraryIds: [Int] = []
    var kinds: [String] = ["movie", "episode"]
    var genresAny: [String] = []
    var tagsAny: [String] = []
    var keywordsAny: [String] = []
    var yearMin: Int?
    var yearMax: Int?
    var includeItemIds: [Int] = []
    var includeShowIds: [Int] = []
    var excludeItemIds: [Int] = []
    var excludeShowIds: [Int] = []
    var ordering: LibraryChannelOrdering = .balancedShuffle
    var includeSpecials = false
    var autoRefresh = true
    var matchAllInScope = false
}

struct LibraryChannelOccurrence: Codable, Equatable {
    let cycle: Int64
    let ordinal: UInt32
}

struct LibraryChannelProgramme: Codable, Identifiable, Equatable {
    let source: String
    let channelId: String
    let generationId: String
    let cycle: Int64
    let ordinal: UInt32
    let startsAtMs: Int64
    let endsAtMs: Int64
    let itemId: Int
    let fileId: Int
    let title: String

    var id: String { "\(channelId):\(generationId):\(cycle):\(ordinal)" }
}

struct LibraryChannel: Codable, Identifiable, Equatable {
    let id: String
    let ownerUserId: Int
    var name: String
    var description: String
    var visibility: LibraryChannelVisibility
    var enabled: Bool
    var revision: Int64
    var recipe: LibraryChannelRecipe
    var seed: [UInt8]
    var activeGenerationId: String?
    var activeEpochMs: Int64?
    var pendingGenerationId: String?
    var pendingEpochMs: Int64?
    var favourite: Bool
    var createdAtMs: Int64
    var updatedAtMs: Int64
    var source: String?
    var canEdit: Bool?
    var canDelete: Bool?
    var canShare: Bool?
    var now: LibraryChannelProgramme?
    var next: LibraryChannelProgramme?
}

struct LibraryChannelDefinition: Codable {
    let requestId: String
    let name: String
    let description: String
    let visibility: LibraryChannelVisibility
    let enabled: Bool
    let recipe: LibraryChannelRecipe
    let previewSeed: String?
}

struct LibraryChannelUpdateRequest: Codable {
    let expectedRevision: Int64
    let requestId: String
    let name: String
    let description: String
    let visibility: LibraryChannelVisibility
    let enabled: Bool
    let recipe: LibraryChannelRecipe
    let previewSeed: String?
}

struct LibraryChannelMutation: Codable {
    let channel: LibraryChannel
    let buildState: String
}

struct LibraryChannelPreviewRequest: Codable {
    let recipe: LibraryChannelRecipe
    let limit: Int
    let previewSeed: String?
}

struct LibraryChannelPreview: Codable {
    struct Match: Codable, Identifiable {
        let candidate: Candidate
        let reasons: [String]
        var id: Int { candidate.itemId }
    }

    struct Candidate: Codable {
        let itemId: Int
        let fileId: Int
        let title: String
        let durationMs: Int64
    }

    struct Entry: Codable, Identifiable {
        let ordinal: UInt32
        let itemId: Int
        let fileId: Int
        let durationMs: Int64
        var id: UInt32 { ordinal }
    }

    let eligibleCount: Int
    let uniqueDurationMs: Int64
    let repeatDescription: String
    let contentDigest: String
    let matches: [Match]
    let firstTen: [Entry]
    let previewSeed: String
    let excludedCount: Int
}

struct LibraryChannelFavouriteRequest: Codable { let favourite: Bool }

struct LibraryChannelRebuildRequest: Codable {
    let expectedRevision: Int64
    let requestId: String
    let activation: String
    let reshuffle: Bool
}

struct LibraryChannelBuild: Codable {
    let state: String
    let activeGenerationId: String?
    let pendingGenerationId: String?
    let pendingActivationMs: Int64?
}

struct LibraryChannelCapabilities: Codable, Equatable {
    let joinSchedule: Bool
    let watchFromStart: Bool
    let seekWhileFollowing: Bool
    let record: Bool
}

struct LibraryChannelResolved: Codable, Equatable {
    let source: String
    let channelId: String
    let definitionRevision: Int64
    let generationId: String
    let occurrence: LibraryChannelOccurrence
    let serverNowMs: Int64
    let startsAtMs: Int64
    let endsAtMs: Int64
    let itemId: Int
    let fileId: Int
    let positionMs: Int64
    let capabilities: LibraryChannelCapabilities
}

struct LibraryChannelSessionRequest: Codable {
    let generationId: String
    let occurrence: LibraryChannelOccurrence
    let tuneSequence: UInt64
    let playback: CreateSessionRequest
}

struct LibraryChannelPlaybackPurpose: Codable {
    let channelId: String
    let generationId: String
    let cycle: Int64
    let ordinal: UInt32
    let tuneSequence: UInt64
    let startsAtMs: Int64
    let endsAtMs: Int64
}

struct LibraryChannelSession: Codable {
    let playback: HlsStart
    let libraryChannel: LibraryChannelPlaybackPurpose
}

struct DeveloperReadiness: Codable {
    struct Item: Codable, Identifiable {
        let id: String
        let title: String
        let enabled: Bool?
        let setting: String?
        let requirements: [Requirement]
    }

    struct Requirement: Codable, Identifiable {
        let id: String
        let title: String
        let status: String
        let evidence: String
    }

    let items: [Item]
}

// MARK: - Following player

@MainActor
final class LibraryChannelPlayerController: ObservableObject {
    static let shared = LibraryChannelPlayerController()

    @Published private(set) var channels: [LibraryChannel] = []
    @Published private(set) var programmes: [LibraryChannelProgramme] = []
    @Published private(set) var watching: LibraryChannel?
    @Published private(set) var resolved: LibraryChannelResolved?
    @Published private(set) var title: String?
    @Published private(set) var message = "Choose a channel to join its schedule."
    @Published private(set) var busy = false
    @Published private(set) var paused = false
    @Published private(set) var playbackError: String?

    let player = AVPlayer()
    private var api: PlurxAPI?
    private weak var model: AppModel?
    private var profileOrigin: String?
    private var tuneSequence: UInt64 = 0
    private var playbackId = UUID().uuidString
    private var sessionId: String?
    private var boundary: Task<Void, Never>?
    private var clockRefresh: Task<Void, Never>?
    private var serverBaseMs: Int64 = 0
    private var monotonicBaseMs: Int64 = 0
    private var endObserver: NSObjectProtocol?
    private var failedObserver: NSObjectProtocol?
    private var itemStatusObservation: NSKeyValueObservation?
    private var progressObserver: Any?
    private let playbackControl = PlaybackControlSession()
    private var mediaOriginMs: Int64 = 0
    private var mediaDurationMs: Int = 0

    deinit {
        if let progressObserver { player.removeTimeObserver(progressObserver) }
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        if let failedObserver { NotificationCenter.default.removeObserver(failedObserver) }
    }

    func load(model: AppModel) async {
        self.model = model
        if profileOrigin != model.origin || api == nil {
            await stop()
            api = model.requireAPI()
            profileOrigin = model.origin
            playbackId = UUID().uuidString
        }
        await refresh()
    }

    func refresh() async {
        guard let api else { return }
        do {
            let loaded = try await api.libraryChannels()
            channels = loaded.sorted {
                if $0.favourite != $1.favourite { return $0.favourite }
                return $0.name.localizedStandardCompare($1.name) == .orderedAscending
            }
            let ids = channels.map(\.id)
            if !ids.isEmpty {
                let now = Int64(Date().timeIntervalSince1970 * 1_000)
                programmes = try await api.libraryChannelGuide(ids: ids, from: now, to: now + 4 * 60 * 60 * 1_000)
            } else {
                programmes = []
            }
            message = channels.isEmpty
                ? "No Library channels yet. An administrator can make the first one."
                : "\(channels.count) scheduled channels · guide times come from the server"
        } catch is CancellationError {
        } catch {
            message = error.localizedDescription
        }
    }

    func tune(_ channel: LibraryChannel) async {
        guard channel.enabled, let api, let model else { return }
        tuneSequence &+= 1
        let expected = tuneSequence
        busy = true
        playbackError = nil
        message = "Joining \(channel.name)…"
        do {
            let resolveStarted = monotonicMs()
            let occurrence = try await api.resolveLibraryChannel(channel.id)
            recordServerClock(occurrence.serverNowMs, requestStartedMs: resolveStarted)
            guard expected == tuneSequence else { return }
            let plan = try await model.playbackDecision(fileId: occurrence.fileId)
            guard expected == tuneSequence, !Task.isCancelled else { return }
            let request = Self.playbackRequest(
                decision: plan.decision,
                caps: plan.caps,
                playbackId: playbackId,
                positionMs: occurrence.positionMs
            )
            let started = try await api.createLibraryChannelSession(
                channelId: channel.id,
                resolved: occurrence,
                tuneSequence: expected,
                playback: request
            )
            guard expected == tuneSequence,
                  started.libraryChannel.tuneSequence == expected
            else {
                await model.endHlsSession(started.playback.sessionId)
                return
            }
            let oldSession = sessionId
            sessionId = started.playback.sessionId
            if let oldSession { await model.endHlsSession(oldSession) }
            resolved = occurrence
            watching = channel
            title = programmes.first(where: {
                $0.channelId == channel.id && $0.generationId == occurrence.generationId
                    && $0.cycle == occurrence.occurrence.cycle && $0.ordinal == occurrence.occurrence.ordinal
            })?.title ?? channel.name
            paused = false
            guard let playlistURL = Session.shared.url(started.playback.playlistUrl) else {
                throw APIError.badURL
            }
            let item = AVPlayerItem(url: playlistURL)
            item.preferredForwardBufferDuration = 60
            player.replaceCurrentItem(with: item)
            observeEnd(item, channel: channel, sequence: expected)
            observeFailure(item, sequence: expected)
            observeProgress(item, sequence: expected)
            player.play()
            mediaOriginMs = Int64(started.playback.mediaOriginMs ?? Int(occurrence.positionMs))
            mediaDurationMs = started.playback.durationMs ?? 0
            if let bootstrap = started.playback.control, bootstrap.isValid {
                playbackControl.begin(
                    bootstrap: bootstrap,
                    transport: PlaybackControlTransport(
                        origin: model.origin,
                        authorize: { request in Session.shared.authorize(&request) }
                    ),
                    observe: { [weak self] in self?.controlObservation() }
                )
            } else {
                playbackControl.end()
            }
            message = "Following live · seeking and watch history are off"
            scheduleBoundary(channel: channel, occurrence: occurrence, sequence: expected)
            scheduleClockRefresh(channel: channel, sequence: expected)
            Task { @MainActor [weak self] in
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                guard let self, self.tuneSequence == expected else { return }
                let origin = Int64(started.playback.mediaOriginMs ?? Int(occurrence.positionMs))
                let scheduled = max(0, self.serverNowMs() - occurrence.startsAtMs)
                let currentSeconds = self.player.currentTime().seconds
                guard self.player.currentItem === item, item.status == .readyToPlay,
                      currentSeconds.isFinite else { return }
                let playerPosition = origin + Int64(currentSeconds * 1_000)
                if scheduled - playerPosition > 2_000 {
                    await self.player.seek(to: CMTime(seconds: Double(max(0, scheduled - origin)) / 1_000, preferredTimescale: 1_000))
                }
            }
        } catch is CancellationError {
        } catch {
            if expected == tuneSequence { message = error.localizedDescription }
        }
        if expected == tuneSequence { busy = false }
    }

    /// Channels retain their scheduled-session endpoint, but execute the same
    /// per-file decision and capability snapshot as ordinary library playback.
    nonisolated static func playbackRequest(
        decision: Decision, caps: DeviceCaps, playbackId: String, positionMs: Int64
    ) -> CreateSessionRequest {
        let mode = PlayerController.playbackMode(decision)
        let copy = mode == "direct" || mode == "remux"
        let audio = decision.delivery?.audio ?? decision.selection?.audioIndex
            ?? decision.audio?.first(where: { $0.default })?.index
        return CreateSessionRequest(
            playbackId: playbackId,
            start: Double(positionMs) / 1_000,
            audio: audio,
            nativeSubtitles: true,
            copy: copy ? true : nil,
            aac: copy ? PlayerController.needsAAC(audioIndex: audio, decision: decision) : nil,
            preserveDolbyVision: copy ? PlayerController.shouldPreserveDolbyVision(decision) : nil,
            hdr10: PlayerController.sessionHDR10Request(
                copy: copy, deliveredRange: decision.deliveredDynamicRange, forcesSDR: false
            ),
            caps: caps
        )
    }

    func togglePause() async {
        guard let channel = watching else { return }
        if paused {
            if let resolved,
               serverNowMs() >= resolved.endsAtMs {
                await tune(channel)
            } else {
                paused = false
                player.play()
                message = "Following live · seeking and watch history are off"
            }
        } else {
            paused = true
            player.pause()
            message = "Paused. Resume rejoins server-now if the programme changes."
        }
        playbackControl.playerChanged()
    }

    func stop() async {
        tuneSequence &+= 1
        boundary?.cancel()
        boundary = nil
        clockRefresh?.cancel()
        clockRefresh = nil
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = nil
        if let failedObserver { NotificationCenter.default.removeObserver(failedObserver) }
        failedObserver = nil
        itemStatusObservation = nil
        if let progressObserver { player.removeTimeObserver(progressObserver) }
        progressObserver = nil
        playbackError = nil
        player.pause()
        playbackControl.end()
        player.replaceCurrentItem(with: nil)
        if let sessionId, let model { await model.endHlsSession(sessionId) }
        sessionId = nil
        watching = nil
        resolved = nil
        title = nil
        paused = false
        busy = false
        mediaOriginMs = 0
        mediaDurationMs = 0
    }

    func setFavourite(_ channel: LibraryChannel) async {
        guard let api else { return }
        do {
            try await api.setLibraryChannelFavourite(channel.id, favourite: !channel.favourite)
            await refresh()
        } catch { message = error.localizedDescription }
    }

    private func scheduleBoundary(channel: LibraryChannel, occurrence: LibraryChannelResolved, sequence: UInt64) {
        boundary?.cancel()
        let delayMs = max(0, occurrence.endsAtMs - serverNowMs())
        boundary = Task { @MainActor [weak self] in
            do { try await Task.sleep(nanoseconds: UInt64(delayMs) * 1_000_000) } catch { return }
            guard let self, self.tuneSequence == sequence else { return }
            if self.paused {
                self.message = "The programme changed while paused. Resume to rejoin live."
            } else {
                await self.tune(channel)
            }
        }
    }

    private func monotonicMs() -> Int64 {
        Int64(ProcessInfo.processInfo.systemUptime * 1_000)
    }

    private func recordServerClock(_ serverNowMs: Int64, requestStartedMs: Int64) {
        let received = monotonicMs()
        serverBaseMs = serverNowMs + max(0, received - requestStartedMs) / 2
        monotonicBaseMs = received
    }

    private func serverNowMs() -> Int64 {
        guard serverBaseMs != 0 else { return Int64(Date().timeIntervalSince1970 * 1_000) }
        return serverBaseMs + max(0, monotonicMs() - monotonicBaseMs)
    }

    private func scheduleClockRefresh(channel: LibraryChannel, sequence: UInt64) {
        clockRefresh?.cancel()
        guard let api else { return }
        clockRefresh = Task { @MainActor [weak self] in
            while let self, self.tuneSequence == sequence {
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { return }
                guard self.tuneSequence == sequence, !self.paused else { continue }
                let started = self.monotonicMs()
                guard let fresh = try? await api.resolveLibraryChannel(channel.id) else { continue }
                self.recordServerClock(fresh.serverNowMs, requestStartedMs: started)
                if self.resolved?.generationId != fresh.generationId || self.resolved?.occurrence != fresh.occurrence {
                    await self.tune(channel)
                    return
                }
                self.resolved = fresh
                self.scheduleBoundary(channel: channel, occurrence: fresh, sequence: sequence)
            }
        }
    }

    private func observeEnd(_ item: AVPlayerItem, channel: LibraryChannel, sequence: UInt64) {
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.tuneSequence == sequence, !self.paused else { return }
                self.playbackControl.playerChanged()
                if let resolved = self.resolved, resolved.endsAtMs > self.serverNowMs() {
                    self.message = "This programme ended early. Waiting for its scheduled boundary."
                } else {
                    await self.tune(channel)
                }
            }
        }
    }

    private func observeProgress(_ item: AVPlayerItem, sequence: UInt64) {
        if let progressObserver { player.removeTimeObserver(progressObserver) }
        let capture = makeProgressObservation(item, sequence: sequence) { [weak self] in
            self?.playbackControl.playerChanged()
        }
        progressObserver = player.addPeriodicTimeObserver(
            forInterval: CMTime(seconds: 1, preferredTimescale: 2), queue: .main
        ) { _ in
            MainActor.assumeIsolated { capture() }
        }
    }

    /// The reporter reads an immutable snapshot; playback must keep publishing
    /// progress or the server keeps budgeting production from the join point.
    func makeProgressObservation(
        _ item: AVPlayerItem, sequence: UInt64, notify: @escaping @MainActor () -> Void
    ) -> @MainActor () -> Void {
        { [weak self] in
            guard let self, self.tuneSequence == sequence, self.player.currentItem === item else { return }
            notify()
        }
    }

    func observeFailure(_ item: AVPlayerItem, sequence: UInt64) {
        itemStatusObservation = item.observe(\.status, options: [.initial, .new]) { [weak self] item, _ in
            guard item.status == .failed else { return }
            Task { @MainActor in self?.handleItemFailure(item, sequence: sequence) }
        }
        if let failedObserver { NotificationCenter.default.removeObserver(failedObserver) }
        failedObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemFailedToPlayToEndTime, object: item, queue: .main
        ) { [weak self] notification in
            let error = notification.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? NSError
            Task { @MainActor in self?.handleItemFailure(item, sequence: sequence, notificationError: error) }
        }
    }

    func handleItemFailure(_ item: AVPlayerItem, sequence: UInt64, notificationError: NSError? = nil) {
        guard sequence == tuneSequence, player.currentItem === item else { return }
        let error = notificationError ?? (item.error as NSError?)
        let event = item.errorLog()?.events.last
        if playbackError == nil || error != nil || event != nil {
            playbackError = Self.playbackFailureDescription(error, eventDomain: event?.errorDomain,
                                                            eventStatus: event?.errorStatusCode)
        }
        busy = false
        boundary?.cancel()
        clockRefresh?.cancel()
        playbackControl.playerChanged()
    }

    nonisolated static func playbackFailureDescription(
        _ error: Error?, eventDomain: String? = nil, eventStatus: Int? = nil
    ) -> String {
        guard let error = error as NSError? else {
            if let eventDomain, let eventStatus {
                return "The channel stream could not be played. (\(eventDomain) \(eventStatus))"
            }
            return "The channel stream could not be played. Try Watch live again."
        }
        // Keep capability URLs and userInfo out of the UI while retaining the
        // useful decoder/transport codes hidden by Apple's generic message.
        var codes = "\(error.domain) \(error.code)"
        if let underlying = error.userInfo[NSUnderlyingErrorKey] as? NSError {
            codes += "; \(underlying.domain) \(underlying.code)"
        }
        return "\(error.localizedDescription) (\(codes))"
    }

    func controlObservation() -> PlayerControlObservation? {
        guard let item = player.currentItem else { return nil }
        let local = player.currentTime().seconds
        let position = mediaOriginMs + Int64((local.isFinite ? max(0, local) : 0) * 1_000)
        return PlayerControlObservation(
            positionMs: Int(min(Int64(Int.max), max(0, position))),
            durationMs: mediaDurationMs,
            bufferedFromMs: nil,
            bufferedThroughMs: nil,
            rate: Double(player.rate),
            // A decoder waiting for bytes has zero rate too. Reporting that
            // as Hold stops the producer whose next segment would unblock it.
            isPaused: paused,
            isEnded: item.status == .failed || (resolved?.endsAtMs ?? Int64.max) <= serverNowMs(),
            isSeeking: false,
            hasStarted: player.timeControlStatus == .playing,
            waitingForMs: nil,
            isLikelyToKeepUp: item.isPlaybackLikelyToKeepUp,
            errorCode: item.error == nil ? nil : .media,
            errorDetail: item.error == nil ? nil : "library_channel_media_failed",
            droppedFrames: nil,
            observedDownloadBps: nil,
            selection: ClientSelection(
                quality: .auto,
                audioTrack: nil,
                subtitle: SubtitleSelection(mode: .off, track: nil),
                audioOffsetMs: 0,
                codec: .auto,
                dynamicRange: .auto
            ),
            capabilities: Caps.controlCapabilities()
        )
    }
}

// MARK: - Browse, guide, and authoring

struct LibraryChannelsView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var controller = LibraryChannelPlayerController.shared
    @State private var editing: LibraryChannel?
    @State private var creating = false
    @State private var creationSeed: LibraryChannelCreationSeed?
    @State private var personalPlayback: PlayContext?
    @State private var returnChannel: LibraryChannel?
    @State private var focusedProgrammeId: String?
    @AppStorage("libraryChannelTVLayout") private var tvLayout = "guide_preview"

    var body: some View {
        GeometryReader { proxy in
            let wide = proxy.size.width >= 900
            Group {
                #if os(tvOS)
                switch tvLayout {
                case "guide_over_picture":
                    ZStack(alignment: .leading) {
                        playerPane
                        channelList.frame(maxWidth: proxy.size.width * 0.42)
                            .padding().background(Palette.surface.opacity(0.92))
                    }
                case "channel_browser":
                    HStack(spacing: 24) {
                        channelList
                        playerPane.frame(width: proxy.size.width * 0.38)
                    }
                default:
                    HStack(spacing: 24) {
                        playerPane.frame(width: proxy.size.width * 0.58)
                        channelList
                    }
                }
                #else
                if wide {
                    HStack(spacing: 24) {
                        playerPane.frame(width: proxy.size.width * 0.58)
                        channelList
                    }
                } else {
                    VStack(spacing: 16) {
                        playerPane.frame(height: min(360, proxy.size.height * 0.42))
                        channelList
                    }
                }
                #endif
            }
            .padding()
        }
        #if os(tvOS)
        .buttonStyle(TVReadableButtonStyle(prominent: false))
        #endif
        .navigationTitle("Library channels")
        .toolbar {
            #if os(iOS)
            ToolbarItem(placement: .primaryAction) {
                Button { creating = true } label: { Label("Make a channel", systemImage: "plus") }
            }
            #else
            ToolbarItem(placement: .primaryAction) {
                Button(layoutLabel) {
                    tvLayout = tvLayout == "guide_preview" ? "guide_over_picture"
                        : (tvLayout == "guide_over_picture" ? "channel_browser" : "guide_preview")
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false))
            }
            #endif
        }
        .sheet(isPresented: $creating, onDismiss: { creationSeed = nil }) {
            NavigationStack { LibraryChannelEditor(channel: nil, seed: creationSeed) { Task { await controller.refresh() } } }
        }
        .sheet(item: $editing) { channel in
            NavigationStack { LibraryChannelEditor(channel: channel, seed: nil) { Task { await controller.refresh() } } }
        }
        .onReceive(NotificationCenter.default.publisher(for: .makeLibraryChannelFromItem)) { note in
            guard let itemId = note.userInfo?["itemId"] as? Int,
                  let kind = note.userInfo?["kind"] as? String,
                  let title = note.userInfo?["title"] as? String else { return }
            creationSeed = LibraryChannelCreationSeed(itemId: itemId, kind: kind, title: title)
            creating = true
        }
        .fullScreenCover(item: $personalPlayback, onDismiss: {
            if let channel = returnChannel {
                returnChannel = nil
                Task { await controller.tune(channel) }
            }
        }) { context in
            ZStack(alignment: .topTrailing) {
                PlayerView(
                    itemId: context.itemId, fileId: context.fileId, startMs: 0,
                    durationMs: context.durationMs, title: context.title,
                    progressOffsetMs: 0, itemDurationMs: nil,
                    subtitle: nil, year: nil, airDate: nil, overview: context.overview,
                    selection: .none, onPlayNext: { personalPlayback = $0 },
                    onPlaybackStopped: { _ in }
                ).environmentObject(model)
                Button("Return to channel") { personalPlayback = nil }
                    #if os(tvOS)
                    .buttonStyle(TVReadableButtonStyle(prominent: true))
                    #else
                    .buttonStyle(.borderedProminent)
                    #endif
                    .padding()
            }
        }
        .task {
            await controller.load(model: model)
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { return }
                await controller.refresh()
            }
        }
        .onDisappear { Task { await controller.stop() } }
    }

    private var layoutLabel: String {
        switch tvLayout {
        case "guide_over_picture": "Layout · Guide over picture"
        case "channel_browser": "Layout · Channel browser"
        default: "Layout · Guide + preview"
        }
    }

    private var playerPane: some View {
        VStack(alignment: .leading, spacing: 12) {
            ZStack {
                RoundedRectangle(cornerRadius: 18).fill(Color.black)
                if controller.watching != nil {
                    VideoPlayer(player: controller.player).clipShape(RoundedRectangle(cornerRadius: 18))
                } else {
                    ContentUnavailableView("Choose a channel", systemImage: "play.tv", description: Text("Join the programme already in progress."))
                        .foregroundStyle(.white)
                }
            }
            .aspectRatio(16 / 9, contentMode: .fit)
            if let channel = controller.watching, let resolved = controller.resolved {
                Text(channel.name).font(.title2.bold())
                Text(controller.title ?? "On now").font(.headline)
                HStack {
                    Button(controller.paused ? "Resume live" : "Pause") { Task { await controller.togglePause() } }
                    Button("Watch from start") {
                        returnChannel = channel
                        personalPlayback = PlayContext(
                            itemId: resolved.itemId,
                            fileId: resolved.fileId,
                            startMs: 0,
                            durationMs: Int(resolved.endsAtMs - resolved.startsAtMs),
                            title: controller.title ?? channel.name,
                            overview: "Started from \(channel.name)."
                        )
                        Task { await controller.stop() }
                    }
                    Button("Stop") { Task { await controller.stop() } }
                }
            }
            if let error = controller.playbackError {
                Label(error, systemImage: "exclamationmark.triangle.fill")
                    .font(.callout).foregroundStyle(Palette.onBg)
                    .padding().frame(maxWidth: .infinity, alignment: .leading)
                    .background(Palette.surfaceHi, in: RoundedRectangle(cornerRadius: 12))
                    .accessibilityIdentifier("library-channel-playback-error")
            }
            Text(controller.message).font(.caption).foregroundStyle(.secondary)
        }
    }

    private var channelList: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 12) {
                ForEach(controller.channels) { channel in
                    let guide = controller.programmes.filter { $0.channelId == channel.id }
                    VStack(alignment: .leading, spacing: 8) {
                        HStack {
                            VStack(alignment: .leading) {
                                Text(channel.name).font(.headline)
                                Text(channel.description).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                            }
                            Spacer()
                            Button { Task { await controller.setFavourite(channel) } } label: {
                                Image(systemName: channel.favourite ? "star.fill" : "star")
                            }
                        }
                        ScrollView(.horizontal) {
                            HStack(spacing: 6) {
                                ForEach(guide) { programme in
                                    let isPlaying = controller.watching?.id == channel.id
                                        && controller.resolved?.generationId == programme.generationId
                                        && controller.resolved?.occurrence.ordinal == programme.ordinal
                                        && controller.resolved?.occurrence.cycle == programme.cycle
                                    Button {
                                        focusedProgrammeId = programme.id
                                        if programme.startsAtMs <= Int64(Date().timeIntervalSince1970 * 1_000)
                                            && programme.endsAtMs > Int64(Date().timeIntervalSince1970 * 1_000) {
                                            Task { await controller.tune(channel) }
                                        }
                                    } label: {
                                        VStack(alignment: .leading) {
                                            Text(programme.title).lineLimit(1)
                                            Text(Date(timeIntervalSince1970: Double(programme.startsAtMs) / 1_000), style: .time)
                                                .font(.caption.monospacedDigit())
                                            if isPlaying { Label("Watching", systemImage: "play.fill").font(.caption2) }
                                        }
                                        .frame(width: min(360, max(120, CGFloat(programme.endsAtMs - programme.startsAtMs) / 300_000 * 80)), alignment: .leading)
                                    }
                                    #if os(tvOS)
                                    .buttonStyle(TVReadableButtonStyle(prominent: isPlaying))
                                    #else
                                    .buttonStyle(.bordered)
                                    .tint(isPlaying ? Palette.accent : nil)
                                    #endif
                                    .accessibilityValue(focusedProgrammeId == programme.id ? "Focused programme" : "")
                                }
                            }
                        }
                        HStack {
                            Button(channel.enabled ? "Watch live" : "Disabled") { Task { await controller.tune(channel) } }
                                .disabled(!channel.enabled || controller.busy)
                            #if os(iOS)
                            if channel.canEdit == true {
                                Button("Edit") { editing = channel }
                            }
                            #endif
                        }
                    }
                    .padding()
                    .background(Palette.surface, in: RoundedRectangle(cornerRadius: 14))
                }
            }
        }
    }
}

private struct LibraryChannelEditor: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @Environment(\.scenePhase) private var scenePhase
    let channel: LibraryChannel?
    let seed: LibraryChannelCreationSeed?
    let saved: () -> Void

    @State private var name = ""
    @State private var description = ""
    @State private var visibility: LibraryChannelVisibility = .personal
    @State private var enabled = true
    @State private var movies = true
    @State private var episodes = true
    @State private var genres = ""
    @State private var tags = ""
    @State private var keywords = ""
    @State private var yearMin = ""
    @State private var yearMax = ""
    @State private var ordering: LibraryChannelOrdering = .balancedShuffle
    @State private var includeSpecials = false
    @State private var autoRefresh = true
    @State private var matchAllInScope = false
    @State private var libraryIds: [Int] = []
    @State private var includeItemIds: [Int] = []
    @State private var includeShowIds: [Int] = []
    @State private var excludeItemIds: [Int] = []
    @State private var excludeShowIds: [Int] = []
    @State private var searchText = ""
    @State private var searchResults: [Item] = []
    @State private var preview: LibraryChannelPreview?
    @State private var previewSeed: String?
    @State private var busy = false
    @State private var message: String?
    @State private var canShare = false
    @State private var step = 0

    var body: some View {
        Form {
            if step == 0 { Section("1 · Content") {
                HStack {
                    Button("Space docs") { genres = "Documentary"; keywords = "space, astronomy, mars"; matchAllInScope = false }
                    Button("’90s comedy") { genres = "Comedy"; yearMin = "1990"; yearMax = "1999"; matchAllInScope = false }
                    Button("Film noir") { genres = "Film Noir, Crime"; keywords = "noir"; matchAllInScope = false }
                    Button("All in scope") { genres = ""; tags = ""; keywords = ""; yearMin = ""; yearMax = ""; matchAllInScope = true }
                }
                Toggle("Movies", isOn: $movies)
                Toggle("Episodes", isOn: $episodes)
                ForEach(model.libraries.filter { $0.kind == "movies" || $0.kind == "shows" }) { library in
                    Toggle(library.name, isOn: Binding(
                        get: { libraryIds.contains(library.id) },
                        set: { selected in
                            if selected { libraryIds = Array(Set(libraryIds + [library.id])).sorted() }
                            else { libraryIds.removeAll { $0 == library.id } }
                        }
                    ))
                }
                TextField("Genres, comma separated", text: $genres)
                TextField("Tags, comma separated", text: $tags)
                TextField("Title keywords, comma separated", text: $keywords)
                HStack { TextField("From year", text: $yearMin); TextField("Through year", text: $yearMax) }
                TextField("Search movies, series, episodes", text: $searchText)
                Button("Search titles") { Task { await searchTitles() } }
                    .disabled(busy || searchText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                ForEach(searchResults.prefix(12)) { item in
                    HStack {
                        Text(item.title).lineLimit(1)
                        Spacer()
                        Button("Include") { include(item) }
                        Button("Exclude") { exclude(item) }
                    }
                }
                if !includeItemIds.isEmpty || !includeShowIds.isEmpty || !excludeItemIds.isEmpty || !excludeShowIds.isEmpty {
                    Text("Included items \(includeItemIds.map(String.init).joined(separator: ", ")) · series \(includeShowIds.map(String.init).joined(separator: ", "))")
                        .font(.caption).foregroundStyle(.secondary)
                    Text("Excluded items \(excludeItemIds.map(String.init).joined(separator: ", ")) · series \(excludeShowIds.map(String.init).joined(separator: ", "))")
                        .font(.caption).foregroundStyle(.secondary)
                    ForEach(includeItemIds, id: \.self) { id in Button("Remove included item \(id)") { includeItemIds.removeAll { $0 == id } } }
                    ForEach(includeShowIds, id: \.self) { id in Button("Remove included series \(id)") { includeShowIds.removeAll { $0 == id } } }
                    ForEach(excludeItemIds, id: \.self) { id in Button("Remove excluded item \(id)") { excludeItemIds.removeAll { $0 == id } } }
                    ForEach(excludeShowIds, id: \.self) { id in Button("Remove excluded series \(id)") { excludeShowIds.removeAll { $0 == id } } }
                }
                Button("Preview matches") { Task { await loadPreview() } }.disabled(busy || (!movies && !episodes))
                if let preview {
                    Text("\(preview.eligibleCount) titles · \(preview.repeatDescription)")
                    ForEach(preview.firstTen) { entry in
                        let match = preview.matches.first { $0.candidate.itemId == entry.itemId }
                        VStack(alignment: .leading) {
                            Text(match?.candidate.title ?? "Title \(entry.itemId)")
                            Text(match?.reasons.joined(separator: " · ") ?? "scheduled").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            } }
            if step == 1 { Section("2 · Playback") {
                Picker("Order", selection: $ordering) {
                    ForEach(LibraryChannelOrdering.allCases) { Text($0.label).tag($0) }
                }
                Toggle("Include specials", isOn: $includeSpecials)
                Toggle("Refresh future rotations automatically", isOn: $autoRefresh)
                if let preview { Text("\(preview.eligibleCount) titles · \(preview.repeatDescription)") }
            } }
            if step == 2 { Section("3 · Channel") {
                TextField("Name", text: $name)
                TextField("Description", text: $description, axis: .vertical)
                if canShare {
                    Picker("Visibility", selection: $visibility) {
                        ForEach(LibraryChannelVisibility.allCases) { Text($0.label).tag($0) }
                    }
                } else {
                    Text("Personal channel · an administrator can make it shared")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Toggle("Enabled", isOn: $enabled)
            } }
            if let message { Text(message).foregroundStyle(.secondary) }
            if step == 2, channel != nil {
                Section {
                    Button("Apply after this programme") { Task { await rebuild(afterProgramme: true, reshuffle: false) } }
                    Button("Reshuffle next rotation") { Task { await rebuild(afterProgramme: false, reshuffle: true) } }
                    Button("Delete channel", role: .destructive) { Task { await deleteChannel() } }
                }
            }
        }
        .navigationTitle(channel == nil ? "Make a channel" : "Edit channel")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button(step == 0 ? "Cancel" : "Back") { if step == 0 { persistDraft(); dismiss() } else { step -= 1 } }
            }
            ToolbarItem(placement: .confirmationAction) {
                if step < 2 { Button("Next") { step += 1 }.disabled(busy || (step == 0 && !movies && !episodes)) }
                else { Button("Save") { Task { await save() } }.disabled(busy || !valid) }
            }
        }
        .onAppear { loadChannel(); restoreDraft(); applySeed() }
        .onChange(of: scenePhase) { _, phase in if phase != .active { persistDraft() } }
        .task {
            if let user = try? await model.requireAPI().me() {
                canShare = user.isAdmin == true
            }
            if !canShare { visibility = .personal }
        }
        .interactiveDismissDisabled(dirty)
    }

    private var valid: Bool {
        !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && name.count <= 80 && description.count <= 500 && (movies || episodes)
    }

    private var dirty: Bool {
        channel == nil ? !name.isEmpty || !description.isEmpty || preview != nil
            : definition.name != channel?.name || definition.description != channel?.description
                || definition.visibility != channel?.visibility || definition.enabled != channel?.enabled
                || definition.recipe != channel?.recipe
    }

    private var recipe: LibraryChannelRecipe {
        var value = channel?.recipe ?? LibraryChannelRecipe()
        value.libraryIds = libraryIds
        value.kinds = (movies ? ["movie"] : []) + (episodes ? ["episode"] : [])
        value.genresAny = list(genres)
        value.tagsAny = list(tags)
        value.keywordsAny = list(keywords)
        value.yearMin = Int(yearMin)
        value.yearMax = Int(yearMax)
        value.ordering = ordering
        value.includeSpecials = includeSpecials
        value.autoRefresh = autoRefresh
        value.includeItemIds = includeItemIds
        value.includeShowIds = includeShowIds
        value.excludeItemIds = excludeItemIds
        value.excludeShowIds = excludeShowIds
        // Empty criteria remain an editable draft. Only a previously explicit
        // all-in-scope preset retains that affirmative selection.
        value.matchAllInScope = matchAllInScope
        return value
    }

    private var definition: LibraryChannelDefinition {
        LibraryChannelDefinition(
            requestId: UUID().uuidString,
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            description: description.trimmingCharacters(in: .whitespacesAndNewlines),
            visibility: canShare ? visibility : .personal,
            enabled: enabled,
            recipe: recipe,
            previewSeed: previewSeed
        )
    }

    private struct StoredDraft: Codable {
        let channelId: String?
        let expectedRevision: Int64?
        let step: Int
        let definition: LibraryChannelDefinition
    }

    private var draftKey: String {
        "library-channel-draft-v2:\(model.origin):\(model.userId ?? 0):\(channel?.id ?? "new")"
    }

    private func persistDraft() {
        guard dirty, let data = try? JSONEncoder().encode(StoredDraft(
            channelId: channel?.id, expectedRevision: channel?.revision,
            step: step, definition: definition
        )) else { return }
        UserDefaults.standard.set(data, forKey: draftKey)
    }

    private func clearDraft() { UserDefaults.standard.removeObject(forKey: draftKey) }

    private func restoreDraft() {
        guard let data = UserDefaults.standard.data(forKey: draftKey),
              let stored = try? JSONDecoder().decode(StoredDraft.self, from: data),
              stored.channelId == channel?.id,
              stored.expectedRevision == channel?.revision
        else { return }
        let value = stored.definition
        name = value.name; description = value.description; visibility = value.visibility
        enabled = value.enabled; movies = value.recipe.kinds.contains("movie")
        episodes = value.recipe.kinds.contains("episode"); genres = value.recipe.genresAny.joined(separator: ", ")
        tags = value.recipe.tagsAny.joined(separator: ", "); keywords = value.recipe.keywordsAny.joined(separator: ", ")
        yearMin = value.recipe.yearMin.map(String.init) ?? ""; yearMax = value.recipe.yearMax.map(String.init) ?? ""
        ordering = value.recipe.ordering; includeSpecials = value.recipe.includeSpecials
        autoRefresh = value.recipe.autoRefresh; matchAllInScope = value.recipe.matchAllInScope
        libraryIds = value.recipe.libraryIds; includeItemIds = value.recipe.includeItemIds
        includeShowIds = value.recipe.includeShowIds; excludeItemIds = value.recipe.excludeItemIds
        excludeShowIds = value.recipe.excludeShowIds; previewSeed = value.previewSeed; step = stored.step
    }

    private func list(_ value: String) -> [String] {
        value.split(separator: ",").map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }.filter { !$0.isEmpty }
    }

    private func loadChannel() {
        guard let channel else { return }
        name = channel.name
        description = channel.description
        visibility = channel.visibility
        enabled = channel.enabled
        movies = channel.recipe.kinds.contains("movie")
        episodes = channel.recipe.kinds.contains("episode")
        genres = channel.recipe.genresAny.joined(separator: ", ")
        tags = channel.recipe.tagsAny.joined(separator: ", ")
        keywords = channel.recipe.keywordsAny.joined(separator: ", ")
        yearMin = channel.recipe.yearMin.map(String.init) ?? ""
        yearMax = channel.recipe.yearMax.map(String.init) ?? ""
        ordering = channel.recipe.ordering
        includeSpecials = channel.recipe.includeSpecials
        autoRefresh = channel.recipe.autoRefresh
        matchAllInScope = channel.recipe.matchAllInScope
        libraryIds = channel.recipe.libraryIds
        includeItemIds = channel.recipe.includeItemIds
        includeShowIds = channel.recipe.includeShowIds
        excludeItemIds = channel.recipe.excludeItemIds
        excludeShowIds = channel.recipe.excludeShowIds
        previewSeed = channel.seed.map { String(format: "%02x", $0) }.joined()
    }

    private func applySeed() {
        guard channel == nil, let seed else { return }
        if name.isEmpty { name = "\(seed.title) channel" }
        if seed.kind == "show" {
            includeShowIds = Array(Set(includeShowIds + [seed.itemId])).sorted()
        } else {
            includeItemIds = Array(Set(includeItemIds + [seed.itemId])).sorted()
        }
    }

    private func searchTitles() async {
        busy = true
        do {
            let response = try await model.requireAPI().search(searchText, limit: 30)
            searchResults = (response.results ?? []).filter { ["movie", "show", "episode"].contains($0.kind) }
            message = nil
        } catch { message = error.localizedDescription }
        busy = false
    }

    private func include(_ item: Item) {
        if item.kind == "show" { includeShowIds = Array(Set(includeShowIds + [item.id])).sorted() }
        else { includeItemIds = Array(Set(includeItemIds + [item.id])).sorted() }
    }

    private func exclude(_ item: Item) {
        if item.kind == "show" { excludeShowIds = Array(Set(excludeShowIds + [item.id])).sorted() }
        else { excludeItemIds = Array(Set(excludeItemIds + [item.id])).sorted() }
    }

    private func loadPreview() async {
        busy = true
        do {
            preview = try await model.requireAPI().previewLibraryChannel(recipe, seed: previewSeed)
            previewSeed = preview?.previewSeed
            message = nil
        }
        catch { message = error.localizedDescription }
        busy = false
    }

    private func save() async {
        busy = true
        do {
            if let channel { _ = try await model.requireAPI().updateLibraryChannel(channel, definition: definition) }
            else { _ = try await model.requireAPI().createLibraryChannel(definition) }
            clearDraft(); saved(); dismiss()
        } catch { message = error.localizedDescription }
        busy = false
    }

    private func rebuild(afterProgramme: Bool, reshuffle: Bool) async {
        guard let channel else { return }
        busy = true
        do {
            _ = try await model.requireAPI().rebuildLibraryChannel(
                channel,
                activation: afterProgramme ? "next_programme" : "next_rotation",
                reshuffle: reshuffle
            )
            message = afterProgramme ? "The new schedule will begin after this programme." : "The next rotation was rebuilt."
        } catch { message = error.localizedDescription }
        busy = false
    }

    private func deleteChannel() async {
        guard let channel else { return }
        busy = true
        do { try await model.requireAPI().deleteLibraryChannel(channel); clearDraft(); saved(); dismiss() }
        catch { message = error.localizedDescription; busy = false }
    }
}
