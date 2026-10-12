import XCTest
@testable import plurx

final class DiagnosticCaptureTests: XCTestCase {
    func testBoundedRecorderRetainsTerminalAndExactCountersWithoutSecrets() throws {
        let recorder = DiagnosticRecorder(persistenceEnabled: false)
        recorder.record("playback", "terminal", ["state": .text("failed"), "failure": .text("decode"), "url": .text("CANARY_TOKEN")])
        recorder.record("playback", "state", ["frame_count": .text("18446744073709551615"), "dropped_frames": .text("9007199254740993"), "title": .text("CANARY_TITLE")])
        for index in 0..<20_000 { recorder.record("playback", "sample", ["playhead_ms": .number(UInt64(index))]) }
        let capture = recorder.freeze(); let data = try JSONEncoder().encode(capture.events)
        XCTAssertLessThanOrEqual(capture.events.count, 10_000); XCTAssertLessThan(data.count, DiagnosticRecorder.maximumBytes)
        XCTAssertGreaterThan(capture.dropped, 0); XCTAssertTrue(capture.events.contains { $0.kind == "terminal" && $0.facts["failure"] == .text("decode") })
        let text = String(decoding: data, as: UTF8.self); XCTAssertFalse(text.contains("CANARY")); XCTAssertTrue(text.contains("18446744073709551615")); XCTAssertTrue(text.contains("9007199254740993"))
    }
    func testRecoveredStoreCannotOpenAnotherAccountOrResurrectClearedCapture() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let first = DiagnosticStore(root: root, authorizationGeneration: { 0 }); let namespace = String(repeating: "a", count: 64)
        var capture = DiagnosticRecorder(persistenceEnabled: false).freeze()
        capture = DiagnosticCapture(id: capture.id, namespace: namespace, generation: 0, capturedAt: capture.capturedAt, incidentMs: capture.incidentMs, events: capture.events, dropped: 0, persistence: "available", historyEnabled: true)
        let draft = DiagnosticStore.Draft(capture: capture, category: "playback", description: "user text", serverEvents: [], serverComponents: [], connection: [])
        let persistence = await first.save(draft); XCTAssertEqual(persistence, "available")
        let restarted = DiagnosticStore(root: root, authorizationGeneration: { 0 }); let recovered = await restarted.drafts(namespace)
        XCTAssertEqual(recovered.map(\.id), [draft.id]); let other = await restarted.drafts(String(repeating: "b", count: 64)); XCTAssertTrue(other.isEmpty)
        await restarted.clear(namespace); let denied = await restarted.save(draft); XCTAssertEqual(denied, "unavailable")
        await restarted.activate(namespace); let cleared = await restarted.drafts(namespace); XCTAssertTrue(cleared.isEmpty)
    }
    func testQueuedWriteCannotResurrectAfterSameAccountAuthenticatesAgain() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        var generation: UInt64 = 0
        let store = DiagnosticStore(root: root, authorizationGeneration: { generation })
        let original = DiagnosticRecorder(persistenceEnabled: false).freeze()
        let capture = DiagnosticCapture(id: original.id, namespace: "anonymous", generation: 0, capturedAt: original.capturedAt, incidentMs: original.incidentMs, events: [], dropped: 0, persistence: "available", historyEnabled: true)
        let draft = DiagnosticStore.Draft(capture: capture, category: "other", description: "", serverEvents: [], serverComponents: [], connection: [])
        _ = await store.save(draft); await store.clear("anonymous"); generation = 1; await store.activate("anonymous")
        let stale = await store.save(draft); XCTAssertEqual(stale, "unavailable")
        let pending = await store.drafts("anonymous"); XCTAssertTrue(pending.isEmpty)
    }
    @MainActor func testArchiveHasStandardCRCAndTitleRequiresConsent() throws {
        let capture = DiagnosticRecorder(persistenceEnabled: false).freeze()
        var draft = DiagnosticStore.Draft(capture: capture, category: "other", description: "123456789", serverEvents: [], serverComponents: [], connection: [])
        let client = DiagnosticReport.Client(platform: "ios", app_version: "fixture", build: "1", os_version: "17", device_model: "fixture")
        let report = try DiagnosticReport.archive(draft, client: client)
        XCTAssertFalse(report.binding.privacy.titles_included); XCTAssertEqual(report.binding.length, report.bytes.count)
        XCTAssertEqual(try DiagnosticWire.manifest(JSONEncoder().encode(report.manifest))["report_id"] as? String, draft.id)
        let zipped = try DiagnosticReport.zip([("description.txt", Data("123456789".utf8))])
        XCTAssertEqual(Array(zipped[14..<18]), [0x26,0x39,0xf4,0xcb]) // IEEE CRC-32 standard vector.
        draft.title = "Explicit title"; let titled = try DiagnosticReport.archive(draft, client: client)
        XCTAssertTrue(titled.binding.privacy.titles_included); XCTAssertNotEqual(report.binding.sha256, titled.binding.sha256)
        XCTAssertThrowsError(try DiagnosticReport.zip([("../outside", Data())]))
    }
    func testSupportDestinationRefusesCredentialsPathsAndUnapprovedPlaintext() {
        for origin in ["https://user:secret@support.example", "https://support.example/a", "https://support.example?secret=canary", "http://support.example", "file:///tmp/report"] { XCTAssertThrowsError(try DiagnosticTransport.origin(origin)) }
        XCTAssertEqual(try DiagnosticTransport.origin("https://support.example/"), "https://support.example")
    }
}
