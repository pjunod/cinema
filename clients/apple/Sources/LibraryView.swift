import SwiftUI

// The index ForEach also has group IDs. A row target must not resolve to
// that horizontal index before ScrollViewProxy reaches the vertical rows.
private enum LibraryScrollTarget: Hashable {
    case row(String)
}

struct LibraryView: View {
    @EnvironmentObject var model: AppModel
    @EnvironmentObject private var remoteNavigation: RemoteNavigationCoordinator
    let collection: LibraryCollection

    @StateObject private var state = LibraryGridCoordinator()
    @State private var sort: LibrarySort = .title
    @State private var filter: WatchFilter = .all
    @State private var query = ""
    @AppStorage("plurx.libraryPresentation") private var presentation = "rows"
    @State private var expandedGroup: String?
    @State private var selectedGroup: String?
    @State private var choiceMenu: String?
    @State private var searchNonce = UUID()
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
                    .remoteControl("library:search", label: "Find a title") {}
                if rows && !state.groups.isEmpty {
                    ScrollView(.horizontal) {
                        HStack(spacing: 8) {
                            ForEach(state.groups) { group in
                                Button(group.label) {
                                    selectedGroup = group.id
                                    withAnimation(reduceMotion ? nil : .default) { proxy.scrollTo(LibraryScrollTarget.row(group.id), anchor: .top) }
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
                .accessibilityIdentifier("library-results")
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
            .overlay {
                if let menu = choiceMenu {
                    RemoteChoicePanel(scope: remoteScope + ":" + menu, title: menu.capitalized, choices: choices(for: menu))
                }
            }
            .onAppear {
                updateRemoteOrder()
                remoteNavigation.setSearch(scope: remoteScope, nonce: searchNonce) { query = $0 }
            }
            .onChange(of: visibleItems.map(\.id)) { _, _ in updateRemoteOrder() }
            .onChange(of: rows) { _, _ in updateRemoteOrder() }
            .onChange(of: gridColumns) { _, _ in updateRemoteOrder() }
            .onChange(of: remoteNavigation.requestedFocus) { _, key in
                guard remoteNavigation.activeScope == remoteScope, let key, key.hasPrefix("item:"),
                      let id = Int(key.dropFirst(5)) else { return }
                if rows, let group = state.groups.first(where: { $0.items.contains(where: { $0.id == id }) }) {
                    proxy.scrollTo(LibraryScrollTarget.row(group.id), anchor: .center)
                } else { proxy.scrollTo(id, anchor: .center) }
            }
            .task(id: loadKey) { await load() }
            .task(id: rows) { state.presentationChanged(rows: rows) }
            .task(id: query) { await state.queryChanged(query) }
            .task(id: filter) { state.filterChanged(filter) }
            .navigationDestination(item: $expandedGroup) { key in
                LibraryGroupView(state: state, groupID: key, title: state.groups.first { $0.id == key }?.label ?? key,
                                 landscape: collection.supportsRecordedSort, parentScope: remoteScope, dismiss: { expandedGroup = nil }, reload: load)
            }
            .onDisappear {
                if expandedGroup == nil { state.stop() }
                visibleIndices.removeAll()
                remoteNavigation.removeSearch(scope: remoteScope, nonce: searchNonce)
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
                                .accessibilityIdentifier("library-group-heading-\(group.id)")
                            Text("\(group.items.count)\(complete ? "" : " loaded")").foregroundColor(Palette.muted)
                            Spacer()
                            Button("View all") { expandedGroup = group.id }
                            .accessibilityLabel("View all \(group.label) items")
                            .remoteControl("group:\(group.id)", label: "View all \(group.label) items") { expandedGroup = group.id }
                        }
                        ScrollViewReader { rowProxy in
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
                                    .id(item.id)
                                    .remoteControl("item:\(item.id)", label: item.title) { remoteNavigation.navigate(to: .item(item.id)) }
                                }
                            }
                            .padding(.vertical, 12)
                        }
                        .accessibilityIdentifier("library-row-\(group.id)")
                        .onChange(of: remoteNavigation.requestedFocus) { _, key in
                            guard remoteNavigation.activeScope == remoteScope, let key, key.hasPrefix("item:"), let id = Int(key.dropFirst(5)), group.items.contains(where: { $0.id == id }) else { return }
                            rowProxy.scrollTo(id, anchor: .center)
                        }
                        }
                    }
                    .id(LibraryScrollTarget.row(group.id))
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
                    .id(item.id)
                    .remoteControl("item:\(item.id)", label: item.title) { remoteNavigation.navigate(to: .item(item.id)) }
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

    private var remoteScope: String { "library:" + collection.id }
    private var remoteKeys: [String] { ["library:view", "library:sort", "library:filter", "library:search"] + (rows ? state.groups.flatMap { ["group:\($0.id)"] + $0.items.map { "item:\($0.id)" } } : visibleItems.map { "item:\($0.id)" }) }
    private func updateRemoteOrder() {
        remoteNavigation.setOrder(scope: remoteScope, keys: remoteKeys, columns: rows ? 1 : gridColumns)
    }
    private func openChoices(_ menu: String) {
        let scope = remoteScope + ":" + menu
        remoteNavigation.openModal(scope: scope, opener: "library:" + menu) { choiceMenu = nil }
        if remoteNavigation.activeScope == scope { choiceMenu = menu }
    }
    private func choices(for menu: String) -> [RemoteChoice] {
        switch menu {
        case "view": return ["rows", "grid"].map { value in
            RemoteChoice(id: value, label: value.capitalized, selected: presentation == value) { presentation = value }
        }
        case "sort": return sorts.map { option in
            RemoteChoice(id: option.rawValue, label: option.label, selected: sort == option) { sort = option }
        }
        default: return WatchFilter.allCases.map { option in
            RemoteChoice(id: option.rawValue, label: option.label, selected: filter == option) { filter = option }
        }
        }
    }
    @ToolbarContentBuilder
    private var libraryToolbar: some ToolbarContent {
        ToolbarItemGroup(placement: .automatic) {
            Button("View") { openChoices("view") }
                .remoteControl("library:view", label: "View") { openChoices("view") }
            Button("Sort") { openChoices("sort") }
                .remoteControl("library:sort", label: "Sort") { openChoices("sort") }
            Button("Filter") { openChoices("filter") }
                .remoteControl("library:filter", label: "Filter") { openChoices("filter") }
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
    @EnvironmentObject private var navigation: RemoteNavigationCoordinator
    @ObservedObject var state: LibraryGridCoordinator
    let groupID: String
    let title: String
    let landscape: Bool
    let parentScope: String
    let dismiss: () -> Void
    let reload: () async -> Void
    @State private var destinationToken = UUID()
    @State private var ownsScope = false
    private var remoteScope: String { parentScope + ":group:" + groupID }
    private var items: [Item] { state.groups.first { $0.id == groupID }?.items ?? [] }
    private func updateOrder() {
        navigation.setOrder(scope: remoteScope, keys: (state.error == nil ? [] : ["group:retry"]) + items.map { "item:\($0.id)" }, columns: 1)
    }

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    Text("\(items.count)\(state.complete ? " items" : " loaded · library still loading")")
                        .foregroundColor(Palette.muted)
                    if let error = state.error {
                        Text("Incomplete library: \(error)")
                        Button("Retry") { Task { await state.retry() } }
                            .remoteControl("group:retry", label: "Retry loading library") { Task { await state.retry() } }
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
                                .id(item.id)
                                .remoteControl("item:\(item.id)", label: item.title) { navigation.navigate(to: .item(item.id)) }
                        }
                    }
                }
                .padding(.horizontal, screenHPad)
                .padding(.bottom, 36)
            }
            .onChange(of: navigation.requestedFocus) { _, key in
                guard ownsScope, navigation.activeScope == remoteScope, let key,
                      key.hasPrefix("item:"), let id = Int(key.dropFirst(5)), items.contains(where: { $0.id == id }) else { return }
                proxy.scrollTo(id, anchor: .center)
            }
        }
        .background(Palette.bg.ignoresSafeArea())
        .remoteScope(remoteScope)
        .remoteRestricted(!ownsScope)
        .navigationTitle(title)
        .onAppear {
            ownsScope = navigation.attachDestination(token: destinationToken, parent: parentScope, scope: remoteScope, opener: "group:" + groupID, dismiss: dismiss)
            updateOrder()
        }
        .onChange(of: items.map(\.id)) { _, _ in updateOrder() }
        .onChange(of: state.error) { _, _ in updateOrder() }
        .onDisappear { navigation.detachDestination(token: destinationToken); ownsScope = false }
        .task { await reload() }
    }
}
