import SwiftUI

struct LibraryView: View {
    @EnvironmentObject var model: AppModel
    let collection: LibraryCollection

    @State private var items: [Item] = []
    @State private var sort: LibrarySort = .title
    @State private var filter: WatchFilter = .all
    @State private var query = ""
    @State private var loading = true
    @State private var error: String?
    @State private var visibleItems: [Item] = []
    @State private var pager: LibraryMerge?
    @State private var fetchTask: Task<Void, Never>?
    @State private var requestedThrough = 0
    @State private var loadedCount = 0
    @State private var total = 0
    @State private var complete = false
    @State private var filterGeneration = 0
    @State private var pageGeneration = 0
    @State private var driveTask: Task<Void, Never>?


    private var sorts: [LibrarySort] {
        LibrarySort.allCases.filter { $0 != .recorded || collection.supportsRecordedSort }
    }

    private var columns: [GridItem] {
        [GridItem(.adaptive(minimum: model.posterSize.posterWidth), spacing: 18, alignment: .top)]
    }

    private var loadKey: String { "\(collection.id):\(sort.rawValue)" }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                summary
                TextField("Find a title in this library", text: $query)
                    #if os(iOS)
                    .textFieldStyle(.roundedBorder)
                    #endif
                    .accessibilityLabel("Find a title in this library")
                stateContent
            }
            .padding(.horizontal, screenHPad)
            .padding(.bottom, 36)
        }
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle(collection.title)
        #if os(iOS)
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await load() }
        #endif
        .toolbar { libraryToolbar }
        .task(id: loadKey) { await load() }
        .task(id: query) {
            try? await Task.sleep(for: .milliseconds(150))
            guard !Task.isCancelled else { return }
            filterNow()
            runDrive()
        }
        .task(id: filter) {
            filterNow()
            runDrive()
        }
        .onChange(of: items) { _, _ in filterNow() }
        .onDisappear {
            driveTask?.cancel()
            fetchTask?.cancel()
            pageGeneration += 1
            requestedThrough = 0
        }
    }

    private var summary: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 4) {
                Text(complete && filter == .all && query.isEmpty
                     ? "\(visibleItems.count) \(visibleItems.count == 1 ? "item" : "items")"
                     : "\(loadedCount) of \(total) loaded · \(visibleItems.count) match")
                    .font(.system(.subheadline, design: .monospaced))
                    .foregroundColor(Palette.muted)
                if collection.libraries.count > 1 {
                    Text(collection.libraries.map(\.name).joined(separator: ", "))
                        .font(.caption)
                        .foregroundColor(Palette.muted)
                        .lineLimit(2)
                }
            }
            Spacer()
            if filter != .all || !query.isEmpty {
                Button("Clear filters") { filter = .all; query = "" }
            }
        }
        .padding(.top, 8)
    }

    @ViewBuilder
    private var stateContent: some View {
        if loading && items.isEmpty {
            ProgressView().tint(Palette.accent)
                .frame(maxWidth: .infinity).padding(.top, 80)
        } else if let error, items.isEmpty {
            ContentUnavailableView(
                "Couldn't load \(collection.title)",
                systemImage: "exclamationmark.triangle",
                description: Text(error)
            )
            .frame(maxWidth: .infinity)
        } else if visibleItems.isEmpty {
            ContentUnavailableView(
                !complete ? "Still loading — \(loadedCount) of \(total) titles checked" :
                (filter == .all && query.isEmpty ? "This library is empty" : "No matching titles"),
                systemImage: "rectangle.stack",
                description: filter == .all && query.isEmpty ? nil : Text("Clear the title or watch filter to see more.")
            )
            .frame(maxWidth: .infinity)
        } else {
            LazyVGrid(columns: columns, alignment: .leading, spacing: 24) {
                ForEach(Array(visibleItems.enumerated()), id: \.element.id) { index, item in
                    NavigationLink(value: Route.item(item.id)) {
                        PosterCard(
                            item: item,
                            width: model.posterSize.posterWidth
                        )
                    }
                    .posterButtonStyle()
                    .onAppear {
                        Task { await fetchUntil(index + 12) }
                    }
                }
            }
        }
    }

    @ToolbarContentBuilder
    private var libraryToolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .automatic) {
            Menu {
                Picker("Sort", selection: $sort) {
                    ForEach(sorts) { option in
                        Label(option.label, systemImage: option.icon).tag(option)
                    }
                }
            } label: {
                Label("Sort", systemImage: "arrow.up.arrow.down")
            }

            Menu {
                Picker("Watch status", selection: $filter) {
                    ForEach(WatchFilter.allCases) { option in
                        Text(option.label).tag(option)
                    }
                }
            } label: {
                Label("Filter", systemImage: filter == .all ? "line.3.horizontal.decrease" : "line.3.horizontal.decrease.circle.fill")
            }
        }
    }

    @MainActor
    private func filterNow() {
        filterGeneration += 1
        let generation = filterGeneration
        let snapshot = items
        let selected = filter
        let text = query
        Task {
            let result = await Task.detached(priority: .userInitiated) {
                snapshot.filter {
                    AppModel.matches($0, filter: selected) &&
                    (text.isEmpty || $0.title.localizedCaseInsensitiveContains(text))
                }
            }.value
            if generation == filterGeneration { visibleItems = result }
        }
    }

    @MainActor
    private func runDrive() {
        driveTask?.cancel()
        guard filter != .all || !query.isEmpty else {
            // A cleared filter no longer needs a full catalogue walk. Keep
            // the initial viewport target; scrolling can request more later.
            requestedThrough = min(requestedThrough, 40)
            return
        }
        driveTask = Task { await fetchUntil(Int.max) }
    }

    @MainActor
    private func fetchUntil(_ through: Int) async {
        guard let current = pager, !current.complete, current.decided.count < through else { return }
        let generation = pageGeneration
        requestedThrough = max(requestedThrough, through)
        // Every caller waits on the same worker. A search arriving during the
        // initial 40 rows raises its target to the end of the catalogue.
        // Cancelling a superseded view task does not cancel that worker.
        while generation == pageGeneration && !Task.isCancelled {
            guard let current = pager, !current.complete,
                  current.decided.count < through, error == nil else { return }
            if fetchTask == nil {
                fetchTask = Task { await fetchPages(generation: generation) }
            }
            await fetchTask?.value
        }
    }

    @MainActor
    private func fetchPages(generation: Int) async {
        guard var current = pager else { return }
        defer {
            if generation == pageGeneration { requestedThrough = 0 }
            fetchTask = nil
        }
        do {
            while current.decided.count < requestedThrough && !current.complete &&
                    !Task.isCancelled && generation == pageGeneration {
                guard let request = current.nextRequest else { break }
                let page = try await model.libraryPage(request.libraryId, sort: current.sort, offset: request.offset)
                guard generation == pageGeneration, !Task.isCancelled else { return }
                let snapshot = current
                let batch = page.items ?? []
                current = await Task.detached(priority: .userInitiated) {
                    var revised = snapshot
                    revised.receive(libraryId: request.libraryId, items: batch, total: page.total ?? batch.count)
                    return revised
                }.value
                guard generation == pageGeneration, !Task.isCancelled else { return }
                if current.missingSortKey {
                    // Older servers do not expose the exact sort key. Keep the
                    // previous full-walk path until those servers are upgraded.
                    try await model.libraryItems(collection, sort: sort) { page in
                        guard generation == pageGeneration, !Task.isCancelled else { return }
                        items = page
                        loadedCount = page.count
                        total = page.count
                    }
                    guard generation == pageGeneration, !Task.isCancelled else { return }
                    complete = true
                    return
                }
                pager = current
                items = current.decided
                loadedCount = current.loadedCount
                total = current.total
                complete = current.complete
            }
        } catch {
            guard generation == pageGeneration, !Task.isCancelled else { return }
            self.error = AppModel.homeErrorMessage(for: error, hasCachedContent: !items.isEmpty)
        }
    }

    @MainActor
    private func load() async {
        driveTask?.cancel()
        fetchTask?.cancel()
        pageGeneration += 1
        requestedThrough = 0
        loading = true
        error = nil
        pager = LibraryMerge(libraryIds: collection.libraries.map(\.id), sort: sort)
        // A refresh preserves the prior content until the new first page lands.
        let generation = pageGeneration
        await fetchUntil(40)
        guard generation == pageGeneration, !Task.isCancelled else { return }
        loading = false
        if filter != .all || !query.isEmpty { runDrive() }
    }
}
