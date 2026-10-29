import AVFoundation
import Combine
import Foundation
import XCTest
@testable import plurx

/// The renderer boundary owns the exact items handed to it, without asking
/// AVFoundation to load fake network playlists or trigger transport recovery.
private final class CaptionHandoffItem: AVPlayerItem {
    override var status: AVPlayerItem.Status { .readyToPlay }
    override func currentTime() -> CMTime { .zero }
}
private final class CaptionHandoffPlayer: AVPlayer {
    private var attached: AVPlayerItem?
    private var observedRate: Float = 0
    override var currentItem: AVPlayerItem? { attached }
    override func replaceCurrentItem(with item: AVPlayerItem?) { attached = item }
    override var rate: Float { get { observedRate } set { observedRate = newValue } }
    override var timeControlStatus: AVPlayer.TimeControlStatus { observedRate == 0 ? .paused : .playing }
    override func currentTime() -> CMTime { attached?.currentTime() ?? .zero }
    override func play() { observedRate = 1 }
    override func pause() { observedRate = 0 }
}

/// Synthetic HTTP exercises the production native session. It is not a claim
/// of shipped model quality, server execution, or physical output acceptance.
private final class CaptionFixture: @unchecked Sendable {
    let lock = NSLock()
    var requests: [URLRequest] = []
    var corruptSegment = false
    var wrongPageRevision = false
    var stopped = false
    var stoppedCreateConflict = false
    var unprepared = false
    var oversizedOptions = false
    var textEnabled = false
    var resumed = false
    var heartbeatIntervalMs = 5000
    var pollMinMs = 1000
    var offeredProfiles: [[String: Any]]?
    var defaultProfileID: String?
    private var requestedProfile: [String: Any]?
    private var requestedLanguage = "en"
    var activeProfile: [String: Any] { requestedProfile ?? profile }
    var activeProfileID: String { activeProfile["profile_id"] as? String ?? profileID }
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
    var epoch: String { stopped ? "9007199254740994" : resumed ? "9007199254740995" : "9007199254740993" }
    var viewRevision: String { stopped ? "2" : resumed ? "3" : "1" }
    func data(_ value: Any) -> Data { try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    var profile: [String: Any] {
        ["profile_id": profileID, "model_id": "small", "model_sha256": model, "task": "transcribe", "output_language": NSNull(),
         "input_languages": ["en", "de"], "timeline_contract": "source-audio-clock-v1", "window": ["core_ms": 30000],
         "limits": ["segment_bytes": 16384, "segment_cues": 128, "page_bytes": 65536, "page_entries": 64,
                    "coverage_intervals": 64, "concurrent_fetches": 2, "cue_memory_bytes": 524288, "cue_count": 1024,
                    "cue_window_before_ms": 30000, "cue_window_after_ms": 90000, "poll_min_ms": pollMinMs,
                    "poll_max_ms": 5000, "retry_max_ms": 30000, "heartbeat_interval_ms": heartbeatIntervalMs]]
    }
    var segment: [String: Any] {
        ["version": 1, "asset_id": asset, "segment_id": segmentID, "core_index": 2, "start_ms": 60000, "end_ms": 90000,
         "cues": [["cue_id": cueID, "start_ms": 64000, "end_ms": 67000, "text": "A literal <caption> & source clock"]],
         "provenance": ["engine_sha256": String(repeating: "1", count: 64), "model_sha256": model,
                        "operation_epoch": "9007199254740993", "platform": "fixture", "profile_id": activeProfileID,
                        "source_sha256": source, "worker_fence": "9007199254740995"]]
    }
    var reference: [String: Any] {
        ["segment_id": segmentID, "digest": CaptionValidation.sha256(data(segment)), "start_ms": 60000, "end_ms": 90000]
    }
    func reply(_ request: URLRequest) -> Data {
        lock.lock(); defer { lock.unlock() }
        requests.append(request)
        let path = request.url!.path
        let input = request.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
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
                         "models": offeredProfiles ?? [profile], "default_profile_id": defaultProfileID.map { $0 as Any } ?? NSNull(), "capability": ["supported": true],
                         "permissions": ["read": true, "demand": true, "stop": true, "resume": true], "assets": [], "server_now_ms": 100000])
        }
        if path.hasSuffix("/subtitle-translation") {
            if request.httpMethod == "POST" {
                requestedProfile = offeredProfiles?.first { $0["profile_id"] as? String == input["profile_id"] as? String }
                requestedLanguage = input["spoken_language"] as? String ?? "en"
            }
            if stoppedCreateConflict && stopped {
                return data(["version": 1, "code": "operation_stopped", "message": "Stopped", "operation_id": operation, "server_now_ms": 100000])
            }
            return data(["version": 1, "outcome": "accepted", "operation_id": operation, "operation_epoch": epoch,
                         "view_revision": viewRevision, "asset_id": asset, "demand_id": demand, "demand_generation": "1",
                         "continuity_id": input["continuity_id"] ?? NSNull(), "state": "partial", "server_now_ms": 100000])
        }
        if path.hasSuffix("/stop") { stopped = true
            return data(["version": 1, "outcome": "stopped", "operation_id": operation, "operation_epoch": epoch,
                         "view_revision": "2", "asset_id": asset, "state": "stopped", "server_now_ms": 100000])
        }
        if path.hasSuffix("/retry") {
            stopped = false; resumed = true
            return data(["version": 1, "outcome": "resumed", "operation_id": operation, "operation_epoch": epoch,
                         "view_revision": "3", "asset_id": asset, "demand_id": demand, "demand_generation": "1",
                         "continuity_id": input["continuity_id"] ?? NSNull(), "state": "partial", "server_now_ms": 100000])
        }
        if path.contains("/demands/") {
            return data(["version": 1, "outcome": request.httpMethod == "DELETE" ? "released" : "updated",
                         "operation_id": operation, "operation_epoch": epoch, "view_revision": viewRevision, "demand_id": demand,
                         "demand_generation": input["demand_generation"] ?? "2",
                         "continuity_id": input["continuity_id"] ?? NSNull(), "server_now_ms": 100000])
        }
        if path.hasSuffix("/operations/" + operation) {
            return data(["version": 1, "operation_id": operation, "operation_epoch": epoch, "view_revision": viewRevision,
                         "state": stopped ? "stopped" : "partial", "retryable": false, "asset_id": unprepared ? NSNull() : asset as Any, "content_revision": unprepared ? NSNull() : source as Any,
                         "file_id": "7", "source_size": "10000", "source_mtime": "200", "audio_index": 2,
                         "spoken_language": requestedLanguage, "output_language": activeProfile["task"] as? String == "translate" ? "en" : requestedLanguage,
                         "task": activeProfile["task"] ?? "transcribe", "profile_id": activeProfileID,
                         "model_id": activeProfile["model_id"] ?? "small", "model_sha256": activeProfile["model_sha256"] ?? model,
                         "timeline_contract": activeProfile["timeline_contract"] ?? "source-audio-clock-v1",
                         "failed_ranges": [], "server_now_ms": 100000])
        }
        if path.hasSuffix("/manifest") {
            return data(["version": 1, "asset_id": asset, "file_id": "7", "content_revision": source, "profile_id": activeProfileID,
                         "audio_index": 2, "spoken_language": requestedLanguage,
                         "output_language": activeProfile["task"] as? String == "translate" ? "en" : requestedLanguage, "timeline_contract": "source-audio-clock-v1",
                         "manifest_revision": "1", "view_revision": viewRevision,
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
    static var statusOverrides: [String:Int] = [:]
    static var holdRequest: (method:String,path:String)?
    static var onHeld: (() -> Void)?
    static var releaseHeld: (() -> Void)?
    private var cancelled = false
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let fixture = Self.fixture else { client?.urlProtocol(self, didFailWithError: URLError(.cancelled)); return }
        // URLSession may deliver a POST as an InputStream to URLProtocol.
        // Preserve the exact bytes the synthetic server consumes so assertions
        // inspect wire input rather than assuming URLRequest.httpBody storage.
        var recorded=request
        if recorded.httpBody == nil,let stream=recorded.httpBodyStream {
            stream.open();defer { stream.close() }
            var data=Data(),buffer=[UInt8](repeating:0,count:4096)
            while true {
                let count=stream.read(&buffer,maxLength:buffer.count)
                if count < 0 { client?.urlProtocol(self,didFailWithError:stream.streamError ?? URLError(.cannotDecodeRawData));return }
                if count == 0 { break }
                guard data.count+count<=65_536 else { client?.urlProtocol(self,didFailWithError:CaptionProtocolError.budget);return }
                data.append(contentsOf:buffer.prefix(count))
            }
            recorded.httpBodyStream=nil;recorded.httpBody=data
        }
        let body = fixture.reply(recorded)
        if let held = Self.holdRequest, request.httpMethod == held.method, request.url?.path == held.path {
            Self.releaseHeld = { [weak self] in self?.deliver(body) }
            Self.onHeld?()
            return
        }
        deliver(body)
    }
    private func deliver(_ body: Data) {
        guard !cancelled else { return }
        let status=Self.statusOverrides.first { request.url?.path.hasSuffix($0.key)==true }?.value ?? 200
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() { cancelled = true }
}

@MainActor
final class ProgressiveCaptionTests: XCTestCase {
    private var subscriptions: Set<AnyCancellable> = []
    private var transport: URLSession!
    private var fixture: CaptionFixture!
    private var owner: ProgressiveCaptionSession!
    private var captionOwners:[ProgressiveCaptionSession]=[]
    private var textOwners:[SubtitleTextTranslation]=[]
    override func setUp() {
        fixture = CaptionFixture(); CaptionFixtureProtocol.fixture = fixture
        CaptionFixtureProtocol.statusOverrides=[:]
        CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.onHeld=nil;CaptionFixtureProtocol.releaseHeld=nil
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [CaptionFixtureProtocol.self]
        transport = URLSession(configuration: config)
        owner = ProgressiveCaptionSession()
        captionOwners=[owner];textOwners=[]
        Session.shared.setCredentials(origin: "https://captions.test", token: "caption-fixture")
    }
    override func tearDown() async throws {
        // Fence every owner first, then settle its work before closing the
        // shared injected transport. URLSession task creation is asynchronous.
        for caption in captionOwners { caption.retire() }
        for text in textOwners { text.retire() }
        for caption in captionOwners { await caption.retireAndDrain() }
        for text in textOwners { await text.retireAndDrain() }
        subscriptions=[];captionOwners=[];textOwners=[]
        transport.invalidateAndCancel()
        CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.onHeld=nil;CaptionFixtureProtocol.releaseHeld=nil
        Session.shared.setCredentials(origin:"",token:nil);CaptionFixtureProtocol.fixture=nil
    }
    private func configure(now: @escaping () -> (Int, Int, Double, Bool) = { (65000, 60000, 1.5, false) }) async {
        let loaded = expectation(description: "fresh validated options")
        // Ignore the current owner's replayed value: reconfiguration must wait
        // for the next options response, not return on the previous snapshot.
        owner.$options.dropFirst().compactMap { $0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &subscriptions)
        owner.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7, sourceAudioIndex: 2,
                        now: now, current: { true })
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
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(create.httpBody,"CREATE must carry its exact serialized body")) as? [String: Any])
        XCTAssertEqual(body["audio_index"] as? Int, 2)
        XCTAssertEqual(body["task"] as? String, "transcribe")
        XCTAssertEqual(body["output_language"] as? String, "en")
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
        owner.subtitleOffsetMs = 250
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: 60000, offsetMs: owner.window!.offsetMs), 4250..<7250)
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
    }
    private func body(_ request: URLRequest) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody)) as? [String: Any])
    }
    func testAdvancingPlaybackKeepsContinuityAcrossCoreBoundaryPauseRateAndAttachment() async throws {
        fixture.heartbeatIntervalMs = 1; fixture.pollMinMs = 250
        var position = 65000, base = 60000
        var rate = 1.0, paused = false
        await configure(now: { (position, base, rate, paused) })
        XCTAssertTrue(owner.request(.transcribe))
        try await eventually { self.fixture.captured.contains { $0.httpMethod == "PUT" } }
        let create = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" }))
        let continuity = try XCTUnwrap(create["continuity_id"] as? String)
        XCTAssertTrue(CaptionValidation.uuid(continuity))
        // This is an advancing source clock crossing a core boundary, followed
        // by a quality attachment with a different item origin, not a seek.
        position = 95000; base = 90000; rate = 1.5; paused = true
        owner.mediaChanged()
        try await eventually {
            self.fixture.captured.contains { $0.httpMethod == "PUT" && (try? self.body($0)["position_ms"] as? Int) == 95000 }
        }
        position = 100000; paused = false; owner.subtitleOffsetMs = 500; owner.mediaChanged()
        try await eventually {
            self.fixture.captured.contains { $0.httpMethod == "PUT" && (try? self.body($0)["position_ms"] as? Int) == 100000 }
        }
        let updates = try fixture.captured.filter { $0.httpMethod == "PUT" }.map(body)
        XCTAssertTrue(updates.allSatisfy { $0["continuity_id"] as? String == continuity })
        XCTAssertTrue(updates.allSatisfy { $0["playback_id"] as? String == create["playback_id"] as? String })
        XCTAssertTrue(updates.contains { $0["paused"] as? Bool == true && $0["playback_rate"] as? Double == 1.5 })
        XCTAssertTrue(owner.selected)
    }
    func testSeekDuringHeldHeartbeatSettlesCASBeforeSendingNewContinuity() async throws {
        fixture.heartbeatIntervalMs = 30000; fixture.pollMinMs = 250
        var position = 65000
        await configure(now: { (position, 60000, 1, false) })
        let held = expectation(description: "first heartbeat held after server acceptance")
        CaptionFixtureProtocol.holdRequest = (method: "PUT", path: "/api/v1/subtitle-translation/operations/\(fixture.operation)/demands/\(fixture.demand)")
        CaptionFixtureProtocol.onHeld = { held.fulfill() }
        XCTAssertTrue(owner.request(.transcribe))
        await fulfillment(of: [held], timeout: 2)
        let first = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "PUT" }))
        XCTAssertEqual(first["demand_generation"] as? String, "2")
        position = 95000; owner.mediaChanged(seek: true, positionMs: position)
        // The server accepted generation 2 before this explicit seek. Its
        // immutable response must settle, then generation 3 revokes that UUID.
        CaptionFixtureProtocol.holdRequest = nil; CaptionFixtureProtocol.releaseHeld?()
        try await eventually { self.fixture.captured.filter { $0.httpMethod == "PUT" }.count >= 2 }
        let updates = try fixture.captured.filter { $0.httpMethod == "PUT" }.map(body)
        XCTAssertEqual(updates[1]["demand_generation"] as? String, "3")
        XCTAssertNotEqual(updates[1]["continuity_id"] as? String, first["continuity_id"] as? String)
        XCTAssertEqual(updates[1]["playback_id"] as? String, first["playback_id"] as? String)
        XCTAssertEqual(updates[1]["position_ms"] as? Int, 95000)
        XCTAssertTrue(CaptionValidation.uuid(try XCTUnwrap(updates[1]["continuity_id"] as? String)))
        XCTAssertTrue(owner.selected)
    }
    func testStopRetryAndSourceReconfigurationUseFreshTraversal() async throws {
        await configure(); XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.operation != nil }
        let create = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" }))
        owner.stop(); try await eventually { !self.owner.busy && self.owner.stopped }
        owner.retry(); try await eventually { !self.owner.busy && !self.owner.stopped }
        let retry = try body(XCTUnwrap(fixture.captured.first { $0.url?.path.hasSuffix("/retry") == true }))
        XCTAssertNotEqual(retry["continuity_id"] as? String, create["continuity_id"] as? String)
        XCTAssertTrue(CaptionValidation.uuid(try XCTUnwrap(retry["continuity_id"] as? String)))
        await owner.retireAndDrain(); fixture.stopped = false
        let before = fixture.captured.count
        await configure(); XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.operation != nil }
        let replacement = try body(XCTUnwrap(fixture.captured.dropFirst(before).first { $0.httpMethod == "POST" }))
        XCTAssertNotEqual(replacement["continuity_id"] as? String, retry["continuity_id"] as? String)
        XCTAssertNotEqual(replacement["playback_id"] as? String, create["playback_id"] as? String)
    }
    func testStoppedCreateDiscoversUnpreparedOperationAndRequiresExplicitResume() async throws {
        await configure()
        fixture.stopped = true; fixture.stoppedCreateConflict = true; fixture.unprepared = true
        CaptionFixtureProtocol.statusOverrides = ["/subtitle-translation": 409]
        XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.operation?.state == "stopped" }
        XCTAssertTrue(owner.canRetry); XCTAssertNil(owner.assetId)
        XCTAssertEqual(fixture.captured.filter { $0.httpMethod == "POST" }.count, 1)
        XCTAssertFalse(fixture.captured.contains { $0.url?.path.contains("/demands/") == true })
        owner.retry()
        try await eventually { !self.owner.busy && self.fixture.resumed }
        let retry = try body(XCTUnwrap(fixture.captured.first { $0.url?.path.hasSuffix("/retry") == true }))
        XCTAssertEqual(retry["expected_operation_epoch"] as? String, "9007199254740994")
        XCTAssertEqual(retry["expected_view_revision"] as? String, "2")
        XCTAssertEqual(retry["expected_state"] as? String, "stopped")
    }

    func testRecoveredOperationIdentityAndNonretryableCoreStatusStayFenced() throws {
        fixture.stopped = true; fixture.unprepared = true
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let options = try decoder.decode(CaptionOptions.self, from: fixture.reply(URLRequest(url: URL(string: "https://captions.test/options")!)))
        let original = fixture.reply(URLRequest(url: URL(string: "https://captions.test/operations/" + fixture.operation)!))
        var value = try XCTUnwrap(JSONSerialization.jsonObject(with: original) as? [String: Any])
        let validate: ([String: Any]) throws -> CaptionOperation = { value in
            let row = try decoder.decode(CaptionOperation.self, from: self.fixture.data(value))
            try row.validateRecovery(id: self.fixture.operation, file: 7, options: options, profile: options.models[0], audio: 2, language: "en")
            return row
        }
        _ = try validate(value)
        for (key, wrong) in [("file_id", "8"), ("source_size", "99"), ("model_sha256", String(repeating: "9", count: 64)), ("profile_id", String(repeating: "9", count: 64)), ("spoken_language", "de"), ("timeline_contract", "other")] {
            var changed = value; changed[key] = wrong; XCTAssertThrowsError(try validate(changed), key)
        }
        value["failed_ranges"] = [["core_index": 2, "range_revision": "1", "retryable": false, "reason": "source_clock_unsupported"]]
        let row = try validate(value)
        XCTAssertEqual(row.currentFailure(position: 65000, coreMs: 30000, covered: false)?.reason, "source_clock_unsupported")
        XCTAssertNil(row.currentFailure(position: 1000, coreMs: 30000, covered: false))
        XCTAssertNil(row.currentFailure(position: 65000, coreMs: 30000, covered: true))
        value["active_job"] = ["state": "running", "core_index": 2]
        XCTAssertNil(try validate(value).currentFailure(position: 65000, coreMs: 30000, covered: false))
        value.removeValue(forKey: "active_job")
        value["failed_ranges"] = (0..<16).map { ["core_index": $0, "range_revision": "1", "retryable": false, "reason": "source_clock_unsupported"] as [String: Any] }
        let firstPage = try validate(value)
        let later = [CaptionFailedRange(coreIndex: 20, rangeRevision: "1", retryable: false, reason: "source_clock_unsupported")]
        XCTAssertNil(firstPage.currentFailure(position: 605000, coreMs: 30000, covered: false))
        XCTAssertEqual(firstPage.currentFailure(position: 605000, coreMs: 30000, covered: false, displayedFailures: later)?.reason, "source_clock_unsupported")
        XCTAssertNil(firstPage.currentFailure(position: 605000, coreMs: 30000, covered: true, displayedFailures: later))
        value["active_job"] = ["state": "running", "core_index": 20]
        XCTAssertNil(try validate(value).currentFailure(position: 605000, coreMs: 30000, covered: false, displayedFailures: later))
    }

    func testDemandReceiptMustEchoSentContinuityExactly() throws {
        let expected = UUID().uuidString.lowercased()
        let json: [String: Any] = ["version": 1, "outcome": "updated", "operation_id": fixture.operation,
                                 "operation_epoch": "1", "view_revision": "1", "demand_id": fixture.demand,
                                 "demand_generation": "2", "continuity_id": UUID().uuidString.lowercased(), "server_now_ms": 100000]
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let receipt = try decoder.decode(CaptionReceipt.self, from: fixture.data(json))
        XCTAssertThrowsError(try receipt.validate(continuity: expected))
        XCTAssertNoThrow(try receipt.validate(continuity: receipt.continuityId))
    }
    private func installFourProfileFixture(defaultModel: String) -> [String: String] {
        var identifiers: [String: String] = [:]
        let rows: [(String, String, String)] = [("base", "translate", "1"), ("base", "transcribe", "2"),
                                                ("small", "translate", "3"), ("small", "transcribe", "4")]
        fixture.offeredProfiles = rows.map { model, task, digit in
            var entry = fixture.profile
            let id = String(repeating: digit, count: 64)
            identifiers[model + ":" + task] = id
            entry["profile_id"] = id; entry["model_id"] = model; entry["task"] = task
            entry["label"] = model.capitalized; entry["default"] = model == defaultModel
            entry["output_language"] = task == "translate" ? "en" as Any : NSNull()
            return entry
        }
        fixture.defaultProfileID = identifiers[defaultModel + ":translate"]
        return identifiers
    }
    func testSavedSmallDefaultHintPreservesModelAcrossTranscriptionAndTranslation() async throws {
        let ids = installFourProfileFixture(defaultModel: "small")
        await configure()
        XCTAssertEqual(owner.profileId, ids["small:translate"])
        XCTAssertEqual(owner.profile?.label, "Small")
        XCTAssertEqual(owner.profile?.default, true, "Optional metadata must decode when supplied")
        owner.spokenLanguage = "de"
        XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.window != nil }
        XCTAssertEqual(owner.profileId, ids["small:transcribe"])
        XCTAssertTrue(owner.request(.translate))
        try await eventually { !self.owner.busy && self.owner.window != nil }
        XCTAssertEqual(owner.profileId, ids["small:translate"])
        let creates = try fixture.captured.filter { $0.httpMethod == "POST" && $0.url?.path == "/api/v1/files/7/subtitle-translation" }.map(body)
        XCTAssertEqual(creates.compactMap { $0["model_id"] as? String }, ["small", "small"])
        XCTAssertEqual(creates.compactMap { $0["task"] as? String }, ["transcribe", "translate"])
        // An explicit owner choice wins over the saved default hint on refresh.
        owner.deselect(); owner.profileId = try XCTUnwrap(ids["base:translate"])
        await configure()
        XCTAssertEqual(owner.profileId, ids["base:translate"])
    }
    func testUnsetDefaultHintPrefersBaseAndRejectsUnknownProfileIdentity() async throws {
        let ids = installFourProfileFixture(defaultModel: "base")
        // Reorder the catalog to prove the hint, rather than first row, wins.
        fixture.offeredProfiles = fixture.offeredProfiles?.reversed().map { $0 }
        await configure()
        XCTAssertEqual(owner.profileId, ids["base:translate"])
        owner.spokenLanguage = "de"; XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.window != nil }
        XCTAssertEqual(owner.profileId, ids["base:transcribe"])
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        var request = URLRequest(url: URL(string: "https://captions.test/api/v1/files/7/subtitle-translation/options")!)
        request.httpMethod = "GET"
        fixture.defaultProfileID = String(repeating: "9", count: 64)
        let unknown = try decoder.decode(CaptionOptions.self, from: fixture.reply(request))
        XCTAssertThrowsError(try unknown.validate(file: 7))
        fixture.defaultProfileID = "not-a-profile-digest"
        let malformed = try decoder.decode(CaptionOptions.self, from: fixture.reply(request))
        XCTAssertThrowsError(try malformed.validate(file: 7))
        fixture.defaultProfileID = nil; owner.deselect(); owner.profileId = ""
        await configure()
        XCTAssertEqual(owner.profile?.modelId, "base", "Legacy null hints retain the Base fallback")
        var legacy = try XCTUnwrap(JSONSerialization.jsonObject(with: fixture.reply(request)) as? [String: Any])
        legacy.removeValue(forKey: "default_profile_id")
        let absent = try decoder.decode(CaptionOptions.self, from: fixture.data(legacy))
        XCTAssertNoThrow(try absent.validate(file: 7))
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
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(stop.httpBody,"Stop must carry its exact serialized body")) as? [String: Any])
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
        textOwners.append(text)
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
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody,"text translation must carry its exact serialized body")) as? [String: Any])
        XCTAssertEqual(body["source_language"] as? String, "de")
        XCTAssertEqual(body["target_language"] as? String, "en")
        XCTAssertEqual(body["model_id"] as? String, "opus-mt-mul-en")
        XCTAssertNil(body["audio_index"], "Existing-text translation is not an ASR request")
    }
    func testBitmapTextTranslationDoesNotInventOCRWork() async {
        let text = SubtitleTextTranslation()
        textOwners.append(text)
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
    private var publishedAsset: CaptionAsset {
        CaptionAsset(assetId:fixture.asset,profileId:fixture.profileID,operationId:fixture.operation,
                     audioIndex:2,spokenLanguage:"en",outputLanguage:"en",task:.transcribe)
    }
    private func eventually(_ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<400 { if condition() { return };try await Task.sleep(for:.milliseconds(10)) }
        XCTFail("caption transition did not settle");throw CaptionProtocolError.expired
    }
    func testPublishedReadCancelsMutationWithoutRetainingBusyOrLateStopStatus() async throws {
        for mutation in ["create","stop","retry"] {
            owner.retire();fixture.stopped=false
            await configure();owner.spokenLanguage="en"
            if mutation != "create" {
                XCTAssertTrue(owner.request(.transcribe));try await eventually { !self.owner.busy && self.owner.window != nil }
            }
            if mutation == "retry" { owner.stop();try await eventually { !self.owner.busy && self.owner.stopped } }
            let held=expectation(description:"held "+mutation)
            let path=mutation == "create" ? "/api/v1/files/7/subtitle-translation" : "/api/v1/subtitle-translation/operations/\(fixture.operation)/\(mutation)"
            CaptionFixtureProtocol.holdRequest=(method:"POST",path:path)
            CaptionFixtureProtocol.onHeld={ held.fulfill() }
            switch mutation { case "create":XCTAssertTrue(owner.request(.transcribe));case "stop":owner.stop();default:owner.retry() }
            await fulfillment(of:[held],timeout:2);XCTAssertTrue(owner.busy)
            owner.read(publishedAsset)
            XCTAssertFalse(owner.busy,"the read owns presentation, not the cancelled mutation's busy flag")
            try await eventually { self.owner.window != nil }
            let replacementStatus=owner.status
            CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.releaseHeld?()
            for _ in 0..<20 { await Task.yield() }
            XCTAssertFalse(owner.busy);XCTAssertEqual(owner.status,replacementStatus)
            XCTAssertFalse(owner.status.contains("Stop is unconfirmed"))
            XCTAssertTrue(owner.request(.transcribe),"Generate remains usable after replacing "+mutation)
            owner.deselect();CaptionFixtureProtocol.onHeld=nil;CaptionFixtureProtocol.releaseHeld=nil
        }
    }
    func testCancelledStopCannotOverwriteReconfiguredCaptionOwner() async throws {
        await configure();owner.spokenLanguage="en";XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.window != nil }
        let held=expectation(description:"held Stop")
        CaptionFixtureProtocol.holdRequest=(method:"POST",path:"/api/v1/subtitle-translation/operations/\(fixture.operation)/stop");CaptionFixtureProtocol.onHeld={ held.fulfill() }
        owner.stop();await fulfillment(of:[held],timeout:2)
        owner.retire();await configure()
        let status=owner.status
        CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.releaseHeld?()
        for _ in 0..<20 { await Task.yield() }
        XCTAssertFalse(owner.busy);XCTAssertFalse(owner.selected);XCTAssertEqual(owner.status,status)
    }

    private func burnHandoff(_ outcome: String, generate: Bool) async throws {
        let decision=try JSONDecoder().decode(Decision.self,from:Data("""
        {"fileId":7,"method":"transcode","playUrl":"/not-fetched","source":{"height":1080,"durationMs":120000},
         "audio":[{"index":2,"codec":"aac","default":true}],
         "subtitles":[{"index":0,"codec":"ass","language":"en","default":false,"forced":false,"text":true,"native":false}],"ladder":[]}
        """.utf8))
        func session(_ id:String) throws -> HlsStart { try JSONDecoder().decode(HlsStart.self,from:Data("""
        {"sessionId":"\(id)","playlistUrl":"/hls/\(id)/index.m3u8","mediaOriginMs":0,"durationMs":120000,"vod":true}
        """.utf8)) }
        let queued=outcome.hasPrefix("queued")
        let caps=Caps.snapshot().document,entered=expectation(description:"subtitle-off successor held")
        var calls=0,release:CheckedContinuation<HlsStart,Error>?
        var initialAttached=false,holdNextChange=false
        let controller=PlayerController(player:CaptionHandoffPlayer(),
            makePlayerItem:{ _ in CaptionHandoffItem(asset:AVMutableComposition()) },requestPlaybackDecision:{ _,_,_,_ in (decision,caps) },
            mediaSelectionPreparation:.init(native:{ _,_,_ in .init(hasSubtitleOptions:false,apply:{true}) },audio:{_,_ in nil}),
            itemPreparation:.init(ready:{_,_ in },seek:{_,_ in true}),
            releaseHlsSession:{_,_ in },prepareGrowingPlaylist:{_ in },requestHlsSession:{_,_,body in
                calls+=1
                if calls==1 { XCTAssertEqual(body.subtitleBurn,0);return try session("burned") }
                guard initialAttached else {
                    XCTFail("unexpected recovery during initial fixture attachment")
                    throw URLError(.badServerResponse)
                }
                if queued,holdNextChange { XCTAssertEqual(body.subtitleBurn,0) }
                else { XCTAssertNil(body.subtitleBurn) }
                if holdNextChange {
                    holdNextChange=false
                    return try await withCheckedThrowingContinuation { release=$0;entered.fulfill() }
                }
                return try session("subtitle-off-queued")
            },requestHlsStatus:{_,id in PlaybackSessionStatus(id:id)})
        captionOwners.append(controller.progressiveCaptions);textOwners.append(controller.subtitleTextTranslation)
        defer { release?.resume(throwing:CancellationError());controller.stop() }
        let playbackTransport=try XCTUnwrap(transport)
        let model=AppModel(startServices:false,makeAPI:{ _ in PlurxAPI(origin:"https://captions.test",testTransport:playbackTransport) })
        controller.start(model:model,itemId:7,fileId:7,startMs:0,durationMs:120000,title:"Burned fixture",selection:PrePlaySelection(audioIndex:2,subtitleIndex:0))
        try await eventually { calls==1 && controller.player.currentItem != nil && !controller.isChangingStream }
        initialAttached=true
        let incumbent=try XCTUnwrap(controller.player.currentItem)
        let captions=controller.progressiveCaptions
        captions.retire()
        let loaded=expectation(description:"caption options")
        captions.$options.compactMap{$0}.prefix(1).sink{_ in loaded.fulfill()}.store(in:&subscriptions)
        captions.configure(api:PlurxAPI(origin:"https://captions.test",testTransport:transport),fileId:7,sourceAudioIndex:2,
                           now:{(65000,0,1,false)},current:{true})
        await fulfillment(of:[loaded],timeout:2);captions.spokenLanguage="en"
        let before=fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/subtitle-translation")==true}.count
        holdNextChange=true
        if queued { controller.selectQuality(720);await fulfillment(of:[entered],timeout:2) }
        if generate { controller.selectProgressiveCaptions(.transcribe) } else { controller.selectProgressiveAsset(publishedAsset) }
        if !queued { await fulfillment(of:[entered],timeout:2) }
        XCTAssertFalse(captions.selected);XCTAssertTrue(controller.player.currentItem===incumbent)
        XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/subtitle-translation")==true}.count,before)
        if outcome=="newer-off" { controller.selectSubtitle(nil) }
        if queued { try await eventually { !controller.progressiveCaptionOutputSelected } }
        let continuation=try XCTUnwrap(release);release=nil
        if outcome=="failure" || outcome=="queued-failure" { continuation.resume(throwing:URLError(.cannotConnectToHost)) }
        else { continuation.resume(returning:try session("subtitle-off")) }
        if outcome=="success" {
            try await eventually { captions.selected }
            XCTAssertFalse(controller.player.currentItem===incumbent)
            XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/subtitle-translation")==true}.count,before+(generate ? 1:0))
        } else {
            try await eventually { !controller.isChangingStream && !controller.progressiveCaptionOutputSelected }
            XCTAssertFalse(captions.selected)
            XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/subtitle-translation")==true}.count,before)
            if outcome=="failure" || outcome=="queued-failure" { XCTAssertTrue(controller.player.currentItem===incumbent) }
            if outcome=="queued" { XCTAssertGreaterThanOrEqual(calls,3,"the existing reopen owner still drains the queued removal") }
        }
    }
    func testBurnRemovalFailureNeverStartsSpeechOrPublishedCaptions() async throws {
        for generate in [true,false] { try await burnHandoff("failure",generate:generate) }
    }
    func testBurnRemovalMustAttachBeforeSpeechOrPublishedCaptionsStart() async throws {
        for generate in [true,false] { try await burnHandoff("success",generate:generate) }
    }
    func testQueuedRemovalAndNewerOffCannotAdoptOldCaptionIntent() async throws {
        for outcome in ["queued","queued-failure","newer-off"] { for generate in [true,false] { try await burnHandoff(outcome,generate:generate) } }
    }

    func testStopAccessRetirementKeepsAuthenticationStatus() async throws {
        await configure();owner.spokenLanguage="en";XCTAssertTrue(owner.request(.transcribe))
        try await eventually { !self.owner.busy && self.owner.window != nil }
        CaptionFixtureProtocol.statusOverrides=["/stop":403]
        owner.stop();try await eventually { self.owner.options == nil }
        XCTAssertFalse(owner.busy);XCTAssertFalse(owner.selected)
        XCTAssertTrue(owner.status.contains("Caption access ended"))
        XCTAssertFalse(owner.status.contains("Retry Stop"))
    }

    func testNetworkDrainWaitsForCancelledWorkAndCapturedCleanup() async throws {
        let tasks=CaptionNetworkTasks(),entered=expectation(description:"work entered"),cancelled=expectation(description:"work cancelled"),cleanupEntered=expectation(description:"cleanup entered")
        var finishWork:CheckedContinuation<Void,Never>?,finishCleanup:CheckedContinuation<Void,Never>?
        tasks.run {
            await withTaskCancellationHandler {
                await withCheckedContinuation { finishWork=$0;entered.fulfill() }
            } onCancel:{ cancelled.fulfill() }
        }
        tasks.run(cleanup:true) { await withCheckedContinuation { finishCleanup=$0;cleanupEntered.fulfill() } }
        await fulfillment(of:[entered,cleanupEntered],timeout:2)
        tasks.cancelWork();await fulfillment(of:[cancelled],timeout:2)
        var drainFinished=false
        let drainEntered=expectation(description:"drain entered")
        let drain=Task { drainEntered.fulfill();await tasks.drain();drainFinished=true }
        await fulfillment(of:[drainEntered],timeout:2)
        XCTAssertFalse(drainFinished,"drain must wait while work and cleanup are held")
        XCTAssertEqual(tasks.pendingCount,2,"cancelled work remains owned until it settles")
        let work=try XCTUnwrap(finishWork);work.resume()
        try await eventually { tasks.pendingCount==1 }
        XCTAssertEqual(tasks.pendingCount,1,"best-effort cleanup also belongs to transport retirement")
        XCTAssertFalse(drainFinished,"drain must also await captured cleanup after work settles")
        let cleanup=try XCTUnwrap(finishCleanup);cleanup.resume()
        await drain.value;XCTAssertTrue(drainFinished);XCTAssertEqual(tasks.pendingCount,0)
    }

}
