import AVFoundation
import Combine
import Foundation
import XCTest
@testable import plurx

/// The renderer boundary owns the exact items handed to it, without asking
/// AVFoundation to load fake network playlists or trigger transport recovery.
private final class CaptionHandoffItem: AVPlayerItem {
    var time = CMTime.zero
    override var status: AVPlayerItem.Status { .readyToPlay }
    override func currentTime() -> CMTime { time }
}
private final class CaptionHandoffPlayer: AVPlayer {
    private var attached: AVPlayerItem?
    private var observedRate: Float = 0
    var simulatedExternalPlayback = false
    override var isExternalPlaybackActive: Bool { simulatedExternalPlayback }
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
    var invalidCue = false
    var largeCueWindow = false
    var oversizedOptions = false
    var completeOperation = false
    var offeredRoute = "audio"
    var terminalState: String?
    var defaultProfileID: String?
    let asset = String(repeating: "a", count: 64)
    let profileID = String(repeating: "b", count: 64)
    let source = String(repeating: "c", count: 64)
    let model = String(repeating: "d", count: 64)
    let operation = "11111111-1111-4111-8111-111111111111"
    let demand = "22222222-2222-4222-8222-222222222222"
    let lease = "33333333-3333-4333-8333-333333333333"
    let cueID = String(repeating: "f", count: 64)
    func data(_ value: Any) -> Data { try! JSONSerialization.data(withJSONObject: value, options: [.sortedKeys]) }
    var profile: [String: Any] {
        ["profile_id": profileID, "model_id": "small", "model_sha256": model, "task": "transcribe", "output_language": NSNull(),
         "input_languages": ["en", "de"], "timeline_contract": "source-audio-clock-v1", "window": ["core_ms": 30000],
         "limits": ["segment_bytes": 16384, "segment_cues": 128, "page_bytes": 65536, "page_entries": 64,
                    "coverage_intervals": 64, "concurrent_fetches": 2, "cue_memory_bytes": 524288, "cue_count": 1024,
                    "cue_window_before_ms": 30000, "cue_window_after_ms": 90000, "poll_min_ms": 1000,
                    "poll_max_ms": 5000, "retry_max_ms": 30000, "heartbeat_interval_ms": 5000]]
    }
    var retainedOptions: [String: Any] {
        ["version": 1, "file_id": "7", "source_size": "10000", "source_mtime": "200", "duration_ms": 120000,
         "audio_tracks": [["index": 2, "audio_ordinal": 2, "source_index": 5, "normalized_language": "en"]],
         "models": [profile], "default_profile_id": defaultProfileID.map { $0 as Any } ?? NSNull(),
         "capability": ["supported": true], "permissions": ["read": true, "demand": true, "stop": true, "resume": true],
         "assets": [], "server_now_ms": 100000]
    }

    var retainedOperation: [String: Any] {
        ["version": 1, "operation_id": operation, "operation_epoch": "9007199254740994", "view_revision": "2",
         "state": "stopped", "retryable": false, "asset_id": NSNull(), "content_revision": NSNull(),
         "file_id": "7", "source_size": "10000", "source_mtime": "200", "audio_index": 2,
         "spoken_language": "en", "output_language": "en", "task": "transcribe", "profile_id": profileID,
         "model_id": "small", "model_sha256": model, "timeline_contract": "source-audio-clock-v1",
         "failed_ranges": [], "server_now_ms": 100000]
    }

    func reply(_ request: URLRequest) -> Data {
        lock.lock(); defer { lock.unlock() }
        requests.append(request)
        let path = request.url!.path
        let filePath = "/api/v1/files/7/ai-subtitles"
        let operationPath = "/api/v1/ai-subtitles/operations/" + operation
        if path == filePath && request.httpMethod == "GET" {
            if oversizedOptions { return Data(repeating: 120, count: 70_000) }
            return data(["allowed": true, "target": "en", "route": offeredRoute, "alternatives": []])
        }
        let state = terminalState ?? (completeOperation ? "complete" : "working")
        let trackID: Any = completeOperation ? asset as Any : NSNull()
        if path == operationPath + "/cues" {
            let from = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?
                .queryItems?.first(where: { $0.name == "from_ms" })?.value.flatMap(Int.init) ?? 0
            let cues: [[String: Any]] = !completeOperation && state == "working" && from < 67000
                ? [["start_ms": invalidCue ? -1 : 64000, "end_ms": 67000, "text": "A literal <caption> & source clock"]] : []
            var window: [String: Any] = ["state": state, "track_id": trackID,
                "ready_from_ms": completeOperation ? 0 : 60000, "ready_to_ms": completeOperation ? 120000 : 90000,
                "cues": cues]
            if largeCueWindow {
                window["cues"] = (0..<96).map { cueIndex in
                    ["start_ms": 60_000 + 50 * cueIndex, "end_ms": 60_050 + 50 * cueIndex,
                     "text": String(repeating: "caption ", count: 90)] as [String: Any]
                }
            }
            return data(window)
        }
        if path == operationPath + "/viewer" && request.httpMethod == "DELETE" { return Data() }
        if (path == filePath && request.httpMethod == "POST") || (path == operationPath + "/viewer" && request.httpMethod == "PUT") {
            return data(["id": operation, "route": offeredRoute, "state": state, "progress": completeOperation ? 1 : 0.25,
                         "track_id": trackID, "track_index": completeOperation ? 100005 as Any : NSNull()])
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
        let status=Self.statusOverrides.first { request.url?.path.hasSuffix($0.key)==true }?.value
            ?? (request.httpMethod == "DELETE" ? 204 : 200)
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() { cancelled = true }
}

@MainActor
final class ProgressiveCaptionTests: XCTestCase {
    func testAIEntryRequiresAnAvailableRouteAndNoExistingEnglishTrack() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let available = Data(#"{"allowed":true,"route":"audio","alternatives":[]}"#.utf8)
        let existing = Data(#"{"allowed":true,"existing_index":0,"alternatives":[]}"#.utf8)
        let denied = Data(#"{"allowed":false,"route":"audio","alternatives":[]}"#.utf8)
        XCTAssertTrue(try decoder.decode(AISubtitleOptions.self, from: available).canStart)
        XCTAssertFalse(try decoder.decode(AISubtitleOptions.self, from: existing).canStart)
        XCTAssertFalse(try decoder.decode(AISubtitleOptions.self, from: denied).canStart)
    }


    private var subscriptions: Set<AnyCancellable> = []
    private var transport: URLSession!
    private var fixture: CaptionFixture!
    private var owner: ProgressiveCaptionSession!
    private var captionOwners:[ProgressiveCaptionSession]=[]
    override func setUp() {
        fixture = CaptionFixture(); CaptionFixtureProtocol.fixture = fixture
        CaptionFixtureProtocol.statusOverrides=[:]
        CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.onHeld=nil;CaptionFixtureProtocol.releaseHeld=nil
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [CaptionFixtureProtocol.self]
        transport = URLSession(configuration: config)
        owner = ProgressiveCaptionSession()
        captionOwners=[owner]
        Session.shared.setCredentials(origin: "https://captions.test", token: "caption-fixture")
    }
    override func tearDown() async throws {
        // Fence every owner first, then settle its work before closing the
        // shared injected transport. URLSession task creation is asynchronous.
        for caption in captionOwners { caption.retire() }
        for caption in captionOwners { await caption.retireAndDrain() }
        subscriptions=[];captionOwners=[]
        transport.invalidateAndCancel()
        CaptionFixtureProtocol.holdRequest=nil;CaptionFixtureProtocol.onHeld=nil;CaptionFixtureProtocol.releaseHeld=nil
        Session.shared.setCredentials(origin:"",token:nil);CaptionFixtureProtocol.fixture=nil
    }
    private func configure(now: @escaping () -> (Int, Int, Double, Bool) = { (65000, 60000, 1.5, false) }) async {
        let loaded = expectation(description: "fresh validated options")
        // Ignore the current owner's replayed value: reconfiguration must wait
        // for the next options response, not return on the previous snapshot.
        owner.$aiOptions.dropFirst().compactMap { $0 }.prefix(1).sink { _ in loaded.fulfill() }.store(in: &subscriptions)
        owner.configure(api: PlurxAPI(origin: "https://captions.test", testTransport: transport), fileId: 7, sourceAudioIndex: 2,
                        now: now, current: { true })
        await fulfillment(of: [loaded], timeout: 2)
    }
    func testNonzeroAudioOrdinalAndProgressiveTranscriptionUseSourceClock() async throws {
        await configure()
        let visible = expectation(description: "validated partial cue")
        owner.$window.compactMap { $0 }.filter { $0.cues.count == 1 }.prefix(1).sink { _ in visible.fulfill() }.store(in: &subscriptions)
        XCTAssertTrue(owner.requestAI { _, _ in })
        await fulfillment(of: [visible], timeout: 2)
        let window = try XCTUnwrap(owner.window), cue = try XCTUnwrap(window.cues.first)
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: window.baseMs, offsetMs: 0), 4000..<7000)
        XCTAssertEqual(cue.text, "A literal <caption> & source clock")
        let create = try XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" })
        let body = try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(create.httpBody,"CREATE must carry its exact serialized body")) as? [String: Any])
        XCTAssertEqual(body["audio_index"] as? Int, 2)
        XCTAssertEqual(body["route"] as? String, "auto")
        XCTAssertEqual(body["target"] as? String, "en")
        XCTAssertEqual(create.url?.path, "/api/v1/files/7/ai-subtitles")
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
        owner.subtitleOffsetMs = 250
        XCTAssertEqual(CaptionRenderWindow.itemInterval(cue, baseMs: 60000, offsetMs: owner.window!.offsetMs), 4250..<7250)
        XCTAssertEqual(body["position_ms"] as? Int, 65000)
    }
    private func body(_ request: URLRequest) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: XCTUnwrap(request.httpBody)) as? [String: Any])
    }
    func testAdvancingPlaybackKeepsViewerAcrossCoreBoundaryPauseRateAndAttachment() async throws {
        var position = 65000, base = 60000
        var rate = 1.0, paused = false
        await configure(now: { (position, base, rate, paused) })
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.window?.cues.count == 1 }
        let create = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" }))
        let playback = try XCTUnwrap(create["playback_id"] as? String)
        XCTAssertTrue(CaptionValidation.uuid(playback))

        position = 95000; base = 90000; rate = 1.5; paused = true
        owner.mediaChanged()
        try await eventually {
            self.fixture.captured.contains { $0.httpMethod == "PUT" && (try? self.body($0)["position_ms"] as? Int) == 95000 }
        }
        let updates = try fixture.captured.filter { $0.httpMethod == "PUT" }.map(body)
        XCTAssertTrue(updates.allSatisfy { $0["playback_id"] as? String == playback })
        XCTAssertTrue(updates.contains { $0["playback_rate"] as? Double == 1.5 })
        XCTAssertEqual(owner.window?.baseMs, 90000)
        XCTAssertTrue(owner.selected)
    }

    func testSeekDuringHeldHeartbeatDiscardsTheOldCueWindow() async throws {
        var position = 65000
        await configure(now: { (position, 60000, 1, false) })
        let held = expectation(description: "old viewer update held")
        CaptionFixtureProtocol.holdRequest = (method: "PUT", path: "/api/v1/ai-subtitles/operations/\(fixture.operation)/viewer")
        CaptionFixtureProtocol.onHeld = { held.fulfill() }
        XCTAssertTrue(owner.requestAI { _, _ in })
        await fulfillment(of: [held], timeout: 2)
        let first = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "PUT" }))
        var showedOldCue = false
        owner.$window.sink { if $0?.cues.isEmpty == false { showedOldCue = true } }.store(in: &subscriptions)
        position = 95000
        owner.mediaChanged(seek: true, positionMs: position)
        CaptionFixtureProtocol.holdRequest = nil
        CaptionFixtureProtocol.releaseHeld?()
        try await eventually {
            self.fixture.captured.contains { $0.url?.path.hasSuffix("/cues") == true && $0.url?.query?.contains("from_ms=85000") == true }
        }
        let updates = try fixture.captured.filter { $0.httpMethod == "PUT" }.map(body)
        XCTAssertEqual(updates.last?["playback_id"] as? String, first["playback_id"] as? String)
        XCTAssertEqual(updates.last?["position_ms"] as? Int, 95000)
        XCTAssertFalse(showedOldCue)
        XCTAssertTrue(owner.selected)
    }

    func testRetirementThenRestartCreatesANewViewer() async throws {
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.window?.cues.count == 1 }
        await owner.retireAndDrain()
        let create = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" }))
        let release = try XCTUnwrap(fixture.captured.first { $0.httpMethod == "DELETE" })
        let playback = try XCTUnwrap(create["playback_id"] as? String)
        XCTAssertEqual(release.url?.query, "playback_id=" + playback)
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.window?.cues.count == 1 }
        let replacement = try body(XCTUnwrap(fixture.captured.last { $0.httpMethod == "POST" }))
        XCTAssertNotEqual(replacement["playback_id"] as? String, playback)
    }

    func testRecoveredOperationIdentityAndNonretryableCoreStatusStayFenced() throws {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let options = try decoder.decode(CaptionOptions.self, from: fixture.data(fixture.retainedOptions))
        var value = fixture.retainedOperation
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
    func testUnifiedRequestUsesExplicitRouteAndLanguageWithServerOwnedModel() async throws {
        await configure()
        XCTAssertTrue(owner.requestAI(route: "audio", language: "de") { _, _ in })
        try await eventually { self.owner.window?.cues.count == 1 }
        let request = try XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" })
        let input = try body(request)
        XCTAssertEqual(input["route"] as? String, "audio")
        XCTAssertEqual(input["spoken_language"] as? String, "de")
        XCTAssertEqual(input["target"] as? String, "en")
        XCTAssertNil(input["profile_id"])
        XCTAssertNil(input["model_id"])
    }

    func testRetainedOptionsRejectUnknownProfileIdentity() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        for invalid in [String(repeating: "9", count: 64), "not-a-profile-digest"] {
            fixture.defaultProfileID = invalid
            let options = try decoder.decode(CaptionOptions.self, from: fixture.data(fixture.retainedOptions))
            XCTAssertThrowsError(try options.validate(file: 7))
        }
        fixture.defaultProfileID = nil
        var legacy = fixture.retainedOptions
        legacy.removeValue(forKey: "default_profile_id")
        let absent = try decoder.decode(CaptionOptions.self, from: fixture.data(legacy))
        XCTAssertNoThrow(try absent.validate(file: 7))
    }

    func testCueWindowCanExceedTheOptionsByteBudget() async throws {
        fixture.largeCueWindow = true
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.window?.cues.count == 96 }
    }

    func testOutOfBoundsCueNeverPublishesText() async throws {
        fixture.invalidCue = true
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.coverageStatus == "Coverage ahead: 25 seconds." }
        XCTAssertTrue(owner.window?.cues.isEmpty == true)
    }

    func testDeselectRevokesPresentationAndViewerBeforeSeek() async throws {
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in })
        try await eventually { self.owner.window?.cues.count == 1 }
        owner.deselect()
        owner.mediaChanged(seek: true)
        try await eventually { self.fixture.captured.contains { $0.httpMethod == "DELETE" } }
        let released = try XCTUnwrap(fixture.captured.last { $0.httpMethod == "DELETE" })
        XCTAssertEqual(released.url?.path, "/api/v1/ai-subtitles/operations/\(fixture.operation)/viewer")
        XCTAssertFalse(owner.selected)
        XCTAssertNil(owner.window)
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
            let _: CaptionHTTPReply<AISubtitleOptions> = try await api.caption("files/7/ai-subtitles")
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
        XCTAssertNil(owner.aiOptions)
    }
    func testTextRouteHandsOffStableStoredTrack() async throws {
        fixture.offeredRoute = "text"
        fixture.completeOperation = true
        await configure()
        let completed = expectation(description: "durable text translation result")
        XCTAssertTrue(owner.requestAI { id, index in
            XCTAssertEqual(id, self.fixture.asset)
            XCTAssertNil(index)
            completed.fulfill()
        })
        await fulfillment(of: [completed], timeout: 2)
        XCTAssertEqual(owner.aiRoute, "text")
        XCTAssertTrue(fixture.captured.contains { $0.url?.path.hasSuffix("/cues") == true })
    }

    func testDownloadRouteHandsOffWithoutStartingAViewerLoop() async throws {
        fixture.offeredRoute = "download"
        fixture.completeOperation = true
        await configure()
        let completed = expectation(description: "existing track selected")
        XCTAssertTrue(owner.requestAI { id, index in
            XCTAssertEqual(id, self.fixture.asset)
            XCTAssertEqual(index, 100005)
            completed.fulfill()
        })
        await fulfillment(of: [completed], timeout: 2)
        await owner.retireAndDrain()
        XCTAssertFalse(fixture.captured.contains { $0.url?.path.contains("/operations/") == true })
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
    private func eventually(file: StaticString = #filePath, line: UInt = #line,
                            _ condition: @MainActor () -> Bool) async throws {
        for _ in 0..<400 {
            if condition() { return }
            try await Task.sleep(for: .milliseconds(10))
        }
        XCTFail("Caption transition did not settle", file: file, line: line)
        throw CaptionProtocolError.expired
    }
    func testLateStartReceiptReleasesSupersededViewerWithoutPresentingCues() async throws {
        await configure()
        let held = expectation(description: "start receipt held")
        CaptionFixtureProtocol.holdRequest = (method: "POST", path: "/api/v1/files/7/ai-subtitles")
        CaptionFixtureProtocol.onHeld = { held.fulfill() }
        XCTAssertTrue(owner.requestAI { _, _ in XCTFail("Superseded work cannot select a track") })
        await fulfillment(of: [held], timeout: 2)
        let create = try body(XCTUnwrap(fixture.captured.first { $0.httpMethod == "POST" }))
        owner.deselect()
        CaptionFixtureProtocol.holdRequest = nil
        CaptionFixtureProtocol.releaseHeld?()
        try await eventually { self.fixture.captured.contains { $0.httpMethod == "DELETE" } }
        let release = try XCTUnwrap(fixture.captured.first { $0.httpMethod == "DELETE" })
        XCTAssertEqual(release.url?.query, "playback_id=" + (try XCTUnwrap(create["playback_id"] as? String)))
        XCTAssertFalse(fixture.captured.contains { $0.url?.path.hasSuffix("/cues") == true })
        XCTAssertFalse(owner.selected)
        XCTAssertNil(owner.window)
    }

    private func burnHandoff(_ outcome: String) async throws {
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
        captionOwners.append(controller.progressiveCaptions)
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
        captions.$aiOptions.compactMap{$0}.prefix(1).sink{_ in loaded.fulfill()}.store(in:&subscriptions)
        captions.configure(api:PlurxAPI(origin:"https://captions.test",testTransport:transport),fileId:7,sourceAudioIndex:2,
                           now:{(65000,0,1,false)},current:{true})
        await fulfillment(of:[loaded],timeout:2)
        let before=fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/ai-subtitles")==true}.count
        holdNextChange=true
        if queued { controller.selectQuality(720);await fulfillment(of:[entered],timeout:2) }
        controller.selectAISubtitles()
        if !queued { await fulfillment(of:[entered],timeout:2) }
        XCTAssertFalse(captions.selected);XCTAssertTrue(controller.player.currentItem===incumbent)
        XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/ai-subtitles")==true}.count,before)
        if outcome=="newer-off" { controller.selectSubtitle(nil) }
        if queued { try await eventually { !controller.progressiveCaptionOutputSelected } }
        let continuation=try XCTUnwrap(release);release=nil
        if outcome=="failure" || outcome=="queued-failure" { continuation.resume(throwing:URLError(.cannotConnectToHost)) }
        else { continuation.resume(returning:try session("subtitle-off")) }
        if outcome=="success" {
            try await eventually { captions.selected && self.fixture.captured.filter { $0.httpMethod == "POST" && $0.url?.path.hasSuffix("/ai-subtitles") == true }.count == before + 1 }
            XCTAssertFalse(controller.player.currentItem===incumbent)
            XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/ai-subtitles")==true}.count,before+1)
        } else {
            try await eventually { !controller.isChangingStream && !controller.progressiveCaptionOutputSelected }
            XCTAssertFalse(captions.selected)
            XCTAssertEqual(fixture.captured.filter{$0.httpMethod=="POST" && $0.url?.path.hasSuffix("/ai-subtitles")==true}.count,before)
            if outcome=="failure" || outcome=="queued-failure" { XCTAssertTrue(controller.player.currentItem===incumbent) }
            if outcome=="queued" { XCTAssertGreaterThanOrEqual(calls,3,"the existing reopen owner still drains the queued removal") }
        }
    }
    func testBurnRemovalFailureNeverStartsAISubtitles() async throws {
        try await burnHandoff("failure")
    }
    func testBurnRemovalMustAttachBeforeAISubtitlesStart() async throws {
        try await burnHandoff("success")
    }
    func testQueuedRemovalAndNewerOffCannotAdoptOldCaptionIntent() async throws {
        for outcome in ["queued","queued-failure","newer-off"] { try await burnHandoff(outcome) }
    }

    private func switchFromGeneratedCaptions(
        to index: Int, externalPlayback: Bool = false, refusedWith notice: String? = nil
    ) async throws {
        let decoder = JSONDecoder()
        let decision = try decoder.decode(Decision.self, from: Data("""
        {"fileId":7,"method":"remux","playUrl":"/not-fetched","deliveredDynamicRange":"hdr10",
         "source":{"height":1080,"durationMs":120000},
         "audio":[{"index":2,"codec":"aac","default":true}],
         "subtitles":[
           {"index":0,"codec":"ass","default":false,"forced":false,"text":true,"native":false},
           {"index":1,"codec":"hdmv_pgs_subtitle","default":false,"forced":false,"text":false,"native":false,"overlay":"pgs-v1"},
           {"index":2,"codec":"subrip","default":false,"forced":false,"text":true,"native":true}
         ],"ladder":[]}
        """.utf8))
        let session = try decoder.decode(HlsStart.self, from: Data("""
        {"sessionId":"hdr","playlistUrl":"/hls/hdr/index.m3u8","mediaOriginMs":0,
         "durationMs":120000,"vod":true,"deliveredDynamicRange":"hdr10"}
        """.utf8))
        let player = CaptionHandoffPlayer()
        let caps = Caps.snapshot().document
        var sessionStarts = 0
        var nativeSelections: [Int?] = []
        let controller = PlayerController(
            player: player, makePlayerItem: { _ in CaptionHandoffItem(asset: AVMutableComposition()) },
            requestPlaybackDecision: { _, _, _, _ in (decision, caps) },
            mediaSelectionPreparation: .init(native: { selection, _, _ in
                .init(hasSubtitleOptions: true, apply: { nativeSelections.append(selection); return true })
            }, audio: { _, _ in nil }),
            itemPreparation: .init(ready: { _, _ in }, seek: { _, _ in true }),
            releaseHlsSession: { _, _ in }, prepareGrowingPlaylist: { _ in },
            requestHlsSession: { _, _, _ in sessionStarts += 1; return session },
            requestHlsStatus: { _, id in PlaybackSessionStatus(id: id) }
        )
        let captions = controller.progressiveCaptions
        captionOwners.append(captions)
        defer {
            // Fixture time only drives the caption window. A zero position
            // keeps stop's detached progress report off the test transport.
            (player.currentItem as? CaptionHandoffItem)?.time = .zero
            controller.stop()
        }
        let playbackTransport = try XCTUnwrap(transport)
        let model = AppModel(startServices: false, makeAPI: { _ in
            PlurxAPI(origin: "https://captions.test", testTransport: playbackTransport)
        })
        controller.start(
            model: model, itemId: 7, fileId: 7, startMs: 0, durationMs: 120000, title: "HDR fixture",
            selection: PrePlaySelection(audioIndex: 2, subtitleIndex: PrePlaySelection.subtitleOff)
        )
        try await eventually { player.currentItem != nil && !controller.isChangingStream && captions.aiOptions != nil }
        let incumbent = try XCTUnwrap(player.currentItem as? CaptionHandoffItem)
        incumbent.time = CMTime(seconds: 65, preferredTimescale: 1000)
        controller.selectAISubtitles()
        try await eventually { captions.window?.cues.count == 1 }
        let window = try XCTUnwrap(captions.window)
        let cue = try XCTUnwrap(window.cues.first)
        let interval = try XCTUnwrap(CaptionRenderWindow.itemInterval(cue, baseMs: window.baseMs, offsetMs: window.offsetMs))
        XCTAssertTrue(interval.contains(65000))
        XCTAssertTrue(controller.progressiveCaptionOutputSelected)
        XCTAssertFalse(player.allowsExternalPlayback)
        XCTAssertEqual(controller.deliveredRange, "hdr10")

        func demandRequests(_ method: String) -> [URLRequest] {
            fixture.captured.filter { $0.httpMethod == method && $0.url?.path.hasSuffix("/operations/" + fixture.operation + "/viewer") == true }
        }
        let selectionCount = nativeSelections.count
        let cueReadCount = fixture.captured.filter { $0.url?.path.hasSuffix("/cues") == true }.count
        player.simulatedExternalPlayback = externalPlayback
        controller.selectSubtitle(index)
        if let notice {
            XCTAssertEqual(controller.surface.surface.detail, notice)
            XCTAssertNil(controller.selectedSubtitle)
            XCTAssertTrue(controller.progressiveCaptionOutputSelected)
            XCTAssertTrue(captions.selected)
            XCTAssertEqual(captions.window?.cues, window.cues)
            XCTAssertFalse(player.allowsExternalPlayback)
            XCTAssertTrue(player.currentItem === incumbent)
            XCTAssertEqual(controller.deliveredRange, "hdr10")
            XCTAssertEqual(nativeSelections.count, selectionCount)
            try await eventually { self.fixture.captured.filter { $0.url?.path.hasSuffix("/cues") == true }.count > cueReadCount }
            XCTAssertTrue(demandRequests("DELETE").isEmpty)
            controller.selectSubtitle(nil)
        }

        let replacement: Int? = notice == nil ? index : nil
        try await eventually { nativeSelections.count == selectionCount + 1 && demandRequests("DELETE").count == 1 }
        XCTAssertEqual(nativeSelections.last, .some(replacement))
        XCTAssertEqual(controller.selectedSubtitle, replacement)
        XCTAssertFalse(controller.progressiveCaptionOutputSelected)
        XCTAssertNil(captions.window)
        XCTAssertTrue(player.allowsExternalPlayback)
        XCTAssertTrue(player.currentItem === incumbent)
        XCTAssertEqual(controller.deliveredRange, "hdr10")
        XCTAssertEqual(sessionStarts, 1)
        XCTAssertFalse(fixture.captured.contains { $0.url?.path.contains("/subtitle-text-translation") == true })
        await captions.retireAndDrain()
        XCTAssertEqual(demandRequests("DELETE").count, 1)
    }

    func testRefusedHDRSubtitlePreservesGeneratedCaptionsUntilOff() async throws {
        try await switchFromGeneratedCaptions(to: 0, refusedWith: PlayerController.hdrSubtitleNotice)
    }

    func testRefusedExternalOverlayPreservesGeneratedCaptionsUntilOff() async throws {
        try await switchFromGeneratedCaptions(
            to: 1, externalPlayback: true, refusedWith: PlayerController.pgsOverlayExternalPlaybackNotice
        )
    }

    func testSupportedNativeSubtitleReplacesGeneratedCaptionsInPlace() async throws {
        try await switchFromGeneratedCaptions(to: 2)
    }

    func testViewerAccessLossClearsPresentation() async throws {
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in XCTFail("An unauthorized viewer cannot select a track") })
        try await eventually { self.owner.window?.cues.count == 1 }
        CaptionFixtureProtocol.statusOverrides = ["/cues": 403]
        try await eventually { !self.owner.selected }
        XCTAssertNil(owner.window)
    }

    func testStoppedOperationEndsPresentationAndReleasesViewer() async throws {
        fixture.terminalState = "stopped"
        await configure()
        XCTAssertTrue(owner.requestAI { _, _ in XCTFail("A stopped operation cannot select a track") })
        try await eventually { !self.owner.selected && self.fixture.captured.contains { $0.httpMethod == "DELETE" } }
        XCTAssertNil(owner.window)
        XCTAssertEqual(owner.status, "AI subtitles stopped.")
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
