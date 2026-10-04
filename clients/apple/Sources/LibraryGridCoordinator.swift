import Foundation
import Combine

/// Owns the library grid's existing paging and immutable filter work. The view
/// supplies its API, while tests can control real page and worker completion.
@MainActor
final class LibraryGridCoordinator: ObservableObject {
    typealias Fetch = (Int, LibrarySort, Int) async throws -> Page
    typealias Legacy = (LibrarySort, ([Item]) -> Void) async throws -> Void
    typealias FilterWorker = @Sendable ([Item], WatchFilter, String) async -> [Item]
    typealias Delay = @Sendable (Duration) async throws -> Void

    @Published private(set) var items: [Item] = []
    @Published private(set) var visibleItems: [Item] = []
    @Published private(set) var loading = true
    @Published private(set) var error: String?
    @Published private(set) var loadedCount = 0
    @Published private(set) var total = 0
    @Published private(set) var complete = false
    private var sort: LibrarySort = .title
    @Published private var filter: WatchFilter = .all
    @Published private var query = ""
    private var pager: LibraryMerge?
    private var fetchTask: Task<Void, Never>?
    private var driveTask: Task<Void, Never>?
    private var requestedThrough = 0
    private var filterGeneration = 0
    private var queryGeneration = 0
    private var pageGeneration = 0
    private var filterTasks: [Int: Task<Void, Never>] = [:]
    private var fetch: Fetch
    private var legacy: Legacy
    private let worker: FilterWorker
    private let delay: Delay

    init(fetch: @escaping Fetch = { _, _, _ in Page(items: [], total: 0) },
         legacy: @escaping Legacy = { _, publish in publish([]) },
         worker: @escaping FilterWorker = { await LibraryGridCoordinator.filterSnapshot($0, $1, $2) },
         delay: @escaping Delay = { try await Task.sleep(for: $0) }) {
        self.fetch = fetch
        self.legacy = legacy
        self.worker = worker
        self.delay = delay
    }

    func configure(fetch: @escaping Fetch, legacy: @escaping Legacy) {
        self.fetch = fetch
        self.legacy = legacy
    }

    var pendingFilterCount: Int { filterTasks.count }

    var summary: String {
        complete && filter == .all && query.isEmpty
            ? "\(visibleItems.count) \(visibleItems.count == 1 ? "item" : "items")"
            : "\(loadedCount) of \(total) loaded · \(visibleItems.count) match"
    }

    nonisolated static func filterSnapshot(_ snapshot: [Item], _ selected: WatchFilter, _ text: String) async -> [Item] {
        await Task.detached(priority: .userInitiated) {
            snapshot.filter {
                AppModel.matches($0, filter: selected) &&
                (text.isEmpty || $0.title.localizedCaseInsensitiveContains(text))
            }
        }.value
    }

    /// SwiftUI's query-keyed task cancels a previous call during its150ms wait.
    /// The generation also fences an injected/non-cooperative delay completion.
    func queryChanged(_ text: String) async {
        guard !Task.isCancelled else { return }
        query = text
        queryGeneration += 1
        let generation = queryGeneration
        do { try await delay(.milliseconds(150)) } catch { return }
        guard generation == queryGeneration, !Task.isCancelled else { return }
        filterNow()
        runDrive()
    }

    func filterChanged(_ selected: WatchFilter) {
        filter = selected
        filterNow()
        runDrive()
    }

    private func filterNow() {
        filterGeneration += 1
        let generation = filterGeneration
        let snapshot = items
        let selected = filter
        let text = query
        let worker = worker
        filterTasks[generation] = Task { [weak self] in
            let result = await worker(snapshot, selected, text)
            guard let self else { return }
            self.filterTasks[generation] = nil
            if generation == self.filterGeneration, !Task.isCancelled { self.visibleItems = result }
        }
    }

    func stop() {
        driveTask?.cancel()
        fetchTask?.cancel()
        pageGeneration += 1
        queryGeneration += 1
        filterGeneration += 1
        requestedThrough = 0
        for task in filterTasks.values { task.cancel() }
        filterTasks.removeAll()
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
    func fetchUntil(_ through: Int) async {
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
                let page = try await fetch(request.libraryId, current.sort, request.offset)
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
                    try await legacy(sort) { page in
                        guard generation == pageGeneration, !Task.isCancelled else { return }
                        items = page
                        filterNow()
                        loadedCount = page.count
                        total = page.count
                    }
                    guard generation == pageGeneration, !Task.isCancelled else { return }
                    complete = true
                    return
                }
                pager = current
                items = current.decided
                filterNow()
                loadedCount = current.loadedCount
                total = current.total
                complete = current.complete
            }
        } catch {
            guard generation == pageGeneration, !Task.isCancelled else { return }
            self.error = AppModel.homeErrorMessage(for: error, hasCachedContent: !items.isEmpty)
        }
    }

    func load(libraryIds: [Int], sort: LibrarySort) async {
        driveTask?.cancel()
        fetchTask?.cancel()
        pageGeneration += 1
        requestedThrough = 0
        loading = true
        error = nil
        self.sort = sort
        pager = LibraryMerge(libraryIds: libraryIds, sort: sort)
        // A refresh preserves the prior content until the new first page lands.
        let generation = pageGeneration
        await fetchUntil(40)
        guard generation == pageGeneration, !Task.isCancelled else { return }
        loading = false
        if filter != .all || !query.isEmpty { runDrive() }
    }
}
