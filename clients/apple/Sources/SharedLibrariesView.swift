import SwiftUI

struct SharedLibrariesEntry: View {
    var body: some View {
        NavigationLink { SharedLibrariesView() } label: {
            Label("Shared libraries · Browse by Source", systemImage: "network")
        }
        .accessibilityIdentifier("shared-libraries-entry")
    }
}

struct SharedLibrariesView: View {
    @State private var assignments: [SharedLibraryAssignment] = []
    @State private var history: [SharedContinueGroup] = []
    @State private var loading = false
    @State private var error: String?
    private var groups: [[SharedLibraryAssignment]] {
        Dictionary(grouping: assignments, by: { $0.identity.sourceId }).values
            .map { $0.sorted { $0.libraryId < $1.libraryId } }
            .sorted { $0[0].sourceName == $1[0].sourceName ? $0[0].identity.sourceId < $1[0].identity.sourceId : $0[0].sourceName < $1[0].sourceName }
    }
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 20) {
                Text("Libraries shared with your account on this server.").foregroundStyle(.secondary)
                if loading { ProgressView() }
                if let error { Text(error).foregroundStyle(.secondary) }
                if !loading && error == nil && assignments.isEmpty { Text("No Shared libraries are assigned to your account.") }
                ForEach(groups, id: \.first!.identity.sourceId) { group in SharedSourceLibrariesView(assignments: group, historyGroup: history.first { $0.id == group.first!.identity.sourceId }) }
                Button("Refresh Shared libraries") { Task { await load() } }.disabled(loading)
            }.padding()
        }
        .navigationTitle("Shared libraries")
        .task { await load() }
    }
    private func load() async {
        loading = true; defer { loading = false }
        do {
            let client = try SharedLibraryClient()
            let rows = try await client.assignments(); try client.requireCurrent()
            assignments = rows; error = nil
            do { history = try await client.continueGroups(); try client.requireCurrent() }
            catch { history = []; self.error = "Shared Continue Watching is unavailable." }
        } catch { self.error = error.localizedDescription }
    }
}

private struct SharedSourceLibrariesView: View {
    let assignments: [SharedLibraryAssignment]
    let historyGroup: SharedContinueGroup?
    @State private var recent: SharedContinueItems?
    @State private var historyError: String?
    @State private var rows: [SharedLibraryRow] = []
    @State private var loading = false
    @State private var error: String?
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
                        NavigationLink { SharedLibraryDetailView(reference: reference, library: row) } label: {
                            VStack(alignment: .leading) {
                                Text(entry.item.title)
                                Text("Resume at \(entry.watch.positionMs / 1000) s · \(sourceName)").font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                if let historyError { Text(historyError).foregroundStyle(.secondary) }
            }
            if loading { ProgressView() }
            if let error { Label(error, systemImage: "exclamationmark.triangle").foregroundStyle(.secondary) }
            ForEach(rows) { row in
                NavigationLink { SharedLibraryItemsView(library: row) } label: {
                    VStack(alignment: .leading) { Text(row.name); Text("\(row.kind) · \(row.sourceName)").font(.caption).foregroundStyle(.secondary) }
                }
            }
            if !loading && rows.isEmpty && error == nil { Text("No libraries are currently available from this Source.") }
            Button("Refresh \(sourceName)") { Task { await load() } }.disabled(loading)
        }
        .task(id: assignments.map(\.id).joined(separator: ",") + "|" + (historyGroup?.id ?? "") + "|" + String(historyGroup?.count ?? 0)) { await load() }
    }
    private func load() async {
        loading = true; recent = nil; defer { loading = false }
        do {
            let client = try SharedLibraryClient()
            do {
                rows = try await client.libraries(assignments); try client.requireCurrent(); error = nil
            } catch { rows = []; self.error = "Source libraries unavailable." }
            if let historyGroup {
                do { recent = try await client.continueItems(historyGroup, assigned: assignments); try client.requireCurrent(); historyError = nil }
                catch { recent = nil; historyError = "Continue Watching unavailable for this Source." }
            }
        } catch { rows = []; recent = nil; self.error = error.localizedDescription }
    }
}

struct SharedLibraryItemsView: View {
    let library: SharedLibraryRow
    var parent: SharedPlaybackReference? = nil
    var title: String? = nil
    @State private var query = ""
    @State private var browse = SharedBrowseAccumulator()
    @State private var loading = false
    @State private var error: String?
    @State private var requestGeneration = UUID()
    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 16) {
                Text("Source · \(library.sourceName)").font(.subheadline).foregroundStyle(.secondary)
                TextField("Find a title", text: $query)
                if let error { Text(error).foregroundStyle(.secondary) }
                ForEach(browse.items) { item in
                    NavigationLink { SharedLibraryDetailView(reference: item.reference, library: library) } label: {
                        HStack(alignment: .top, spacing: 12) {
                            SharedArtworkImage(subject: item.artworkSubject).frame(width: 72, height: 108).clipShape(RoundedRectangle(cornerRadius: 8))
                            VStack(alignment: .leading, spacing: 4) {
                                Text(item.title).font(.headline)
                                Text([item.kind, item.year.map(String.init), library.sourceName].compactMap { $0 }.joined(separator: " · "))
                                    .font(.caption).foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                if loading { ProgressView() }
                if !loading && error == nil && browse.items.isEmpty { Text("No matching titles in this Shared library.") }
                if browse.nextCursor != nil { Button("Load more") { Task { await load(reset: false) } }.disabled(loading) }
                Button("Refresh list") { Task { await load(reset: true) } }.disabled(loading)
            }.padding()
        }
        .navigationTitle(title ?? library.name)
        .task(id: query) { await load(reset: true) }
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
    @State private var playbackPlan: SharedPlaybackPlan?
    @State private var detail: SharedLibraryDetail?
    @State private var error: String?
    @State private var loading = false
    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("Source · \(library.sourceName)").font(.subheadline).foregroundStyle(.secondary)
                if loading { ProgressView() }
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
                        NavigationLink("Browse children") {
                            SharedLibraryItemsView(library: library, parent: reference, title: detail.item.title)
                        }
                    }
                    ForEach(detail.files) { file in
                        VStack(alignment: .leading, spacing: 4) {
                            Text([file.container, file.videoCodec].compactMap { $0 }.joined(separator: " · "))
                            if let width = file.width, let height = file.height { Text("\(width) × \(height)") }
                            if let duration = file.durationMs { Text("Duration: \(duration / 1000) seconds") }
                            if detail.deliveryStatus == "available" && file.fileBase != nil {
                                Button("Play Shared file") { Task { await play(file.fileId) } }.disabled(loading)
                            }
                        }.font(.caption).foregroundStyle(.secondary)
                    }
                }
                Button("Refresh details") { Task { await load() } }.disabled(loading)
            }.padding()
        }
        .navigationTitle(detail?.item.title ?? "Shared title")
        .sheet(isPresented: Binding(get: { playbackPlan != nil }, set: { if !$0 { playbackPlan = nil } })) {
            if let playbackPlan { SharedPlayerView(plan: playbackPlan).environmentObject(model) }
        }
        .task(id: reference) { await load() }
    }
    private func play(_ file: String) async {
        loading = true; defer { loading = false }
        do { playbackPlan = try await model.prepareSharedPlayback(reference: reference, fileId: file); error = nil }
        catch { self.error = error.localizedDescription }
    }
    private func load() async {
        loading = true; defer { loading = false }
        do {
            let client = try SharedLibraryClient()
            let result = try await client.detail(reference); try client.requireCurrent()
            detail = result; error = nil
        } catch { self.error = error.localizedDescription }
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
