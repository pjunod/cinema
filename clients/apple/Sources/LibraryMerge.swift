import Foundation

/// The server's total order across individually paged libraries. A buffered
/// head from every unfinished cursor is required before any item is decided.
struct LibraryMerge {
    struct Cursor {
        let libraryId: Int
        var offset = 0
        var total = 0
        var buffer: [Item] = []
        var exhausted = false
    }

    let sort: LibrarySort
    private(set) var cursors: [Cursor]
    private(set) var decided: [Item] = []
    private(set) var missingSortKey = false

    init(libraryIds: [Int], sort: LibrarySort) {
        self.sort = sort
        cursors = libraryIds.map { Cursor(libraryId: $0) }
    }

    var loadedCount: Int { cursors.reduce(0) { $0 + $1.offset } }
    var total: Int { cursors.reduce(0) { $0 + $1.total } }
    var complete: Bool { cursors.allSatisfy { $0.exhausted && $0.buffer.isEmpty } }

    var nextRequest: (libraryId: Int, offset: Int)? {
        guard let cursor = cursors.first(where: { !$0.exhausted && $0.buffer.isEmpty }) else { return nil }
        return (cursor.libraryId, cursor.offset)
    }

    mutating func receive(libraryId: Int, items: [Item], total: Int, limit: Int = 200) {
        guard let index = cursors.firstIndex(where: { $0.libraryId == libraryId }) else { return }
        missingSortKey = missingSortKey || items.contains { $0.sortTitle == nil }
        cursors[index].buffer.append(contentsOf: items)
        cursors[index].offset += items.count
        cursors[index].total = total
        cursors[index].exhausted = items.isEmpty || cursors[index].offset >= total || items.count < limit
        drainDecidable()
    }

    mutating func drainDecidable() {
        while nextRequest == nil {
            let heads = cursors.indices.filter { !cursors[$0].buffer.isEmpty }
            guard let first = heads.first else { return }
            let selected = heads.dropFirst().reduce(first) { best, candidate in
                Self.precedes(cursors[candidate].buffer[0], cursors[best].buffer[0], sort: sort) ? candidate : best
            }
            decided.append(cursors[selected].buffer.removeFirst())
        }
    }

    static func precedes(_ lhs: Item, _ rhs: Item, sort: LibrarySort) -> Bool {
        func title() -> Bool? {
            let a = Array((lhs.sortTitle ?? "").utf8)
            let b = Array((rhs.sortTitle ?? "").utf8)
            if a == b { return nil }
            return a.lexicographicallyPrecedes(b)
        }
        func descending<T: Comparable>(_ a: T?, _ b: T?) -> Bool? {
            if a == b { return nil }
            guard let a else { return false }
            guard let b else { return true }
            return a > b
        }
        switch sort {
        case .title:
            if let order = title() { return order }
        case .added:
            if let order = descending(lhs.addedAt, rhs.addedAt) { return order }
            return lhs.id > rhs.id
        case .year:
            if let order = descending(lhs.year, rhs.year) { return order }
            if let order = title() { return order }
        case .resolution:
            if let order = descending(lhs.resolution ?? -1, rhs.resolution ?? -1) { return order }
            if let order = title() { return order }
        case .recorded:
            if let order = descending(lhs.recordedAt, rhs.recordedAt) { return order }
            if let order = title() { return order }
        }
        return lhs.id < rhs.id
    }
}
