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
    @MainActor func testStaleHistoryCannotReplaceFrozenConsentOrResurrectDeletedReport() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let auth = Session.shared.playbackAuthorization
        let store = DiagnosticStore(root: root, authorizationGeneration: { auth.generation })
        let initial = DiagnosticRecorder(persistenceEnabled: false).freeze()
        let capture = DiagnosticCapture(id: initial.id, namespace: "anonymous", generation: auth.generation, capturedAt: initial.capturedAt, incidentMs: initial.incidentMs, events: [], dropped: 0, persistence: "available", historyEnabled: true)
        let row = DiagnosticStore.Draft(capture: capture, category: "other", description: "", serverEvents: [], serverComponents: [], connection: [])
        _ = await store.save(row)
        var frozen = row; frozen.consented = true; frozen.origin = "https://support.example"; frozen.title = "Explicit private title"; frozen.includePlatform = true
        let report = try DiagnosticReport.archive(frozen, client: DiagnosticReport.client)
        frozen.binding = report.binding; frozen.manifest = report.manifest; frozen.state = "staged"
        _ = await store.saveArchive(report.bytes, id: row.id, scope: "anonymous", generation: auth.generation)
        _ = await store.save(frozen)
        let staleWrite = await store.save(row); XCTAssertEqual(staleWrite, "unavailable")
        let replacement = await store.saveArchive(Data("replacement".utf8), id: row.id, scope: "anonymous", generation: auth.generation)
        XCTAssertEqual(replacement, "unavailable")
        let model = DiagnosticReportModel(store: store); model.pending = [row]; model.draft = row
        model.resume(row)
        for _ in 0..<200 where model.busy { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(model.busy); XCTAssertEqual(model.draft?.binding, report.binding)
        XCTAssertTrue(model.uploadConsent); XCTAssertTrue(model.includeTitle); XCTAssertTrue(model.includePlatform)
        XCTAssertEqual(model.destination, frozen.origin); XCTAssertEqual(model.pending.first?.state, "staged")
        let kept = await store.archive(row.id, scope: "anonymous"); XCTAssertEqual(kept, report.bytes)
        model.deleteLocal(row); model.close()
        for _ in 0..<200 {
            if await store.drafts("anonymous").isEmpty { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTAssertNil(model.draft)
        let deleted = await store.drafts("anonymous"); XCTAssertTrue(deleted.isEmpty)
        let queued = await store.save(frozen); XCTAssertEqual(queued, "unavailable")
        let queuedArchive = await store.saveArchive(report.bytes, id: row.id, scope: "anonymous", generation: auth.generation); XCTAssertEqual(queuedArchive, "unavailable")
        let removed = await store.archive(row.id, scope: "anonymous"); XCTAssertNil(removed)
    }
    @MainActor func testExportProducesAttachableZIPWithoutUpload() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let auth = Session.shared.playbackAuthorization
        let store = DiagnosticStore(root: root, authorizationGeneration: { auth.generation })
        let initial = DiagnosticRecorder(persistenceEnabled: false).freeze()
        let capture = DiagnosticCapture(id: initial.id, namespace: "anonymous", generation: auth.generation, capturedAt: initial.capturedAt, incidentMs: initial.incidentMs, events: [], dropped: 0, persistence: "available", historyEnabled: true)
        let row = DiagnosticStore.Draft(capture: capture, category: "connection", description: "Cannot sign in; expected the library", serverEvents: [], serverComponents: [], connection: [])
        _ = await store.save(row)
        let model = DiagnosticReportModel(store: store); model.draft = row; model.presented = true
        defer { model.cleanupShare(); model.close() }
        XCTAssertFalse(model.uploadConsent); XCTAssertTrue(model.destination.isEmpty)
        model.export()
        for _ in 0..<500 where model.busy { try await Task.sleep(for: .milliseconds(10)) }
        XCTAssertFalse(model.busy)
        let url = try XCTUnwrap(model.shareURL)
        XCTAssertEqual(url.lastPathComponent, "cinema-report-" + row.id + ".zip")
        let bytes = try Data(contentsOf: url)
        let expected = try DiagnosticReport.archive(row, client: DiagnosticReport.client)
        XCTAssertEqual(bytes, expected.bytes, "The actual exported file must contain the selected incident")
        XCTAssertTrue(bytes.starts(with: [0x50, 0x4b, 0x03, 0x04]))
        XCTAssertTrue(String(decoding: bytes, as: UTF8.self).contains("summary.txt"))
        XCTAssertTrue(String(decoding: bytes, as: UTF8.self).contains("manifest.json"))
        XCTAssertEqual(model.draft?.id, row.id); XCTAssertFalse(model.draft?.consented ?? true)
        XCTAssertNil(model.draft?.origin); XCTAssertNil(model.draft?.binding)
        XCTAssertTrue(model.message.contains("Attach it to your bug or issue report"))
    }
    func testLocalDownloadRequiresExplicitPOSTAndReturnsExactArchiveOnce() async throws {
        let bytes = Data([0x50, 0x4b, 3, 4, 10, 0, 99])
        let export = try DiagnosticLocalExport(bytes: bytes, reportID: UUID().uuidString.lowercased(), host: "127.0.0.1")
        defer { export.stop() }
        let url = try await export.start()
        let configuration = URLSessionConfiguration.ephemeral; configuration.timeoutIntervalForRequest = 3
        let session = URLSession(configuration: configuration); defer { session.invalidateAndCancel() }
        let (wrong, rejected) = try await session.data(from: url.deletingLastPathComponent().appendingPathComponent("wrong"))
        XCTAssertEqual((rejected as? HTTPURLResponse)?.statusCode, 404); XCTAssertTrue(wrong.isEmpty)
        var crossOrigin = URLRequest(url: url); crossOrigin.httpMethod = "POST"; crossOrigin.setValue("http://attacker.invalid", forHTTPHeaderField: "Origin")
        let (_, crossRejected) = try await session.data(for: crossOrigin); XCTAssertEqual((crossRejected as? HTTPURLResponse)?.statusCode, 403)
        let (page, response) = try await session.data(from: url)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 200); XCTAssertTrue(String(decoding: page, as: UTF8.self).contains("Download diagnostic ZIP"))
        XCTAssertNotEqual(page, bytes)
        var download = URLRequest(url: url); download.httpMethod = "POST"
        let (actual, attachment) = try await session.data(for: download)
        XCTAssertEqual(actual, bytes); XCTAssertEqual((attachment as? HTTPURLResponse)?.mimeType, "application/zip")
        XCTAssertTrue((attachment as? HTTPURLResponse)?.value(forHTTPHeaderField: "Content-Disposition")?.contains("attachment") == true)
        do { _ = try await session.data(for: download); XCTFail("The capability must be single-download") } catch {}
    }
    func testLocalDownloadExpiryAndCancellationRevokeListener() async throws {
        let expired = try DiagnosticLocalExport(bytes: Data([1]), reportID: UUID().uuidString, host: "127.0.0.1", lifetime: 0.1)
        let expiredURL = try await expired.start()
        try await Task.sleep(for: .milliseconds(250))
        let session = URLSession(configuration: .ephemeral); defer { session.invalidateAndCancel() }
        do { _ = try await session.data(from: expiredURL); XCTFail("Expired URL must not serve") } catch {}
        let stopped = try DiagnosticLocalExport(bytes: Data([1]), reportID: UUID().uuidString, host: "127.0.0.1")
        let stoppedURL = try await stopped.start(); stopped.stop()
        try await Task.sleep(for: .milliseconds(50))
        do { _ = try await session.data(from: stoppedURL); XCTFail("Closed report must not serve") } catch {}
    }
    func testSupportDestinationRefusesCredentialsPathsAndUnapprovedPlaintext() {
        for origin in ["https://user:secret@support.example", "https://support.example/a", "https://support.example?secret=canary", "http://support.example", "file:///tmp/report"] { XCTAssertThrowsError(try DiagnosticTransport.origin(origin)) }
        XCTAssertEqual(try DiagnosticTransport.origin("https://support.example/"), "https://support.example")
    }
}
