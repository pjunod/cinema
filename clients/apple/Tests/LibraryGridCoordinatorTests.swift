import Foundation
import XCTest
@testable import plurx

private actor LibraryDelayGate {
    private var sleeps: [CheckedContinuation<Void, Never>] = []
    private var observers: [(Int, CheckedContinuation<Void, Never>)] = []
    private(set) var durations: [Duration] = []
    func sleep(_ duration: Duration) async {
        durations.append(duration)
        await withCheckedContinuation { continuation in
            sleeps.append(continuation)
            let ready = observers.filter { durations.count >= $0.0 }
            observers.removeAll { durations.count >= $0.0 }
            ready.forEach { $0.1.resume() }
        }
    }
    func waitForSleeps(_ count: Int) async {
        if durations.count >= count { return }
        await withCheckedContinuation { observers.append((count, $0)) }
    }
    func releaseAll() {
        let pending = sleeps
        sleeps.removeAll()
        pending.forEach { $0.resume() }
    }
}
private actor LibraryWorkerGate {
    private var pending: [String: CheckedContinuation<Void, Never>] = [:]
    private var observers: [String: CheckedContinuation<Void, Never>] = [:]
    func block(_ query: String) async {
        await withCheckedContinuation { continuation in
            pending[query] = continuation
            observers.removeValue(forKey: query)?.resume()
        }
    }
    func waitFor(_ query: String) async {
        if pending[query] != nil { return }
        await withCheckedContinuation { observers[query] = $0 }
    }
    func release(_ query: String) { pending.removeValue(forKey: query)?.resume() }
}
private actor LibraryQueryRecorder {
    private(set) var queries: [String] = []
    func append(_ query: String) { queries.append(query) }
    func clear() { queries.removeAll() }
}

@MainActor
final class LibraryGridCoordinatorTests: XCTestCase {
    private func items(_ count: Int) throws -> [Item] {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try (1...count).map { id in
            let row: [String: Any] = ["id": id, "kind": "movie", "title": id == count ? "Zero Harbor" : "Title \(id)",
                                     "sort_title": String(format: "title %04d", id)]
            return try decoder.decode(Item.self, from: JSONSerialization.data(withJSONObject: row))
        }
    }
    private func waitUntil(_ condition: @escaping @MainActor () -> Bool) async {
        let done = expectation(description: "Production coordinator published expected state")
        let observer = Task { @MainActor in
            while !Task.isCancelled {
                if condition() { done.fulfill(); return }
                await Task.yield()
            }
        }
        await fulfillment(of: [done], timeout: 3)
        observer.cancel()
        await observer.value
    }
    func testQueryDrivesActualPagerToCompletionAndSummaryReportsProgress() async throws {
        let rows = try items(600)
        let secondPage = LibraryWorkerGate()
        var offsets: [Int] = []
        let state = LibraryGridCoordinator(fetch: { _, _, offset in
            offsets.append(offset)
            if offset == 200 { await secondPage.block("page") }
            return Page(items: Array(rows.dropFirst(offset).prefix(200)), total: rows.count)
        }, delay: { _ in })
        defer { state.stop() }
        await state.load(libraryIds: [1], sort: .title)
        XCTAssertEqual(offsets, [0])
        XCTAssertEqual(state.loadedCount, 200)
        await state.queryChanged("Zero Harbor")
        await secondPage.waitFor("page")
        await waitUntil { state.visibleItems.isEmpty }
        XCTAssertEqual(state.summary, "200 of 600 loaded · 0 match")
        XCTAssertFalse(state.complete, "An unloaded match must not be declared absent")
        await secondPage.release("page")
        await waitUntil { state.complete && state.visibleItems.map(\.id) == [600] }
        XCTAssertEqual(offsets, [0, 200, 400])
        XCTAssertEqual(state.summary, "600 of 600 loaded · 1 match")
        await state.queryChanged("")
        await waitUntil { state.visibleItems.count == 600 }
        XCTAssertEqual(state.summary, "600 items")
        XCTAssertEqual(offsets, [0, 200, 400], "Clearing keeps loaded rows without another fetch")
    }
    func testLateFilterResultCannotReplaceNewerQuerySnapshot() async throws {
        let rows = try items(2)
        let gate = LibraryWorkerGate()
        let state = LibraryGridCoordinator(fetch: { _, _, _ in Page(items: rows, total: rows.count) }, worker: { snapshot, filter, query in
            if query == "Title" { await gate.block(query) }
            return await LibraryGridCoordinator.filterSnapshot(snapshot, filter, query)
        }, delay: { _ in })
        defer { state.stop() }
        await state.load(libraryIds: [1], sort: .title)
        await state.queryChanged("Title")
        await gate.waitFor("Title")
        await state.queryChanged("Zero Harbor")
        await waitUntil { state.visibleItems.map(\.id) == [2] }
        await gate.release("Title")
        await waitUntil { state.pendingFilterCount == 0 }
        XCTAssertEqual(state.visibleItems.map(\.id), [2])
    }
    func testFiveQueryEditsCoalesceIntoOneActualFilterPassAfter150msWait() async throws {
        let rows = try items(2)
        let delay = LibraryDelayGate()
        let recorder = LibraryQueryRecorder()
        let state = LibraryGridCoordinator(fetch: { _, _, _ in Page(items: rows, total: rows.count) }, worker: { snapshot, filter, query in
            await recorder.append(query)
            return await LibraryGridCoordinator.filterSnapshot(snapshot, filter, query)
        }, delay: { await delay.sleep($0) })
        defer { state.stop() }
        await state.load(libraryIds: [1], sort: .title)
        await waitUntil { state.pendingFilterCount == 0 }
        await recorder.clear()
        var edits: [Task<Void, Never>] = []
        for (index, text) in ["Z", "Ze", "Zer", "Zero", "Zero Harbor"].enumerated() {
            edits.last?.cancel() // Matches SwiftUI's query-keyed task cancellation.
            edits.append(Task { await state.queryChanged(text) })
            await delay.waitForSleeps(index + 1)
        }
        let before = await recorder.queries
        XCTAssertEqual(before, [], "No filtering before the production debounce wait finishes")
        let durations = await delay.durations
        XCTAssertEqual(durations, Array(repeating: .milliseconds(150), count: 5))
        await delay.releaseAll()
        for edit in edits { await edit.value }
        await waitUntil { state.pendingFilterCount == 0 }
        let queries = await recorder.queries
        XCTAssertEqual(queries, ["Zero Harbor"])
        XCTAssertEqual(state.visibleItems.map(\.id), [2])
    }
    func testRowsDriveTheWholeLibraryAndRetryKeepsDecidedItems() async throws {
        let rows = try items(421)
        var fail = true
        var offsets: [Int] = []
        let state = LibraryGridCoordinator(fetch: { _, _, offset in
            offsets.append(offset)
            if offset == 200 && fail { fail = false; throw URLError(.cannotConnectToHost) }
            return Page(items: Array(rows.dropFirst(offset).prefix(200)), total: rows.count)
        }, delay: { _ in })
        defer { state.stop() }
        state.presentationChanged(rows: true)
        await state.load(libraryIds: [1], sort: .title)
        await waitUntil { state.error != nil }
        XCTAssertFalse(state.complete)
        XCTAssertEqual(state.items.count, 200)
        await state.retry()
        await waitUntil { state.groups.flatMap(\.items).count == 421 }
        XCTAssertTrue(state.complete)
        XCTAssertNil(state.error)
        XCTAssertEqual(offsets, [0, 200, 200, 400])
        XCTAssertEqual(Set(state.groups.flatMap(\.items).map(\.id)).count, 421)
    }

    func testReturningToLoadedRowsDoesNotRefetchPages() async throws {
        let rows = try items(421)
        var calls = 0
        let state = LibraryGridCoordinator(fetch: { _, _, offset in
            calls += 1
            return Page(items: Array(rows.dropFirst(offset).prefix(200)), total: rows.count)
        }, delay: { _ in })
        defer { state.stop() }
        state.presentationChanged(rows: true)
        await state.load(libraryIds: [1], sort: .title)
        await waitUntil { state.complete && state.visibleItems.count == 421 }
        state.stop()
        await state.resume()
        XCTAssertEqual(calls, 3)
        XCTAssertEqual(state.visibleItems.count, 421)
    }

}
