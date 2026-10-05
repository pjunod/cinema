import SwiftUI

struct LibraryView: View {
    @EnvironmentObject var model: AppModel
    let collection: LibraryCollection

    @StateObject private var state = LibraryGridCoordinator()
    @State private var sort: LibrarySort = .title
    @State private var filter: WatchFilter = .all
    @State private var query = ""
    @AppStorage("plurx.libraryPresentation") private var presentation = "rows"
    @State private var expandedGroup: String?
    @State private var selectedGroup: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    init(collection: LibraryCollection) {
        self.collection = collection
        _sort = State(initialValue: collection.supportsRecordedSort ? .recorded : .title)
    }

    private var rows: Bool { presentation != "grid" }

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
        ScrollViewReader { proxy in
            VStack(alignment: .leading, spacing: 12) {
                summary
                TextField("Find a title in this library", text: $query)
                    #if os(iOS)
                    .textFieldStyle(.roundedBorder)
                    #endif
                    .accessibilityLabel("Find a title in this library")
                if rows && !state.groups.isEmpty {
                    ScrollView(.horizontal) {
                        HStack(spacing: 8) {
                            ForEach(state.groups) { group in
                                Button(group.label) {
                                    selectedGroup = group.id
                                    withAnimation(reduceMotion ? nil : .default) { proxy.scrollTo(group.id, anchor: .top) }
                                }
                                .buttonStyle(.bordered)
                                .tint(selectedGroup == group.id ? Palette.accent : Palette.muted)
                                .accessibilityLabel("Jump to \(group.label)")
                                .accessibilityAddTraits(selectedGroup == group.id ? .isSelected : [])
                            }
                        }
                        .padding(.vertical, 8)
                    }
                    .accessibilityIdentifier("library-group-index")
                }
                if let error {
                    HStack {
                        Text("Incomplete library: \(error)").foregroundColor(Palette.muted)
                        Button("Retry") { Task { await state.retry() } }
                    }
                }
                ScrollView {
                    stateContent.padding(.bottom, 36)
                }
                #if os(iOS)
                .refreshable { await load() }
                #endif
            }
            .padding(.horizontal, screenHPad)
            .background(Palette.bg.ignoresSafeArea())
            .navigationTitle(collection.title)
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar { libraryToolbar }
            .task(id: loadKey) { await load() }
            .task(id: rows) { state.presentationChanged(rows: rows) }
            .task(id: query) { await state.queryChanged(query) }
            .task(id: filter) { state.filterChanged(filter) }
            .navigationDestination(item: $expandedGroup) { key in
                LibraryGroupView(state: state, groupID: key, title: state.groups.first { $0.id == key }?.label ?? key,
                                 landscape: collection.supportsRecordedSort, reload: load)
            }
            .onDisappear {
                if expandedGroup == nil { state.stop() }
                visibleIndices.removeAll()
            }
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
        } else if rows {
            LazyVStack(alignment: .leading, spacing: 28) {
                ForEach(state.groups) { group in
                    VStack(alignment: .leading, spacing: 12) {
                        HStack {
                            Text(group.label).font(.title2.bold()).accessibilityAddTraits(.isHeader)
                            Text("\(group.items.count)\(complete ? "" : " loaded")").foregroundColor(Palette.muted)
                            Spacer()
                            Button("View all") { expandedGroup = group.id }
                            .accessibilityLabel("View all \(group.label) items")
                        }
                        ScrollView(.horizontal) {
                            LazyHStack(alignment: .top, spacing: 18) {
                                ForEach(group.items) { item in
                                    NavigationLink(value: Route.item(item.id)) {
                                        if collection.supportsRecordedSort {
                                            LandscapeCard(item: item, width: model.posterSize.landscapeWidth)
                                        } else {
                                            PosterCard(item: item, width: model.posterSize.posterWidth)
                                        }
                                    }
                                    .posterButtonStyle()
                                }
                            }
                            .padding(.vertical, 12)
                        }
                        .accessibilityIdentifier("library-row-\(group.id)")
                    }
                    .id(group.id)
                }
            }
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
                Picker("View", selection: $presentation) {
                    Text("Rows").tag("rows")
                    Text("Grid").tag("grid")
                }
            } label: {
                Label("View", systemImage: rows ? "rectangle.grid.1x2" : "square.grid.2x2")
            }
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


/// Uses the same coordinator so arriving pages update the expanded group.
/// Pushing a destination retains the rows' native scroll and focus state.
private struct LibraryGroupView: View {
    @EnvironmentObject var model: AppModel
    @ObservedObject var state: LibraryGridCoordinator
    let groupID: String
    let title: String
    let landscape: Bool
    let reload: () async -> Void
    private var items: [Item] { state.groups.first { $0.id == groupID }?.items ?? [] }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text("\(items.count)\(state.complete ? " items" : " loaded · library still loading")")
                    .foregroundColor(Palette.muted)
                if let error = state.error {
                    Text("Incomplete library: \(error)")
                    Button("Retry") { Task { await state.retry() } }
                }
                LazyVGrid(columns: [GridItem(.adaptive(minimum: landscape ? model.posterSize.landscapeWidth : model.posterSize.posterWidth), spacing: 18)], spacing: 24) {
                    ForEach(items) { item in
                        NavigationLink(value: Route.item(item.id)) {
                            if landscape {
                                LandscapeCard(item: item, width: model.posterSize.landscapeWidth)
                            } else {
                                PosterCard(item: item, width: model.posterSize.posterWidth)
                            }
                        }.posterButtonStyle()
                    }
                }
            }
            .padding(.horizontal, screenHPad)
            .padding(.bottom, 36)
        }
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle(title)
        .task { await reload() }
    }
}
