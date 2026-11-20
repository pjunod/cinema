import SwiftUI

// Custom tags (docs/features/CUSTOM-TAGS-CLIENTS.md): the reader's Home order
// and custom rows, and the tag chips + checklist on a title's detail screen.

/// A tag the reader can see. Unknown JSON keys are ignored.
struct PlurxTag: Codable, Identifiable, Hashable {
    let id: Int
    var name: String
    var scope: String = "household"
    var color: String? = nil
    var archived: Bool = false
    var itemCount: Int? = nil
    var editable: Bool = false

    init(id: Int, name: String, scope: String = "household", color: String? = nil,
         archived: Bool = false, itemCount: Int? = nil, editable: Bool = false) {
        self.id = id; self.name = name; self.scope = scope; self.color = color
        self.archived = archived; self.itemCount = itemCount; self.editable = editable
    }

    private enum CodingKeys: String, CodingKey { case id, name, scope, color, archived, itemCount, editable }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = try c.decode(Int.self, forKey: .id)
        name = try c.decode(String.self, forKey: .name)
        scope = (try? c.decode(String.self, forKey: .scope)) ?? "household"
        color = try? c.decode(String.self, forKey: .color)
        archived = (try? c.decode(Bool.self, forKey: .archived)) ?? false
        itemCount = try? c.decode(Int.self, forKey: .itemCount)
        editable = (try? c.decode(Bool.self, forKey: .editable)) ?? false
    }

    var isPrivate: Bool { scope == "private" }

    /// The colour names the server hands out, as `#rrggbb`.
    static let colorHex: [String: UInt32] = [
        "violet": 0xa78bfa, "blue": 0x60a5fa, "teal": 0x2dd4bf, "green": 0x4ade80,
        "lime": 0xa3e635, "amber": 0xfbbf24, "orange": 0xfb923c, "red": 0xf87171,
        "pink": 0xf472b6, "slate": 0x94a3b8,
    ]
    var rgb: UInt32 { Self.colorHex[color ?? ""] ?? Self.colorHex["slate"]! }
    var swiftUIColor: Color {
        Color(red: Double((rgb >> 16) & 0xff) / 255, green: Double((rgb >> 8) & 0xff) / 255,
              blue: Double(rgb & 0xff) / 255)
    }
}

struct TagListResponse: Decodable { var tags: [PlurxTag] = [] }

struct HomeLayoutEntry: Decodable, Hashable {
    let key: String
    var kind: String? = nil
    var rowId: Int? = nil
}

struct HomeRowBody: Decodable {
    var title: String = ""
    var total: Int = 0
    var items: [Item] = []
    private enum CodingKeys: String, CodingKey { case title, total, items }
    init(title: String = "", total: Int = 0, items: [Item] = []) {
        self.title = title; self.total = total; self.items = items
    }
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        title = (try? c.decode(String.self, forKey: .title)) ?? ""
        total = (try? c.decode(Int.self, forKey: .total)) ?? 0
        items = (try? c.decode([Item].self, forKey: .items)) ?? []
    }
}

struct HomeRows: Decodable {
    var layout: [HomeLayoutEntry] = []
    var rows: [String: HomeRowBody] = [:]

    private static let decoder: JSONDecoder = {
        let d = JSONDecoder()
        d.keyDecodingStrategy = .convertFromSnakeCase
        return d
    }()
    static func decode(_ data: Data) throws -> HomeRows { try decoder.decode(HomeRows.self, from: data) }
}

struct RowItemsPage: Decodable {
    var items: [Item] = []
    var total: Int = 0
    var offset: Int = 0
    var limit: Int = 0
}

/// One Home section in the order the reader chose.
enum HomeSection: Hashable {
    case continueWatching, nextUp, watchlist, favorites, recentlyAdded, comingSoon
    case row(Int)
}

enum HomeSectionOrder {
    /// The order Home has always drawn.
    static let standard: [HomeSection] = [
        .continueWatching, .nextUp, .watchlist, .favorites, .recentlyAdded, .comingSoon,
    ]

    /// Sections to draw. `nil` (no answer, 404, or an error) is today's Home.
    /// Built-ins the app does not draw (`libraries`) and unknown keys are
    /// ignored; built-ins missing from `layout` are hidden; a `row:N` entry
    /// whose body is absent is dropped.
    static func sections(rows: HomeRows?) -> [HomeSection] {
        guard let rows else { return standard }
        var out: [HomeSection] = []
        for entry in rows.layout {
            switch entry.key {
            case "continue_watching": out.append(.continueWatching)
            case "next_up": out.append(.nextUp)
            case "watchlist": out.append(.watchlist)
            case "favorites": out.append(.favorites)
            case "recently_added": out.append(.recentlyAdded)
            case "coming_soon": out.append(.comingSoon)
            default:
                guard entry.key.hasPrefix("row:"),
                      let id = entry.rowId ?? Int(entry.key.dropFirst(4)),
                      rows.rows[String(id)] != nil else { continue }
                out.append(.row(id))
            }
        }
        return out
    }
}

enum TagSelection {
    /// The ids a PUT carries: every ticked tag the reader may edit, in the
    /// checklist's order. The server leaves tags the reader cannot edit alone.
    /// Every ticked editable tag, archived ones included: the server removes
    /// every editable tag the body leaves out, so dropping one strips it.
    /// `current` (the title's own tags) counts even when the catalogue failed.
    static func putIds(tags: [PlurxTag], ticked: Set<Int>, current: [PlurxTag] = []) -> [Int] {
        let merged = tags + current.filter { c in !tags.contains { $0.id == c.id } }
        return merged.filter { $0.editable && ticked.contains($0.id) }.map(\.id).sorted()
    }
    /// Tags the checklist lists.
    static func choosable(_ tags: [PlurxTag]) -> [PlurxTag] { tags.filter { $0.editable && !$0.archived } }
}

struct SetTagsBody: Encodable { let tagIds: [Int] }
private struct CreateTagBody: Encodable { let name: String; let scope: String }

extension PlurxAPI {
    func homeRows() async throws -> HomeRows { try await getTagsJSON("home/rows") }
    func rowItems(_ id: Int, offset: Int = 0, limit: Int = 60) async throws -> RowItemsPage {
        try await getTagsJSON("rows/\(id)/items", query: [
            URLQueryItem(name: "offset", value: String(offset)),
            URLQueryItem(name: "limit", value: String(limit)),
        ])
    }
    func itemTags(_ id: Int) async throws -> [PlurxTag] {
        (try await getTagsJSON("items/\(id)/tags") as TagListResponse).tags
    }
    func allTags() async throws -> [PlurxTag] {
        (try await getTagsJSON("tags") as TagListResponse).tags
    }
    func setItemTags(_ id: Int, tagIds: [Int]) async throws -> [PlurxTag] {
        (try await putTagsJSON("items/\(id)/tags", body: SetTagsBody(tagIds: tagIds)) as TagListResponse).tags
    }
    func createTag(name: String) async throws -> PlurxTag {
        try await postTagsJSON("tags", body: CreateTagBody(name: name, scope: "private"))
    }
    /// The PUT body, for tests.
    static func setTagsBody(_ ids: [Int]) throws -> Data {
        let e = JSONEncoder(); e.keyEncodingStrategy = .convertToSnakeCase
        return try e.encode(SetTagsBody(tagIds: ids))
    }
}

// MARK: - Detail chips and checklist

/// Chips under a movie's or show's title, and the "Tags…" action.
struct ItemTagsSection: View {
    @EnvironmentObject private var model: AppModel
    let itemId: Int
    @State private var tags: [PlurxTag] = []
    @State private var editing = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if !tags.isEmpty {
                HStack(spacing: 8) {
                    ForEach(tags) { tag in TagChip(tag: tag) }
                }
            }
            Button { editing = true } label: {
                Label("Tags…", systemImage: "tag")
                    #if os(tvOS)
                    .font(.callout)
                    #else
                    .font(.footnote.weight(.semibold))
                    #endif
            }
            #if os(tvOS)
            .buttonStyle(TVReadableButtonStyle(prominent: false, compact: true))
            #else
            .buttonStyle(.bordered)
            .tint(Palette.accent)
            #endif
            .accessibilityIdentifier("item-tags-button")
        }
        .task(id: itemId) {
            if let loaded = try? await model.requireAPI().itemTags(itemId) { tags = loaded }
        }
        .sheet(isPresented: $editing) {
            TagChecklist(itemId: itemId, current: tags) { tags = $0 }
                .environmentObject(model)
        }
    }
}

struct TagChip: View {
    let tag: PlurxTag
    var body: some View {
        HStack(spacing: 4) {
            if tag.isPrivate { Image(systemName: "lock.fill").imageScale(.small) }
            Text(tag.name).lineLimit(1)
        }
        #if os(tvOS)
        .font(.callout.weight(.semibold))
        #else
        .font(.caption.weight(.semibold))
        #endif
        .foregroundStyle(tag.swiftUIColor)
        .padding(.horizontal, 10).padding(.vertical, 4)
        .background(tag.swiftUIColor.opacity(0.16), in: Capsule())
        .overlay(Capsule().stroke(tag.swiftUIColor.opacity(0.5), lineWidth: 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(tag.isPrivate ? "\(tag.name), private tag" : tag.name)
    }
}

struct TagChecklist: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let itemId: Int
    let current: [PlurxTag]
    let onChange: ([PlurxTag]) -> Void
    @State private var all: [PlurxTag] = []
    @State private var ticked: Set<Int> = []
    @State private var busy = false
    @State private var message: String?
    @State private var newName = ""
    @State private var loaded = false

    private var choices: [PlurxTag] { TagSelection.choosable(all) }

    var body: some View {
        NavigationStack {
            List {
                if let message {
                    Text(message).foregroundStyle(Palette.accent)
                }
                ForEach(choices) { tag in
                    Button { toggle(tag) } label: {
                        HStack {
                            Image(systemName: ticked.contains(tag.id) ? "checkmark.circle.fill" : "circle")
                                .foregroundStyle(tag.swiftUIColor)
                            Text(tag.name)
                            if tag.isPrivate { Image(systemName: "lock.fill").foregroundStyle(Palette.muted) }
                            Spacer()
                        }
                    }
                    .disabled(busy)
                    .accessibilityAddTraits(ticked.contains(tag.id) ? .isSelected : [])
                }
                if loaded && choices.isEmpty {
                    Text("No tags you can edit yet.").foregroundStyle(Palette.muted)
                }
                #if os(iOS)
                Section("New tag") {
                    HStack {
                        TextField("New tag", text: $newName)
                            .textInputAutocapitalization(.never)
                            .submitLabel(.done)
                            .onSubmit { createTag() }
                        Button("Add") { createTag() }
                            .disabled(busy || newName.trimmingCharacters(in: .whitespaces).isEmpty)
                    }
                }
                #endif
            }
            .navigationTitle("Tags")
            #if os(iOS)
            .navigationBarTitleDisplayMode(.inline)
            #endif
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .task {
            ticked = Set(current.map(\.id))
            if let list = try? await model.requireAPI().allTags() { all = list }
            loaded = true
        }
    }

    private func toggle(_ tag: PlurxTag) {
        var next = ticked
        if next.contains(tag.id) { next.remove(tag.id) } else { next.insert(tag.id) }
        save(next)
    }

    private func save(_ next: Set<Int>) {
        let ids = TagSelection.putIds(tags: all, ticked: next, current: current)
        busy = true; message = nil
        Task {
            defer { busy = false }
            do {
                let result = try await model.requireAPI().setItemTags(itemId, tagIds: ids)
                ticked = Set(result.map(\.id)); onChange(result)
            } catch {
                if case APIError.http(403) = error { message = "You can't change that tag" }
                else { message = error.localizedDescription }
                // Redraw from the server's truth.
                if let fresh = try? await model.requireAPI().itemTags(itemId) {
                    ticked = Set(fresh.map(\.id)); onChange(fresh)
                }
            }
        }
    }

    private func createTag() {
        let name = newName.trimmingCharacters(in: .whitespaces)
        guard !name.isEmpty else { return }
        busy = true; message = nil
        Task {
            let api = model.requireAPI()
            var tag: PlurxTag?
            do { tag = try await api.createTag(name: name) }
            catch APIError.conflict {
                tag = (try? await api.allTags())?.first { $0.name.caseInsensitiveCompare(name) == .orderedSame }
            } catch { message = error.localizedDescription }
            if let list = try? await api.allTags() { all = list }
            busy = false
            if let tag {
                newName = ""
                if !all.contains(where: { $0.id == tag.id }) { all.append(tag) }
                var next = ticked; next.insert(tag.id)
                save(next)
            }
        }
    }
}

// MARK: - Home custom rows

/// A custom row's poster rail, ending in a "See all" tile when it holds more.
struct CustomRowRail: View {
    @EnvironmentObject private var model: AppModel
    let rowId: Int
    let body_: HomeRowBody

    var body: some View {
        if !body_.items.isEmpty {
            VStack(alignment: .leading, spacing: 12) {
                Text(body_.title)
                    #if os(tvOS)
                    .font(.title3.weight(.semibold))
                    #else
                    .font(.headline.weight(.semibold))
                    #endif
                    .foregroundColor(Palette.onBg)
                    .padding(.horizontal, screenHPad)
                ScrollView(.horizontal, showsIndicators: false) {
                    LazyHStack(alignment: .top, spacing: spacing) {
                        ForEach(body_.items) { item in
                            NavigationLink(value: Route.item(item.id)) {
                                PosterCard(item: item, width: model.posterSize.posterWidth)
                            }
                            .posterButtonStyle()
                        }
                        if body_.total > body_.items.count {
                            NavigationLink {
                                CustomRowGrid(rowId: rowId, title: body_.title)
                            } label: {
                                seeAllTile
                            }
                            .posterButtonStyle()
                            .accessibilityIdentifier("custom-row-see-all-\(rowId)")
                        }
                    }
                    .padding(.horizontal, screenHPad)
                }
            }
            .padding(.vertical, 10)
            .tvNavigationFocusSection()
            .accessibilityIdentifier("custom-row-\(rowId)")
        }
    }

    private var spacing: CGFloat {
        #if os(tvOS)
        return 24
        #else
        return 16
        #endif
    }

    private var seeAllTile: some View {
        let width = model.posterSize.posterWidth
        return VStack(spacing: 8) {
            Image(systemName: "chevron.right.circle").font(.largeTitle)
            Text("See all").font(.headline)
            Text("\(body_.total) titles").font(.caption).foregroundStyle(Palette.muted)
        }
        .foregroundColor(Palette.onBg)
        .frame(width: width, height: width * 1.5)
        .background(Palette.surface, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

struct CustomRowGrid: View {
    @EnvironmentObject private var model: AppModel
    let rowId: Int
    let title: String
    @State private var items: [Item] = []
    @State private var total = 0
    @State private var loading = false
    @State private var failed = false

    var body: some View {
        ScrollView {
            LazyVGrid(columns: [GridItem(.adaptive(minimum: model.posterSize.posterWidth), spacing: 16, alignment: .top)],
                      alignment: .leading, spacing: 20) {
                ForEach(items) { item in
                    NavigationLink(value: Route.item(item.id)) {
                        PosterCard(item: item, width: model.posterSize.posterWidth)
                    }
                    .posterButtonStyle()
                    .onAppear { if item.id == items.last?.id { Task { await loadMore() } } }
                }
            }
            .padding(.horizontal, screenHPad)
            if loading { ProgressView().padding() }
            if failed { Text("Couldn't load this row").foregroundStyle(Palette.muted).padding() }
        }
        .background(Palette.bg.ignoresSafeArea())
        .navigationTitle(title)
        .task { if items.isEmpty { await loadMore() } }
    }

    private func loadMore() async {
        guard !loading, items.isEmpty || items.count < total else { return }
        loading = true; defer { loading = false }
        do {
            let page = try await model.requireAPI().rowItems(rowId, offset: items.count, limit: 60)
            total = page.total
            let known = Set(items.map(\.id))
            items += page.items.filter { !known.contains($0.id) }
            failed = false
        } catch { failed = true }
    }
}
