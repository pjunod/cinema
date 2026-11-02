import SwiftUI

// The index ForEach also has group IDs. A row target must not resolve to
// that horizontal index before ScrollViewProxy reaches the vertical rows.
private enum LibraryScrollTarget: Hashable {
    case row(String)
}

enum TVLibraryNavigation {
    static func survivingBookmark(_ id: Int, previous: [Int], current: [Int]) -> Int? {
        if current.contains(id) { return id }
        guard let index = previous.firstIndex(of: id) else { return current.first }
        let available = Set(current)
        return previous.dropFirst(index + 1).first(where: available.contains)
            ?? previous.prefix(index).reversed().first(where: available.contains)
            ?? current.first
    }
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
    @State private var contentBookmark: String?
    @State private var toolbarReturn: String?
    @State private var contentOwnedFocus = false
    @State private var preparingJump = false
    @State private var jumpGroups: [LibraryGroup] = []
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

    private var searchPrompt: Text? {
        #if os(tvOS)
        Text("Find a title in this library").foregroundColor(Palette.muted)
        #else
        nil
        #endif
    }

    private var loadKey: String { "\(collection.id):\(sort.rawValue)" }

    var body: some View {
        ScrollViewReader { proxy in
            VStack(alignment: .leading, spacing: 12) {
                #if !os(tvOS)
                libraryHeader(proxy: proxy)
                #endif
                #if os(tvOS)
                tvToolbar
                    .remoteContentReturn(to: toolbarReturn ?? contentBookmark ?? visibleItems.first.map { "item:\($0.id)" }) { toolbarReturn = nil }
                #endif
                ScrollView {
                    #if os(tvOS)
                    // However deep the viewer is, one Back returns to the
                    // toolbar. Back from the toolbar is outside this adapter
                    // and leaves the library natively.
                    VStack(alignment: .leading, spacing: 12) {
                        // Keep the preamble in the results scroll. Pinning it
                        // above this viewport leaves less height than a complete
                        // poster card, so native focus clips the artwork's top
                        // while revealing its title and metadata.
                        libraryHeader(proxy: proxy)
                        stateContent.padding(.bottom, 36)
                            .remoteContentBack(to: "library:view") {
                                // Where Back left the content; Down returns there.
                                toolbarReturn = contentBookmark
                            }
                    }
                    #else
                    stateContent.padding(.bottom, 36)
                    #endif
                }
                .accessibilityIdentifier("library-results")
                #if os(tvOS)
                // Bound the results focus section to its viewport, so titles
                // scrolled above it cannot compete with the fixed toolbar.
                .focusSection()
                #endif
                #if os(iOS)
                .refreshable { await load() }
                #endif
            }
            .padding(.horizontal, screenHPad)
            .background(Palette.bg.ignoresSafeArea())
            #if os(tvOS)
            .navigationTitle("")
            #else
            .navigationTitle(collection.title)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            #if os(iOS)
            .toolbar { libraryToolbar }
            #endif
            .remoteChoicePresentation(isPresented: Binding(get: { choiceMenu != nil }, set: { if !$0 { choiceMenu = nil } }), scope: remoteScope + ":" + (choiceMenu ?? "view")) {
                if let menu = choiceMenu {
                    RemoteChoicePanel(scope: remoteScope + ":" + menu, title: menu.capitalized, choices: choices(for: menu))
                }
            }
            .onAppear {
                updateRemoteOrder()
                remoteNavigation.setSearch(scope: remoteScope, nonce: searchNonce) { query = $0 }
            }
            .onChange(of: visibleItems.map(\.id)) { previous, current in
                updateRemoteOrder()
                guard let key = contentBookmark, let id = Int(key.dropFirst(5)),
                      !current.contains(id) else { return }
                let survivor = TVLibraryNavigation.survivingBookmark(id, previous: previous, current: current)
                contentBookmark = survivor.map { "item:\($0)" }
                if remoteNavigation.activeScope == remoteScope, contentOwnedFocus {
                    remoteNavigation.focusControl(scope: remoteScope, key: contentBookmark ?? "library:view")
                }
            }
            .onChange(of: rows) { _, _ in updateRemoteOrder() }
            .onChange(of: gridColumns) { _, _ in updateRemoteOrder() }
            .onRemoteFocusChange(remoteNavigation) {
                guard remoteNavigation.activeScope == remoteScope,
                      let key = remoteNavigation.focusedKey else { return }
                contentOwnedFocus = key.hasPrefix("item:")
                guard key.hasPrefix("item:"),
                      let id = Int(key.dropFirst(5)) else { return }
                contentBookmark = key
                selectedGroup = (rows ? state.groups : jumpGroups).first(where: { $0.items.contains(where: { $0.id == id }) })?.id
            }
            .onChange(of: remoteNavigation.focusRequestRevision) { _, _ in
                let key = remoteNavigation.requestedFocus
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

    @ViewBuilder
    private func libraryHeader(proxy: ScrollViewProxy) -> some View {
        #if os(tvOS)
        Text(collection.title).font(.title2.bold()).foregroundStyle(Palette.onBg)
            .accessibilityAddTraits(.isHeader)
        #endif
        summary
        TextField("Find a title in this library", text: $query, prompt: searchPrompt)
            #if os(tvOS)
            .foregroundStyle(Palette.onBg)
            #endif
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
                            #if os(tvOS)
                            if let item = group.items.first {
                                remoteNavigation.focusControl(scope: remoteScope, key: "item:\(item.id)")
                            }
                            #endif
                        }
                        .shelfActionButtonStyle()
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
                            #if os(tvOS)
                            .shelfActionButtonStyle()
                            #endif
                            .accessibilityLabel("View all \(group.label) items")
                            .remoteControl("group:\(group.id)", label: "View all \(group.label) items") { expandedGroup = group.id }
                        }
                        #if os(tvOS)
                        .padding(.horizontal, 24)
                        #endif
                        .tvNavigationFocusSection()
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
                                    .accessibilityIdentifier("item:\(item.id)")
                                    .remoteControl("item:\(item.id)", label: item.title) { remoteNavigation.navigate(to: .item(item.id)) }
                                }
                            }
                            #if os(tvOS)
                            // Keep the lifted first/last card and its ring inside
                            // the horizontal viewport, including large posters.
                            .padding(24)
                            #else
                            .padding(.vertical, 12)
                            #endif
                        }
                        .accessibilityIdentifier("library-row-\(group.id)")
                        .onChange(of: remoteNavigation.focusRequestRevision) { _, _ in
                            let key = remoteNavigation.requestedFocus
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
                    .accessibilityIdentifier("item:\(item.id)")
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
    private var remoteKeys: [String] { ["library:view", "library:sort", "library:filter", "library:jump", "library:search"] + (rows ? state.groups.flatMap { ["group:\($0.id)"] + $0.items.map { "item:\($0.id)" } } : visibleItems.map { "item:\($0.id)" }) }
    private func updateRemoteOrder() {
        remoteNavigation.setOrder(scope: remoteScope, keys: remoteKeys, columns: rows ? 1 : gridColumns)
    }
    private func openChoices(_ menu: String) {
        let scope = remoteScope + ":" + menu
        remoteNavigation.openModal(scope: scope, opener: "library:" + menu) { choiceMenu = nil }
        if remoteNavigation.activeScope == scope { choiceMenu = menu }
    }
    #if os(tvOS)
    /// One compact row under the title, the same size and style as the Live
    /// TV toolbar. Each control names its current value.
    private var tvToolbar: some View {
        HStack(spacing: 16) {
            ForEach(["view", "sort", "filter"], id: \.self) { menu in
                Button { openChoices(menu) } label: {
                    HStack(spacing: 10) {
                        Text(menu.capitalized)
                        Text(menu == "view" ? presentation.capitalized : menu == "sort" ? sort.label : filter.label)
                            .foregroundStyle(Palette.muted).lineLimit(1)
                    }
                }
                .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                .accessibilityIdentifier("library:" + menu)
                .remoteControl("library:" + menu, label: menu.capitalized) { openChoices(menu) }
            }
            Button { prepareJump() } label: {
                Text(preparingJump ? "Loading index…" : "Jump to…")
            }
                .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
                .disabled(visibleItems.isEmpty)
                .accessibilityIdentifier("library:jump")
                .remoteControl("library:jump", label: "Jump to group", enabled: !visibleItems.isEmpty) { prepareJump() }
            Spacer(minLength: 0)
        }
        .focusSection()
    }
    #endif
    private func prepareJump() {
        guard !preparingJump else { return }
        preparingJump = true
        let identity = "\(loadKey):\(filter):\(query)"
        Task {
            let groups = await state.groupsForJump()
            preparingJump = false
            guard remoteNavigation.activeScope == remoteScope,
                  identity == "\(loadKey):\(filter):\(query)" else { return }
            #if os(tvOS)
            // Loading must not disable the focused opener or present a late
            // menu after the viewer has moved to another control.
            guard remoteNavigation.focusedKey == "library:jump" else { return }
            #endif
            jumpGroups = groups
            updateRemoteOrder()
            openChoices("jump")
        }
    }
    private func choices(for menu: String) -> [RemoteChoice] {
        switch menu {
        case "jump": return jumpGroups.compactMap { group in
            guard let item = group.items.first else { return nil }
            return RemoteChoice(id: group.id, label: group.label, selected: selectedGroup == group.id, returnFocus: "item:\(item.id)") {
                selectedGroup = group.id
                contentBookmark = "item:\(item.id)"
            }
        }
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
    #if os(iOS)
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
    #endif

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
    @State private var bookmark: Int?
    @State private var toolsOwnedFocus = false
    @State private var contentOwnedFocus = false
    @State private var returningToTools = false
    private var remoteScope: String { parentScope + ":group:" + groupID }
    private var items: [Item] { state.groups.first { $0.id == groupID }?.items ?? [] }
    private func updateOrder() {
        var keys: [String] = []
        #if os(tvOS)
        keys.append("group:tools")
        #endif
        keys += (state.error == nil ? [] : ["group:retry"]) + items.map { "item:\($0.id)" }
        navigation.setOrder(scope: remoteScope, keys: keys, columns: 1)
    }

    var body: some View {
        ScrollViewReader { proxy in
            HStack(alignment: .top, spacing: 32) {
                #if os(tvOS)
                VStack(alignment: .leading, spacing: 22) {
                    Text("Library tools").font(.headline).foregroundStyle(Palette.muted)
                    Button("Library tools") { returningToTools = true; dismiss() }
                        .buttonStyle(TVReadableButtonStyle(prominent: false))
                        .accessibilityIdentifier("group:tools")
                        .remoteControl("group:tools", label: "Library tools") { returningToTools = true; dismiss() }
                    Text("Left reaches tools. Right returns to your title.").font(.caption).foregroundStyle(Palette.muted)
                    Spacer(minLength: 0)
                }
                .frame(width: 240)
                .frame(maxHeight: .infinity, alignment: .top)
                .focusSection()
                #endif
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    #if os(tvOS)
                    Text(title).font(.title2.bold()).foregroundStyle(Palette.onBg)
                        .accessibilityAddTraits(.isHeader)
                    #endif
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
                                .accessibilityIdentifier("item:\(item.id)")
                                .remoteControl("item:\(item.id)", label: item.title) { navigation.navigate(to: .item(item.id)) }
                        }
                    }
                }
                .padding(.horizontal, screenHPad)
                .padding(.bottom, 36)
            }
            }
            #if os(tvOS)
            .padding(.horizontal, screenHPad)
            #endif
            .onChange(of: navigation.focusRequestRevision) { _, _ in
                let key = navigation.requestedFocus
                guard ownsScope, navigation.activeScope == remoteScope, let key,
                      key.hasPrefix("item:"), let id = Int(key.dropFirst(5)), items.contains(where: { $0.id == id }) else { return }
                proxy.scrollTo(id, anchor: .center)
            }
        }
        .background(Palette.bg.ignoresSafeArea())
        .remoteScope(remoteScope)
        .remoteRestricted(!ownsScope)
        #if os(tvOS)
        .navigationTitle("")
        #else
        .navigationTitle(title)
        #endif
        .onAppear {
            ownsScope = navigation.attachDestination(token: destinationToken, parent: parentScope, scope: remoteScope, opener: "group:" + groupID, dismiss: dismiss)
            updateOrder()
        }
        .onChange(of: items.map(\.id)) { previous, current in
            updateOrder()
            #if os(tvOS)
            guard let saved = bookmark else { return }
            bookmark = TVLibraryNavigation.survivingBookmark(saved, previous: previous, current: current)
            if navigation.activeScope == remoteScope, contentOwnedFocus, !current.contains(saved) {
                navigation.focusControl(scope: remoteScope, key: bookmark.map { "item:\($0)" } ?? "group:tools")
            }
            #endif
        }
        .onRemoteFocusChange(navigation) {
            guard navigation.activeScope == remoteScope, let key = navigation.focusedKey else { return }
            contentOwnedFocus = key.hasPrefix("item:")
            if key == "group:tools" { toolsOwnedFocus = true }
            guard key.hasPrefix("item:"), let id = Int(key.dropFirst(5)) else { return }
            if toolsOwnedFocus {
                toolsOwnedFocus = false
                if let saved = bookmark, saved != id, items.contains(where: { $0.id == saved }) {
                    navigation.focusControl(scope: remoteScope, key: "item:\(saved)")
                    return
                }
            }
            bookmark = id
        }
        .onChange(of: state.error) { _, _ in updateOrder() }
        .onDisappear {
            navigation.detachDestination(token: destinationToken)
            ownsScope = false
            if returningToTools { navigation.focusControl(scope: parentScope, key: "library:view") }
        }
        .task { await reload() }
    }
}
