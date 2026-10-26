import AVFoundation
import Combine
import Foundation
import XCTest
@testable import plurx

/// Synthetic HTTP exercises the production native session. It is not a claim
/// of shipped model quality, server execution, or physical output acceptance.
private final class CaptionFixture: @unchecked Sendable {
    let lock = NSLock()
    var requests: [URLRequest] = []
    var corruptSegment = false
    var wrongPageRevision = false
    var stopped = false
    var oversizedOptions = false
    var textEnabled = false
    let textJob = "44444444-4444-4444-8444-444444444444"
    let asset = String(repeating: "a", count: 64)
    let profileID = String(repeating: "b", count: 64)
    let source = String(repeating: "c", count: 64)
    let model = String(repeating: "d", count: 64)
    let operation = "11111111-1111-4111-8111-111111111111"
    let demand = "22222222-2222-4222-8222-222222222222"
    let lease = "33333333-3333-4333-8333-333333333333"
    let segmentID = String(repeating: "e", count: 64)
    let cueID = String(repeating: "f", count: 64)
    var epoch: String { stopped ? "9007199254740994" : "9007199254740993" }
    func data(_ value: Any) -> Data { try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    var profile: [String: Any] {
        ["profile_id": profileID, "model_id": "small", "model_sha256": model, "task": "transcribe", "output_language": NSNull(),
         "input_languages": ["en", "de"], "timeline_contract": "source-audio-clock-v1",
         "limits": ["segment_bytes": 16384, "segment_cues": 128, "page_bytes": 65536, "page_entries": 64,
                    "coverage_intervals": 64, "concurrent_fetches": 2, "cue_memory_bytes": 524288, "cue_count": 1024,
                    "cue_window_before_ms": 30000, "cue_window_after_ms": 90000, "poll_min_ms": 1000,
                    "poll_max_ms": 5000, "retry_max_ms": 30000, "heartbeat_interval_ms": 5000]]
    }
    var segment: [String: Any] {
        ["version": 1, "asset_id": asset, "segment_id": segmentID, "core_index": 2, "start_ms": 60000, "end_ms": 90000,
         "cues": [["cue_id": cueID, "start_ms": 64000, "end_ms": 67000, "text": "A literal <caption> & source clock"]],
         "provenance": ["engine_sha256": String(repeating: "1", count: 64), "model_sha256": model,
                        "operation_epoch": "9007199254740993", "platform": "fixture", "profile_id": profileID,
                        "source_sha256": source, "worker_fence": "9007199254740995"]]
    }
    var reference: [String: Any] {
        ["segment_id": segmentID, "digest": CaptionValidation.sha256(data(segment)), "start_ms": 60000, "end_ms": 90000]
    }
    func reply(_ request: URLRequest) -> Data {
        lock.lock(); defer { lock.unlock() }
        requests.append(request)
        let path = request.url!.path
        let textConfig: [String: Any] = ["version": 1, "revision": "1", "enabled": textEnabled, "model_id": "opus-mt-mul-en", "target_language": "en"]
        let textWorkers: [[String: Any]] = [["node_id": "worker-a", "label": "Fixture worker", "boot_id": operation,
                                           "model_id": "opus-mt-mul-en", "status": "missing"]]
        if path.hasSuffix("/subtitle-text-translation/options") {
            return data(["version": 1, "file_id": "7", "source_size": "10000", "source_mtime": "200", "model_id": "opus-mt-mul-en",
                         "configuration": textConfig, "workers": textWorkers, "permissions": ["read": true, "create": true],
                         "subtitles": [["index": 22, "language": "de", "title": "German text", "text_eligible": true],
                                       ["index": 23, "language": "de", "title": "German bitmap", "text_eligible": false]]])
        }
        if path.hasSuffix("/subtitle-text-translation/models") {
            return data(["version": 1, "workers": textWorkers, "input_languages": ["de", "fr"]])
        }
        if path.hasSuffix("/subtitle-text-translation/config") {
            if request.httpMethod == "PUT", let bytes = request.httpBody,
               let body = try? JSONSerialization.jsonObject(with: bytes) as? [String: Any] { textEnabled = body["enabled"] as? Bool ?? false }
            return data(["version": 1, "revision": request.httpMethod == "PUT" ? "2" : "1", "enabled": textEnabled,
                         "model_id": "opus-mt-mul-en", "target_language": "en"])
        }
        if path.hasSuffix("/subtitles/22/translation") {
            return data(["version": 1, "job_id": textJob, "state": "queued"])
        }
        if path.hasSuffix("/subtitle-text-translation/jobs/" + textJob) {
            return data(["version": 1, "job_id": textJob, "state": "succeeded", "phase": "published", "retryable": false,
                         "progress": ["completed": 3, "total": 3],
                         "result": ["file_id": "7", "subtitle_index": 100005, "downloaded_subtitle_id": asset, "language": "en"]])
        }
        if path.hasSuffix("/options") {
            if oversizedOptions { return Data(repeating: 120, count: 70_000) }
            return data(["version": 1, "file_id": "7", "source_size": "10000", "source_mtime": "200", "duration_ms": 120000,
                         "audio_tracks": [["index": 2, "audio_ordinal": 2, "source_index": 5, "normalized_language": "en"]],
                         "models": [profile], "capability": ["supported": true],
                         "permissions": ["read": true, "demand": true, "stop": true, "resume": true], "assets": [], "server_now_ms": 100000])
        }
        if path.hasSuffix("/subtitle-translation") {
            return data(["version": 1, "outcome": "accepted", "operation_id": operation, "operation_epoch": epoch,
                         "view_revision": "1", "asset_id": asset, "demand_id": demand, "demand_generation": "1", "state": "partial", "server_now_ms": 100000])
        }
        if path.hasSuffix("/stop") { stopped = true
            return data(["version": 1, "outcome": "stopped", "operation_id": operation, "operation_epoch": epoch,
                         "view_revision": "2", "asset_id": asset, "state": "stopped", "server_now_ms": 100000])
        }
        if path.contains("/demands/") {
            return data(["version": 1, "outcome": request.httpMethod == "DELETE" ? "released" : "updated",
                         "operation_id": operation, "operation_epoch": epoch, "view_revision": "1", "demand_id": demand,
                         "demand_generation": "2", "server_now_ms": 100000])
        }
        if path.hasSuffix("/operations/" + operation) {
            return data(["version": 1, "operation_id": operation, "operation_epoch": epoch, "view_revision": stopped ? "2" : "1",
                         "state": stopped ? "stopped" : "partial", "retryable": false, "asset_id": asset, "content_revision": source,
                         "file_id": "7", "source_size": "10000", "source_mtime": "200", "audio_index": 2,
                         "spoken_language": "en", "output_language": "en", "task": "transcribe", "profile_id": profileID,
                         "failed_ranges": [], "server_now_ms": 100000])
        }
        if path.hasSuffix("/manifest") {
            return data(["version": 1, "asset_id": asset, "file_id": "7", "content_revision": source, "profile_id": profileID,
                         "audio_index": 2, "spoken_language": "en", "output_language": "en", "timeline_contract": "source-audio-clock-v1",
                         "manifest_revision": "1", "view_revision": stopped ? "2" : "1",
                         "operation": ["operation_id": operation, "epoch": epoch, "state": stopped ? "stopped" : "partial"],
                         "snapshot": ["lease_id": lease, "revision": "1", "expires_in_ms": 15000],
                         "coverage": [["start_ms": 60000, "end_ms": 90000]], "segments": wrongPageRevision ? [] : [reference],
                         "segment_next_cursor": wrongPageRevision ? "page-two" : NSNull(), "server_now_ms": 100000])
        }
        if path.hasSuffix("/segments") {
            return data(["version": 1, "asset_id": asset, "snapshot_revision": "2", "entries": [reference], "next_cursor": NSNull()])
        }
        if path.hasSuffix("/segments/" + segmentID) {
            if corruptSegment { return data(["untrusted": "different exact bytes"]) }
            return data(segment)
        }
        return data([:])
    }
    var captured: [URLRequest] { lock.lock(); defer { lock.unlock() }; return requests }
}
private final class CaptionFixtureProtocol: URLProtocol {
    static var fixture: CaptionFixture?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let fixture = Self.fixture else { client?.urlProtocol(self, didFailWithError: URLError(.cancelled)); return }
        let body = fixture.reply(request)
        let response = HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() { }
}

@MainActor
final class ProgressiveCaptionTests: XCTestCase {
    private var subscriptions: Set<AnyCancellable> = []
    private var transport: URLSession!
    private var fixture: CaptionFixture!
    private var owner: ProgressiveCaptionSession!
    override func setUp() {
        fixture = CaptionFixture(); CaptionFixtureProtocol.fixture = fixture
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [CaptionFixtureProtocol.self]
        transport = URLSession(configuration: config)
        owner = ProgressiveCaptionSession()
        Session.shared.setCredentials(origin: "https://captions.test", token: "caption-fixture")
    }
    override func tearDown() {
        owner.retire(); subscriptions = []; transport.invalidateAndCancel()
        Session.shared.setCredentials(origin: "", token: nil); CaptionFixtureProtocol.fixture = nil
    }
    private func configure() async {
        let loaded = expectation(description: "validated options")
        owner.$options.compactMap { $0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &subscriptions)
        owner.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7, sourceAudioIndex: 2,
                        now: { (65000, 60000, 1.5, false) }, current: { true })
        await fulfillment(of: [loaded], timeout: 2)
    }
    func testNonzeroAudioOrdinalAndProgressiveTranscriptionUseSourceClock() async throws {
        await configure()
        let visible = expectation(description: "validated partial cue")
        owner.$window.compactMap { $0 }.filter { $0.cues.count == 1 }.prefix(1).sink { _ in visible.fulfill() }.store(in: &subscriptions)
        XCTAssertTrue(owner.request(.transcribe))
        await fulfillment(of: [visible], timeout: 2)
        let window = try XCTUnwrap(owner.window), cue = try XCTUnwrap(window.cues.first)
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: window.baseMs, offsetMs: 0), 4000..<7000)
        XCTAssertEqual(cue.text, "A literal <caption> & source clock")
        let create = try XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" })
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: create.httpBody!) as? [String: Any])
        XCTAssertEqual(body["audio_index"] as? Int, 2)
        XCTAssertEqual(body["task"] as? String, "transcribe")
        XCTAssertEqual(body["output_language"] as? String, "en")
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
        owner.subtitleOffsetMs = 250
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: 60000, offsetMs: owner.window!.offsetMs), 4250..<7250)
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
    }
    func testCorruptImmutableSegmentNeverPublishesText() async {
        fixture.corruptSegment = true
        await configure()
        let rejected = expectation(description: "segment rejected")
        owner.$status.filter { $0.contains("validation") }.prefix(1).sink { _ in rejected.fulfill() }.store(in: &subscriptions)
        owner.request(.transcribe)
        await fulfillment(of: [rejected], timeout: 2)
        XCTAssertTrue(owner.window?.cues.isEmpty ?? true)
    }
    func testMixedSnapshotPagesNeverPublishText() async {
        fixture.wrongPageRevision = true
        await configure()
        let rejected = expectation(description: "snapshot rejected")
        owner.$status.filter { $0.contains("validation") }.prefix(1).sink { _ in rejected.fulfill() }.store(in: &subscriptions)
        owner.request(.transcribe)
        await fulfillment(of: [rejected], timeout: 2)
        XCTAssertTrue(owner.window?.cues.isEmpty ?? true)
    }
    func testDeselectRevokesPresentationAndDemandBeforeSeek() async {
        await configure()
        let visible = expectation(description: "partial cue")
        owner.$window.compactMap { $0 }.filter { !$0.cues.isEmpty }.prefix(1).sink { _ in visible.fulfill() }.store(in: &subscriptions)
        owner.request(.transcribe)
        await fulfillment(of: [visible], timeout: 2)
        owner.deselect(); owner.mediaChanged(seek: true)
        XCTAssertFalse(owner.selected); XCTAssertNil(owner.window)
    }
    func testStopRetainsCommittedCuesAndSeekDoesNotRestartWork() async throws {
        await configure()
        let visible = expectation(description: "partial cue")
        owner.$window.compactMap { $0 }.filter { !$0.cues.isEmpty }.prefix(1).sink { _ in visible.fulfill() }.store(in: &subscriptions)
        owner.request(.transcribe)
        await fulfillment(of: [visible], timeout: 2)
        let settled = expectation(description: "stop acknowledged")
        owner.$operation.compactMap { $0 }.filter { $0.state == "stopped" }.prefix(1).sink { _ in settled.fulfill() }.store(in: &subscriptions)
        owner.stop()
        await fulfillment(of: [settled], timeout: 2)
        let before = fixture.captured.filter { $0.httpMethod == "PUT" }.count
        let reread = expectation(description: "committed captions reread after stopped seek")
        owner.$window.dropFirst().compactMap { $0 }.filter { !$0.cues.isEmpty }.prefix(1).sink { _ in reread.fulfill() }.store(in: &subscriptions)
        owner.mediaChanged(seek: true)
        await fulfillment(of: [reread], timeout: 3)
        XCTAssertTrue(owner.stopped)
        XCTAssertEqual(fixture.captured.filter { $0.httpMethod == "PUT" }.count, before)
        XCTAssertEqual(owner.window?.cues.count, 1)
        let stop = try XCTUnwrap(fixture.captured.first { $0.url?.path.hasSuffix("/stop") == true })
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: stop.httpBody!) as? [String: Any])
        XCTAssertEqual(body["expected_operation_epoch"] as? String, "9007199254740993")
    }
    func testSynchronizedTextRebindsToSuccessorOriginAndTeardownRemovesIt() throws {
        let view = PlayerSurfaceView(frame: CGRect(x: 0, y: 0, width: 320, height: 180))
        let first = AVPlayerItem(url: URL(string: "https://captions.test/first.mp4")!)
        let second = AVPlayerItem(url: URL(string: "https://captions.test/second.mp4")!)
        let cue = CaptionCue(cueId: fixture.cueID, startMs: 64000, endMs: 67000, text: "Literal <text>")
        view.applyCaptionWindow(.init(revision: 1, baseMs: 60000, offsetMs: 0, cues: [cue]), to: first)
        var layer = try XCTUnwrap(view.layer.sublayers?.compactMap { $0 as? AVSynchronizedLayer }.first)
        XCTAssertTrue(layer.playerItem === first)
        var animation = try XCTUnwrap(layer.sublayers?.first?.animation(forKey: "caption-visibility"))
        XCTAssertEqual(animation.beginTime - AVCoreAnimationBeginTimeAtZero, 4, accuracy: 0.0001)
        view.applyCaptionWindow(.init(revision: 2, baseMs: 63000, offsetMs: 250, cues: [cue]), to: second)
        layer = try XCTUnwrap(view.layer.sublayers?.compactMap { $0 as? AVSynchronizedLayer }.first)
        XCTAssertTrue(layer.playerItem === second)
        animation = try XCTUnwrap(layer.sublayers?.first?.animation(forKey: "caption-visibility"))
        XCTAssertEqual(animation.beginTime - AVCoreAnimationBeginTimeAtZero, 1.25, accuracy: 0.0001)
        view.applyCaptionWindow(nil, to: nil)
        XCTAssertTrue(view.layer.sublayers?.compactMap { $0 as? AVSynchronizedLayer }.isEmpty ?? true)
    }
    func testPinnedCaptionAPIRejectsAReplacementPrincipalBeforeSending() async throws {
        let authorization = Session.shared.playbackAuthorization.generation
        let api = PlurxAPI(origin: "https://captions.test", testTransport: transport).captionOwner(generation: authorization)
        Session.shared.setCredentials(origin: "https://captions.test", token: "replacement-principal")
        do {
            let _: CaptionHTTPReply<CaptionOptions> = try await api.caption("files/7/subtitle-translation/options")
            XCTFail("The old caption owner must not send with a replacement principal")
        } catch { XCTAssertTrue(error is CancellationError) }
        XCTAssertTrue(fixture.captured.isEmpty)
    }
    func testStreamingBodyCeilingRejectsUnboundedOptionsBeforeDecode() async {
        fixture.oversizedOptions = true
        let rejected = expectation(description: "body ceiling")
        owner.$status.filter { $0.contains("memory limit") }.prefix(1).sink { _ in rejected.fulfill() }.store(in: &subscriptions)
        owner.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7, sourceAudioIndex: 2,
                        now: { (65000, 60000, 1, false) }, current: { true })
        await fulfillment(of: [rejected], timeout: 2)
        XCTAssertNil(owner.options)
    }
    func testExistingTextTranslationUsesDistinctJobAndDoesNotSelectProgressiveTrack() async throws {
        let text = SubtitleTextTranslation()
        defer { text.retire() }
        let loaded = expectation(description: "text caption options")
        text.$options.compactMap { $0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &subscriptions)
        text.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7)
        await fulfillment(of: [loaded], timeout: 2)
        let completed = expectation(description: "durable text translation result")
        text.$job.compactMap { $0?.result }.prefix(1).sink { _ in completed.fulfill() }.store(in: &subscriptions)
        text.translate()
        await fulfillment(of: [completed], timeout: 2)
        XCTAssertEqual(text.job?.result?.subtitleIndex, 100005)
        XCTAssertFalse(owner.selected, "Text translation cannot select the progressive speech track")
        let request = try XCTUnwrap(fixture.captured.first { $0.url?.path.hasSuffix("/subtitles/22/translation") == true })
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: request.httpBody!) as? [String: Any])
        XCTAssertEqual(body["source_language"] as? String, "de")
        XCTAssertEqual(body["target_language"] as? String, "en")
        XCTAssertEqual(body["model_id"] as? String, "opus-mt-mul-en")
        XCTAssertNil(body["audio_index"], "Existing-text translation is not an ASR request")
    }
    func testBitmapTextTranslationDoesNotInventOCRWork() async {
        let text = SubtitleTextTranslation()
        defer { text.retire() }
        let loaded = expectation(description: "text options")
        text.$options.compactMap { $0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &subscriptions)
        text.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7)
        await fulfillment(of: [loaded], timeout: 2)
        text.sourceIndex = 23; text.sourceLanguage = "de"; text.translate()
        XCTAssertFalse(fixture.captured.contains { $0.httpMethod == "POST" })
        XCTAssertTrue(text.status.contains("Bitmap"))
    }
    func testExplicitUseResolvesStableArtifactAfterIndexReuseOrReordering() {
        let wanted = String(repeating: "a", count: 64), other = String(repeating: "b", count: 64)
        let result = TextCaptionResult(fileId: "7", subtitleIndex: 100005, downloadedSubtitleId: wanted, language: "en")
        func track(_ index: Int, _ identity: String?) -> SubtitleTrack {
            SubtitleTrack(index: index, codec: "webvtt", language: "en", title: "English",
                          default: false, forced: false, text: true, native: true, downloadedSubtitleId: identity)
        }
        XCTAssertEqual(PlayerController.translatedTextCaptionIndex(result, fileId: 7,
            tracks: [track(100005, other), track(100004, wanted)]), 100004,
            "Use follows the immutable artifact to its refreshed index")
        XCTAssertNil(PlayerController.translatedTextCaptionIndex(result, fileId: 7,
            tracks: [track(100005, other)]), "A reused old index cannot select another caption")
        XCTAssertNil(PlayerController.translatedTextCaptionIndex(result, fileId: 7,
            tracks: [track(100005, nil)]), "Older servers without identity cannot establish this result")
        XCTAssertNil(PlayerController.translatedTextCaptionIndex(result, fileId: 7,
            tracks: [track(100004, wanted), track(100005, wanted)]), "Ambiguous identities refuse")
        XCTAssertNil(PlayerController.translatedTextCaptionIndex(result, fileId: 8,
            tracks: [track(100004, wanted)]), "A result never migrates to another source file")
    }

    func testCounterPrecisionAndOffsetClipping() throws {
        XCTAssertEqual(CaptionValidation.compare("9007199254740993", "9007199254740992"), .orderedDescending)
        XCTAssertEqual(try CaptionValidation.next("9007199254740993"), "9007199254740994")
        XCTAssertThrowsError(try CaptionValidation.next("9223372036854775807"))
        XCTAssertFalse(CaptionValidation.counter("01"))
        let cue = CaptionCue(cueId: fixture.cueID, startMs: 1000, endMs: 2000, text: "active at attach")
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: 1500, offsetMs: 0), 0..<500)
        XCTAssertNil(CaptionRenderWindow.itemInterval(cue, baseMs: 2000, offsetMs: 0))
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: 2000, offsetMs: 250), 0..<250)
    }
}
