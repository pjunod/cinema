import SwiftUI

struct SharedLibrariesEntry: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    var body: some View {
        NavigationLink(value: Route.sharedLibraries) {
            Label("Shared libraries · Browse by Source", systemImage: "network")
        }
        .accessibilityIdentifier("shared-libraries-entry")
        .remoteControl("libraries:shared", label: "Browse Shared libraries") { navigation.navigate(to: .sharedLibraries) }
    }
}

private struct SharedRemoteOrderKey: PreferenceKey {
    static let defaultValue: [String] = []
    static func reduce(value: inout [String], nextValue: () -> [String]) { value += nextValue() }
}

struct SharedLibrariesView: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @State private var assignments: [SharedLibraryAssignment] = []
    @State private var history: [SharedContinueGroup] = []
    @State private var loading = false
    @State private var error: String?
    @State private var remoteLoadGeneration = UUID()
    private var groups: [[SharedLibraryAssignment]] {
        Dictionary(grouping: assignments, by: { $0.identity.sourceId }).values
            .map { $0.sorted { $0.libraryId < $1.libraryId } }
            .sorted { $0[0].sourceName == $1[0].sourceName ? $0[0].identity.sourceId < $1[0].identity.sourceId : $0[0].sourceName < $1[0].sourceName }
    }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text("Libraries shared with your account on this server.").foregroundStyle(.secondary)
                if loading { ProgressView() }
                if let error { Text(error).foregroundStyle(.secondary) }
                if !loading && error == nil && assignments.isEmpty { Text("No Shared libraries are assigned to your account.") }
                ForEach(groups, id: \.first!.identity.sourceId) { group in SharedSourceLibrariesView(assignments: group, historyGroup: history.first { $0.id == group.first!.identity.sourceId }) }
                Button("Refresh Shared libraries") { Task { await load() } }.disabled(loading)
                    .id("shared:refresh")
                    .remoteControl("shared:refresh", label: "Refresh Shared libraries", enabled: !loading) { Task { await load() } }
            }.padding()
        }
        .remoteScrollRealization()
        .onPreferenceChange(SharedRemoteOrderKey.self) { keys in
            navigation.setOrder(scope: "library:shared", keys: keys + ["shared:refresh"], columns: 1)
        }
        .navigationTitle("Shared libraries")
        .task { await load() }
        .onDisappear { remoteLoadGeneration = UUID() }
    }
    private func load() async {
        remoteLoadGeneration = UUID()
        let generation = remoteLoadGeneration, epoch = navigation.epoch
        func current() -> Bool { generation == remoteLoadGeneration && epoch == navigation.epoch && !Task.isCancelled }
        loading = true; defer { if current() { loading = false } }
        do {
            let client = try SharedLibraryClient()
            let rows = try await client.assignments(); try client.requireCurrent(); guard current() else { return }
            assignments = rows; error = nil
            do { let result = try await client.continueGroups(); try client.requireCurrent(); guard current() else { return }; history = result }
            catch { guard current() else { return }; history = []; self.error = "Shared Continue Watching is unavailable." }
        } catch { guard current() else { return }; self.error = error.localizedDescription }
    }
}

private struct SharedSourceLibrariesView: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let assignments: [SharedLibraryAssignment]
    let historyGroup: SharedContinueGroup?
    @State private var recent: SharedContinueItems?
    @State private var historyError: String?
    @State private var rows: [SharedLibraryRow] = []
    @State private var loading = false
    @State private var error: String?
    @State private var remoteLoadGeneration = UUID()
    private var sourceName: String { assignments.first!.sourceName.isEmpty ? "Shared Source" : assignments.first!.sourceName }
    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text(sourceName).font(.title3.bold())
            Text("Source · \(assignments.first!.serverId.prefix(8))").font(.caption).foregroundStyle(.secondary)
            if let historyGroup {
                Text("Continue Watching · \(historyGroup.sourceName)").font(.headline)
                if let recent {
                    if recent.availability != "online" { Text("Continue Watching unavailable for this Source.").foregroundStyle(.secondary) }
                    ForEach(recent.items) { entry in
                        let reference = entry.item.reference
                        let identity = SharedLibraryIdentity(importId: reference.importId, serverId: reference.serverId, catalogueEpoch: reference.catalogueEpoch, libraryId: reference.libraryId)
                        let row = rows.first { $0.identity == identity } ?? SharedLibraryRow(identity: identity, name: "Shared library", sourceName: sourceName, kind: entry.item.kind)
                        NavigationLink(value: Route.sharedItem(reference, row)) {
                            VStack(alignment: .leading) {
                                Text(entry.item.title)
                                Text("Resume at \(entry.watch.positionMs / 1000) s · \(sourceName)").font(.caption).foregroundStyle(.secondary)
                            }
                        }
                        .id("shared:continue:" + entry.id)
                        .remoteControl("shared:continue:" + entry.id, label: entry.item.title) { navigation.navigate(to: .sharedItem(reference, row)) }
                    }
                }
                if let historyError { Text(historyError).foregroundStyle(.secondary) }
            }
            if loading { ProgressView() }
            if let error { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary) }
            ForEach(rows) { row in
                NavigationLink(value: Route.sharedLibrary(row, parent: nil, title: nil)) {
                    VStack(alignment: .leading) { Text(row.name); Text("\(row.kind) · \(row.sourceName)").font(.caption).foregroundStyle(.secondary) }
                }
                .id("shared:library:" + row.id)
                .remoteControl("shared:library:" + row.id, label: row.name) { navigation.navigate(to: .sharedLibrary(row, parent: nil, title: nil)) }
            }
            if !loading && rows.isEmpty && error == nil { Text("No libraries are currently available from this Source.") }
            Button("Refresh \(sourceName)") { Task { await load() } }.disabled(loading)
                .id("shared:source-refresh:" + assignments.first!.identity.sourceId)
                .remoteControl("shared:source-refresh:" + assignments.first!.identity.sourceId, label: "Refresh " + sourceName, enabled: !loading) { Task { await load() } }
        }
        .preference(key: SharedRemoteOrderKey.self, value: (recent?.items ?? []).map { "shared:continue:" + $0.id } + rows.map { "shared:library:" + $0.id } + ["shared:source-refresh:" + assignments.first!.identity.sourceId])
        .task(id: assignments.map(\.id).joined(separator: ",") + "|" + (historyGroup?.id ?? "") + "|" + String(historyGroup?.count ?? 0)) { await load() }
        .onDisappear { remoteLoadGeneration = UUID() }
    }
    private func load() async {
        remoteLoadGeneration = UUID()
        let generation = remoteLoadGeneration, epoch = navigation.epoch
        func current() -> Bool { generation == remoteLoadGeneration && epoch == navigation.epoch && !Task.isCancelled }
        loading = true; recent = nil; defer { if current() { loading = false } }
        do {
            let client = try SharedLibraryClient()
            do {
                let result = try await client.libraries(assignments); try client.requireCurrent(); guard current() else { return }; rows = result; error = nil
            } catch { guard current() else { return }; rows = []; self.error = "Source libraries unavailable." }
            if let historyGroup {
                do { let result = try await client.continueItems(historyGroup, assigned: assignments); try client.requireCurrent(); guard current() else { return }; recent = result; historyError = nil }
                catch { guard current() else { return }; recent = nil; historyError = "Continue Watching unavailable for this Source." }
            }
        } catch { guard current() else { return }; rows = []; recent = nil; self.error = error.localizedDescription }
    }
}

struct SharedLibraryItemsView: View {
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    let library: SharedLibraryRow
    var parent: SharedPlaybackReference? = nil
    var title: String? = nil
    @State private var query = ""
    @State private var searchNonce = UUID()
    @State private var browse = SharedBrowseAccumulator()
    @State private var loading = false
    @State private var error: String?
    @State private var remoteLoadGeneration = UUID()
    @State private var requestGeneration = UUID()
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                Text("Source · \(library.sourceName)").font(.subheadline).foregroundStyle(.secondary)
                TextField("Find a title", text: $query)
                if let error { Text(error).foregroundStyle(.secondary) }
                ForEach(browse.items) { item in
                    NavigationLink(value: Route.sharedItem(item.reference, library)) {
                        HStack(alignment: .top, spacing: 12) {
                            SharedArtworkImage(subject: item.artworkSubject).frame(width: 72, height: 108).clipShape(RoundedRectangle(cornerRadius: 8))
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.title).font(.headline)
                                Text([item.kind, item.year.map(String.init), library.sourceName].compactMap { $0 }.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                    .id("shared:item:" + item.id)
                    .remoteControl("shared:item:" + item.id, label: item.title) { navigation.navigate(to: .sharedItem(item.reference, library)) }
                }
                if loading { ProgressView() }
                if !loading && error == nil && browse.items.isEmpty { Text("No matching titles in this Shared library.") }
                if browse.nextCursor != nil { Button("Load more") { Task { await load(reset: false) } }.disabled(loading)
                    .id("shared:more")
                    .remoteControl("shared:more", label: "Load more Shared titles", enabled: !loading) { Task { await load(reset: false) } } }
                Button("Refresh list") { Task { await load(reset: true) } }.disabled(loading)
                    .id("shared:list-refresh")
                    .remoteControl("shared:list-refresh", label: "Refresh list", enabled: !loading) { Task { await load(reset: true) } }
            }.padding()
        }
        .remoteScrollRealization()
        .onAppear {
            updateRemoteOrder()
            navigation.setSearch(scope: remoteScope, nonce: searchNonce) { query = $0 }
        }
        .onDisappear {
            navigation.removeSearch(scope: remoteScope, nonce: searchNonce)
            searchNonce = UUID(); requestGeneration = UUID(); loading = false
        }
        .onChange(of: browse.items.map(\.id)) { _, _ in updateRemoteOrder() }
        .onChange(of: browse.nextCursor) { _, _ in updateRemoteOrder() }
        .navigationTitle(title ?? library.name)
        .task(id: query) { await load(reset: true) }
    }
    private var remoteScope: String { RemoteNavigationCoordinator.routeScope(.sharedLibrary(library, parent: parent, title: title)) }
    private func updateRemoteOrder() {
        navigation.setOrder(scope: remoteScope, keys: browse.items.map { "shared:item:" + $0.id } + (browse.nextCursor == nil ? [] : ["shared:more"]) + ["shared:list-refresh"], columns: 1)
    }
    private func load(reset: Bool) async {
        if reset { requestGeneration = UUID(); browse = SharedBrowseAccumulator() }
        let generation = requestGeneration
        let cursor = reset ? nil : browse.nextCursor
        loading = true
        do {
            let client = try SharedLibraryClient()
            let page = try await client.page(library.identity, parent: parent, q: query, cursor: cursor)
            try client.requireCurrent(); guard generation == requestGeneration, !Task.isCancelled else { return }
            try browse.append(page, requestedCursor: cursor, library: library.identity); error = nil
        } catch { if generation == requestGeneration, !Task.isCancelled { self.error = error.localizedDescription } }
        if generation == requestGeneration { loading = false }
    }
}

struct SharedLibraryDetailView: View {
    let reference: SharedPlaybackReference
    let library: SharedLibraryRow
    @EnvironmentObject private var model: AppModel
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @State private var remoteViewLifetime = UUID()
    @State private var remotePreplay = false
    @State private var preparingRemote = false
    @State private var preparationTask: Task<Void, Never>?
    @State private var playbackPlan: SharedPlaybackPlan?
    @State private var detail: SharedLibraryDetail?
    @State private var error: String?
    @State private var remoteLoadGeneration = UUID()
    @State private var loading = false
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Source · \(library.sourceName)").font(.subheadline).foregroundStyle(.secondary)
                if loading || preparingRemote { ProgressView() }
                if let error { Text(error).foregroundStyle(.secondary) }
                if let detail {
                    if detail.item.backdropUrl != nil { SharedArtworkImage(subject: detail.item.artworkSubject, backdrop: true).frame(maxWidth: 780).frame(height: 240) }
                    Text(detail.item.title).font(.title.bold())
                    Text([detail.item.kind, detail.item.year.map(String.init)].compactMap { $0 }.joined(separator: " · "))
                    if let overview = detail.item.overview { Text(overview) }
                    if !detail.item.genres.isEmpty { Text(detail.item.genres.joined(separator: " · ")).foregroundStyle(.secondary) }
                    if detail.deliveryStatus != "available" {
                        Label("Playback is unavailable for this Shared title.", systemImage: "info.circle")
                            .accessibilityIdentifier("shared-playback-unavailable")
                    }
                    if let watch = detail.watch {
                        Text(watch.watched ? "Watched on this server" : "Position on this server: \(watch.positionMs / 1000) seconds")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    if SharedWatchedAction.applies(to: detail.item) {
                        let watched = detail.watch?.watched == true
                        Button(SharedWatchedAction.title(watched: watched)) { Task { await setWatched(!watched) } }
                            .disabled(loading)
                            .accessibilityIdentifier("shared-watched-toggle")
                    }
                    if detail.item.hasChildren {
                        NavigationLink("Browse children", value: Route.sharedLibrary(library, parent: reference, title: detail.item.title))
                            .id("shared:children")
                            .remoteControl("shared:children", label: "Browse children") { navigation.navigate(to: .sharedLibrary(library, parent: reference, title: detail.item.title)) }
                    }
                    ForEach(detail.files) { file in
                        VStack(alignment: .leading, spacing: 4) {
                            Text([file.container, file.videoCodec].compactMap { $0 }.joined(separator: " · "))
                            if let width = file.width, let height = file.height { Text("\(width) × \(height)") }
                            if let duration = file.durationMs { Text("Duration: \(duration / 1000) seconds") }
                            if detail.deliveryStatus == "available" && file.fileBase != nil {
                                Button("Play Shared file") { remotePreplay = false; Task { await play(file.fileId) } }.disabled(loading)
                                    .id("shared:preplay:" + file.id)
                                    .remoteControl("shared:preplay:" + file.id, label: "Prepare Shared file", enabled: !loading, admission: {
                                        !preparingRemote && self.detail?.deliveryStatus == "available" && self.detail?.files.contains(where: { $0.id == file.id && $0.fileBase == file.fileBase }) == true ? nil : .staleContext
                                    }) { prepareRemote(file) }
                            }
                        }.font(.caption).foregroundStyle(.secondary)
                    }
                }
                Button("Refresh details") { Task { await load() } }.disabled(loading)
                    .id("shared:detail-refresh")
                    .remoteControl("shared:detail-refresh", label: "Refresh Shared title", enabled: !loading) { Task { await load() } }
            }.padding()
        }
        .remoteScrollRealization()
        .onAppear { updateRemoteDetailOrder() }
        .onChange(of: detail?.item.hasChildren) { _, _ in updateRemoteDetailOrder() }
        .onChange(of: detail?.files.map(\.id)) { _, _ in navigation.contextChanged(); updateRemoteDetailOrder() }
        .navigationTitle(detail?.item.title ?? "Shared title")
        .sheet(isPresented: Binding(get: { playbackPlan != nil }, set: { if !$0 { playbackPlan = nil } })) {
            if let playbackPlan { SharedPlayerView(plan: playbackPlan, requiresExplicitPlay: remotePreplay).environmentObject(model) }
        }
        .task(id: reference) { await load() }
        .onDisappear { remoteLoadGeneration = UUID(); remoteViewLifetime = UUID(); preparationTask?.cancel(); preparationTask = nil }
    }
    private func updateRemoteDetailOrder() {
        navigation.setOrder(scope: RemoteNavigationCoordinator.routeScope(.sharedItem(reference, library)), keys: (detail?.item.hasChildren == true ? ["shared:children"] : []) + (detail?.deliveryStatus == "available" ? (detail?.files ?? []).filter { $0.fileBase != nil }.map { "shared:preplay:" + $0.id } : []) + ["shared:detail-refresh"], columns: 1)
    }
    @MainActor
    private func prepareRemote(_ file: SharedLibraryFile) {
        guard !loading, !preparingRemote else { return }
        preparationTask?.cancel()
        let lifetime = remoteViewLifetime
        let context = navigation.context
        let scope = RemoteNavigationCoordinator.routeScope(.sharedItem(reference, library))
        preparingRemote = true
        preparationTask = Task {
            @MainActor func current() -> Bool {
                !Task.isCancelled && remoteViewLifetime == lifetime && navigation.context == context && navigation.activeScope == scope &&
                detail?.deliveryStatus == "available" && detail?.files.contains(where: { $0.id == file.id && $0.fileBase == file.fileBase }) == true
            }
            do {
                let plan = try await model.prepareSharedPlayback(reference: reference, fileId: file.fileId, current: current)
                guard current() else { if remoteViewLifetime == lifetime { preparingRemote = false }; return }
                remotePreplay = true; playbackPlan = plan; error = nil
            } catch { if current() { self.error = "Shared preparation could not complete. Try again." } }
            if remoteViewLifetime == lifetime { preparingRemote = false }
        }
    }
    private func play(_ file: String) async {
        let lifetime = remoteViewLifetime, epoch = navigation.epoch
        func current() -> Bool { lifetime == remoteViewLifetime && epoch == navigation.epoch && !Task.isCancelled }
        loading = true; defer { if current() { loading = false } }
        do {
            let plan = try await model.prepareSharedPlayback(reference: reference, fileId: file, current: current)
            guard current() else { return }
            playbackPlan = plan; error = nil
        } catch { if current() { self.error = error.localizedDescription } }
    }
    private func load() async {
        remoteLoadGeneration = UUID()
        let generation = remoteLoadGeneration, epoch = navigation.epoch
        func current() -> Bool { generation == remoteLoadGeneration && epoch == navigation.epoch && !Task.isCancelled }
        loading = true; defer { if current() { loading = false } }
        do {
            let client = try SharedLibraryClient()
            let result = try await client.detail(reference); try client.requireCurrent(); guard current() else { return }
            detail = result; error = nil
        } catch { guard current() else { return }; self.error = error.localizedDescription }
    }
    /// B's own watched override, then a fresh detail read so the page shows
    /// what B now holds rather than what was asked.
    private func setWatched(_ watched: Bool) async {
        loading = true
        do {
            let client = try SharedLibraryClient()
            _ = try await client.setWatched(reference, watched: watched); try client.requireCurrent()
            loading = false
            await load()
        } catch { loading = false; self.error = error.localizedDescription }
    }
}

/// Where the manual watched control appears: B accepts it for a movie or an
/// episode only (`sharing_watch_unsupported` otherwise), as Local offers it.
enum SharedWatchedAction {
    static func applies(to item: SharedLibraryItem) -> Bool { item.kind == "movie" || item.kind == "episode" }
    static func title(watched: Bool) -> String { watched ? "Mark unwatched" : "Mark watched" }
}
