import SwiftUI

struct LibraryView: View {
    @EnvironmentObject var model: AppModel
    let collection: LibraryCollection

    @StateObject private var state = LibraryGridCoordinator()
    @State private var sort: LibrarySort = .title
    @State private var filter: WatchFilter = .all
    @State private var query = ""

    private var items: [Item] { state.items }
    private var visibleItems: [Item] { state.visibleItems }
    private var loading: Bool { state.loading }
    private var error: String? { state.error }
    private var loadedCount: Int { state.loadedCount }
    private var total: Int { state.total }
    private var complete: Bool { state.complete }
    @State private var gridColumns = 1
    @State private var visibleIndices: Set<Int> = []


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
        .task(id: query) { await state.queryChanged(query) }
        .task(id: filter) { state.filterChanged(filter) }
        .onDisappear {
            state.stop()
            visibleIndices.removeAll()
        }
    }

    private var summary: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 4) {
                Text(state.summary)
                    .font(.system(.subheadline, design: .monospaced))
                    .foregroundColor(Palette.muted)
                    .accessibilityIdentifier("library-loaded-summary")
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
                        visibleIndices.insert(index)
                        Task { await state.fetchUntil(LibraryGridPrefetch.exclusiveCount(lastVisibleIndex: index, columns: gridColumns)) }
                    }
                    .onDisappear { visibleIndices.remove(index) }
                }
            }
            .onGeometryChange(for: Int.self) { geometry in
                LibraryGridPrefetch.columns(width: Double(geometry.size.width),
                                            minimumWidth: Double(model.posterSize.posterWidth), spacing: 18)
            } action: { columns in
                gridColumns = columns
                if let last = visibleIndices.max() {
                    Task { await state.fetchUntil(LibraryGridPrefetch.exclusiveCount(lastVisibleIndex: last, columns: columns)) }
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
    private func load() async {
        let apiModel = model
        let selectedCollection = collection
        state.configure(
            fetch: { id, sort, offset in try await apiModel.libraryPage(id, sort: sort, offset: offset) },
            legacy: { sort, publish in try await apiModel.libraryItems(selectedCollection, sort: sort, publish: publish) }
        )
        await state.load(libraryIds: collection.libraries.map(\.id), sort: sort)
    }
}
