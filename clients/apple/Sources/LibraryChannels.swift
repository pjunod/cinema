import AVKit
import Foundation
import SwiftUI

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
}

struct LibraryChannelUpdateRequest: Codable {
    let expectedRevision: Int64
    let requestId: String
    let name: String
    let description: String
    let visibility: LibraryChannelVisibility
    let enabled: Bool
    let recipe: LibraryChannelRecipe
}

struct LibraryChannelMutation: Codable {
    let channel: LibraryChannel
    let buildState: String
}

struct LibraryChannelPreviewRequest: Codable {
    let recipe: LibraryChannelRecipe
    let limit: Int
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

    let eligibleCount: Int
    let uniqueDurationMs: Int64
    let repeatDescription: String
    let contentDigest: String
    let matches: [Match]
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

    let player = AVPlayer()
    private var api: PlurxAPI?
    private weak var model: AppModel?
    private var profileOrigin: String?
    private var tuneSequence: UInt64 = 0
    private var playbackId = UUID().uuidString
    private var sessionId: String?
    private var boundary: Task<Void, Never>?
    private var endObserver: NSObjectProtocol?

    deinit {
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
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
            let ids = Array(channels.prefix(20).map(\.id))
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
        message = "Joining \(channel.name)…"
        do {
            let occurrence = try await api.resolveLibraryChannel(channel.id)
            guard expected == tuneSequence else { return }
            var request = CreateSessionRequest(
                playbackId: playbackId,
                start: Double(occurrence.positionMs) / 1_000,
                nativeSubtitles: true,
                caps: model.caps()
            )
            request.presentation = "vod"
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
            player.play()
            message = "Following live · seeking and watch history are off"
            scheduleBoundary(channel: channel, occurrence: occurrence, sequence: expected)
        } catch is CancellationError {
        } catch {
            if expected == tuneSequence { message = error.localizedDescription }
        }
        if expected == tuneSequence { busy = false }
    }

    func togglePause() async {
        guard let channel = watching else { return }
        if paused {
            if let resolved,
               Int64(Date().timeIntervalSince1970 * 1_000) >= resolved.endsAtMs {
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
    }

    func stop() async {
        tuneSequence &+= 1
        boundary?.cancel()
        boundary = nil
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = nil
        player.pause()
        player.replaceCurrentItem(with: nil)
        if let sessionId, let model { await model.endHlsSession(sessionId) }
        sessionId = nil
        watching = nil
        resolved = nil
        title = nil
        paused = false
        busy = false
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
        let delayMs = max(0, occurrence.endsAtMs - Int64(Date().timeIntervalSince1970 * 1_000))
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

    private func observeEnd(_ item: AVPlayerItem, channel: LibraryChannel, sequence: UInt64) {
        if let endObserver { NotificationCenter.default.removeObserver(endObserver) }
        endObserver = NotificationCenter.default.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime,
            object: item,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                guard let self, self.tuneSequence == sequence, !self.paused else { return }
                await self.tune(channel)
            }
        }
    }
}

// MARK: - Browse, guide, and authoring

struct LibraryChannelsView: View {
    @EnvironmentObject private var model: AppModel
    @StateObject private var controller = LibraryChannelPlayerController.shared
    @State private var editing: LibraryChannel?
    @State private var creating = false

    var body: some View {
        GeometryReader { proxy in
            let wide = proxy.size.width >= 900
            Group {
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
            }
            .padding()
        }
        .navigationTitle("Library channels")
        .toolbar {
            #if os(iOS)
            ToolbarItem(placement: .primaryAction) {
                Button { creating = true } label: { Label("Make a channel", systemImage: "plus") }
            }
            #endif
        }
        .sheet(isPresented: $creating) {
            NavigationStack { LibraryChannelEditor(channel: nil) { Task { await controller.refresh() } } }
        }
        .sheet(item: $editing) { channel in
            NavigationStack { LibraryChannelEditor(channel: channel) { Task { await controller.refresh() } } }
        }
        .task {
            await controller.load(model: model)
            while !Task.isCancelled {
                do { try await Task.sleep(nanoseconds: 30_000_000_000) } catch { return }
                await controller.refresh()
            }
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
                    NavigationLink("Watch from start", value: Route.item(resolved.itemId))
                    Button("Stop") { Task { await controller.stop() } }
                }
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
                        ForEach(guide.prefix(4)) { programme in
                            HStack {
                                Text(programme.title).lineLimit(1)
                                Spacer()
                                Text(Date(timeIntervalSince1970: Double(programme.startsAtMs) / 1_000), style: .time)
                                    .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
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
    let channel: LibraryChannel?
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
    @State private var preview: LibraryChannelPreview?
    @State private var busy = false
    @State private var message: String?

    var body: some View {
        Form {
            Section("1 · Content") {
                Toggle("Movies", isOn: $movies)
                Toggle("Episodes", isOn: $episodes)
                TextField("Genres, comma separated", text: $genres)
                TextField("Tags, comma separated", text: $tags)
                TextField("Title keywords, comma separated", text: $keywords)
                HStack { TextField("From year", text: $yearMin); TextField("Through year", text: $yearMax) }
                Button("Preview matches") { Task { await loadPreview() } }.disabled(busy || !valid)
                if let preview {
                    Text("\(preview.eligibleCount) titles · \(preview.repeatDescription)")
                    ForEach(preview.matches.prefix(10)) { match in
                        VStack(alignment: .leading) {
                            Text(match.candidate.title)
                            Text(match.reasons.joined(separator: " · ")).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            Section("2 · Playback") {
                Picker("Order", selection: $ordering) {
                    ForEach(LibraryChannelOrdering.allCases) { Text($0.label).tag($0) }
                }
                Toggle("Include specials", isOn: $includeSpecials)
                Toggle("Refresh future rotations automatically", isOn: $autoRefresh)
            }
            Section("3 · Channel") {
                TextField("Name", text: $name)
                TextField("Description", text: $description, axis: .vertical)
                Picker("Visibility", selection: $visibility) {
                    ForEach(LibraryChannelVisibility.allCases) { Text($0.label).tag($0) }
                }
                Toggle("Enabled", isOn: $enabled)
            }
            if let message { Text(message).foregroundStyle(.secondary) }
            if channel != nil {
                Section {
                    Button("Apply after this programme") { Task { await rebuild(afterProgramme: true, reshuffle: false) } }
                    Button("Reshuffle next rotation") { Task { await rebuild(afterProgramme: false, reshuffle: true) } }
                    Button("Delete channel", role: .destructive) { Task { await deleteChannel() } }
                }
            }
        }
        .navigationTitle(channel == nil ? "Make a channel" : "Edit channel")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) { Button("Save") { Task { await save() } }.disabled(busy || !valid) }
        }
        .onAppear { loadChannel() }
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
        value.kinds = (movies ? ["movie"] : []) + (episodes ? ["episode"] : [])
        value.genresAny = list(genres)
        value.tagsAny = list(tags)
        value.keywordsAny = list(keywords)
        value.yearMin = Int(yearMin)
        value.yearMax = Int(yearMax)
        value.ordering = ordering
        value.includeSpecials = includeSpecials
        value.autoRefresh = autoRefresh
        value.matchAllInScope = value.libraryIds.isEmpty && value.genresAny.isEmpty && value.tagsAny.isEmpty
            && value.keywordsAny.isEmpty && value.yearMin == nil && value.yearMax == nil
            && value.includeItemIds.isEmpty && value.includeShowIds.isEmpty
        return value
    }

    private var definition: LibraryChannelDefinition {
        LibraryChannelDefinition(
            requestId: UUID().uuidString,
            name: name.trimmingCharacters(in: .whitespacesAndNewlines),
            description: description.trimmingCharacters(in: .whitespacesAndNewlines),
            visibility: visibility,
            enabled: enabled,
            recipe: recipe
        )
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
    }

    private func loadPreview() async {
        busy = true
        do { preview = try await model.requireAPI().previewLibraryChannel(recipe); message = nil }
        catch { message = error.localizedDescription }
        busy = false
    }

    private func save() async {
        busy = true
        do {
            if let channel { _ = try await model.requireAPI().updateLibraryChannel(channel, definition: definition) }
            else { _ = try await model.requireAPI().createLibraryChannel(definition) }
            saved(); dismiss()
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
        do { try await model.requireAPI().deleteLibraryChannel(channel); saved(); dismiss() }
        catch { message = error.localizedDescription; busy = false }
    }
}
