import Foundation
import XCTest
@testable import plurx

private final class DecisionHTTP: URLProtocol {
    static var answer: ((URLRequest) throws -> (URL, Int, [String: String], Data)?)!
    static var stopped: (() -> Void)?
    private var blocked = false
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            guard let (url, status, headers, body) = try Self.answer(request) else { blocked = true; return }
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: headers)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() { if blocked { Self.stopped?() } }
}
final class SharedDecisionClientTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "9007199254740993", itemId: "9223372036854775807")
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/" + String(repeating: "L", count: 236) }
    private var configuration: URLSessionConfiguration { let value = URLSessionConfiguration.ephemeral; value.protocolClasses = [DecisionHTTP.self]; return value }
    override func setUp() { Session.shared.setCredentials(origin: "https://b.test", token: "decision-bearer") }
    override func tearDown() { DecisionHTTP.answer = nil; DecisionHTTP.stopped = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func binding(_ file: String, lifecycle: Any = Int64.max) throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return ["item": try JSONSerialization.jsonObject(with: encoder.encode(ref)), "file_id": file, "revision": String(repeating: "a", count: 64), "lifecycle_generation": lifecycle]
    }
    private func context(_ file: String = "0", lifecycle: Any = Int64.max, change: (inout [String: Any]) -> Void = { _ in }) async throws -> PlaybackFileContext {
        var detail: [String: Any] = ["lifecycle_generation": lifecycle, "files": [["file_id": file, "revision": String(repeating: "a", count: 64), "file_base": base, "reference": try binding(file, lifecycle: lifecycle)]]]
        change(&detail); let data = try JSONSerialization.data(withJSONObject: detail)
        DecisionHTTP.answer = { ($0.url!, 200, [:], data) }
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: file, testTransport: URLSession(configuration: configuration))
    }
    private func wire(_ file: String = "0", change: (inout [String: Any]) -> Void = { _ in }) throws -> Data {
        var result: [String: Any] = ["file_id": file, "reference": try binding(file), "method": "remux", "play_url": base + "/stream.mp4?audio=2", "delivery": ["mode": "remux", "url": base + "/stream.mp4?audio=2", "sessions_url": base + "/hls/sessions", "requires_hls": true, "preserve_dolby_vision": true, "audio": 2], "source": ["hdr": "dolby_vision", "container": "mkv", "width": 3840], "reasons": ["container"], "audio": [["index": 2, "codec": "aac", "channels": 6, "default": false]], "subtitles": [["index": 4, "codec": "hdmv_pgs_subtitle", "default": false, "forced": false, "text": false]], "selection": ["audio_index": 2, "subtitle_index": -1], "audio_offset_ms": -500, "delivered_dynamic_range": "dolby_vision", "delivered_dolby_vision_profile": 8, "quality_candidate_id": "candidate", "future": ["exact": Int64.max]]
        change(&result); return try JSONSerialization.data(withJSONObject: result)
    }
    private func body(_ request: URLRequest) throws -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { throw APIError.badURL }; stream.open(); defer { stream.close() }
        var result = Data(), buffer = [UInt8](repeating: 0, count: 4096)
        while true { let count = stream.read(&buffer, maxLength: buffer.count); if count == 0 { break }; guard count > 0 else { throw APIError.badURL }; result.append(contentsOf: buffer.prefix(count)) }
        return result
    }
    func testActualV2BuilderExactSourceStringsAndTypedNegotiation() async throws {
        for file in ["0", "9007199254740993", "9223372036854775807"] {
            let context = try await context(file), response = try wire(file); var sent: Data?
            DecisionHTTP.answer = { request in
                XCTAssertEqual(request.httpMethod, "POST"); XCTAssertEqual(request.url?.host, "b.test"); XCTAssertEqual(request.url?.path, self.base + "/decision")
                XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer decision-bearer"); sent = try self.body(request)
                return (request.url!, 200, [:], response)
            }
            let result = try await SharedDecisionClient(testConfiguration: configuration).decision(context: context, selection: .init(audioIndex: 2, subtitleIndex: -1), quality: .original, audioOffsetMs: -500)
            let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
            struct Body: Decodable { let caps: DeviceCaps }
            XCTAssertEqual(try decoder.decode(Body.self, from: XCTUnwrap(sent)).caps, result.caps); XCTAssertEqual(result.caps.v, 2)
            XCTAssertEqual(result.decision.fileId, file); XCTAssertEqual(context.lifecycleGeneration, Int64.max)
            XCTAssertEqual(result.decision.presentation.delivery?.audio, 2); XCTAssertEqual(result.decision.presentation.delivery?.requiresHls, true)
            XCTAssertEqual(result.decision.presentation.audio?.first?.channels, 6); XCTAssertEqual(result.decision.presentation.subtitles?.first?.index, 4)
            XCTAssertEqual(result.decision.presentation.deliveredDolbyVisionProfile, 8); XCTAssertEqual(result.decision.presentation.audioOffsetMs, -500)
            XCTAssertEqual(result.decision.wire["future"]?.object?["exact"], .integer(Int64.max)); XCTAssertThrowsError(try context.path("direct"))
        }
    }
    func testLifecycleAndLocatorAreRequiredWithoutLocalFallback() async throws {
        for invalid in [0, -1, 1.5, "1", true] as [Any] { do { _ = try await context(lifecycle: invalid); XCTFail("accepted invalid lifecycle") } catch {} }
        for missing in ["lifecycle_generation", "files"] { do { _ = try await context { $0.removeValue(forKey: missing) }; XCTFail("accepted missing lifecycle/file") } catch {} }
        for bad in ["/api/v1/files/0", base + "A", String(base.dropLast()), "https://a.test" + base] {
            do { _ = try await context { detail in var files = detail["files"] as! [[String: Any]]; files[0]["file_base"] = bad; detail["files"] = files }; XCTFail("accepted malformed alias") } catch {}
        }
        do { _ = try await context { detail in var files = detail["files"] as! [[String: Any]]; var b = files[0]["reference"] as! [String: Any]; b["lifecycle_generation"] = 1; files[0]["reference"] = b; detail["files"] = files }; XCTFail("accepted mismatched lifecycle") } catch {}
    }
    func testBoundedResponseAndForeignDecisionRefuse() async throws {
        let context = try await context(), caps = Caps.snapshot().document
        for size in [4_194_304, 4_194_305] {
            var data = try wire { $0["large_future"] = String(repeating: "x", count: 1_048_577) }; data.append(Data(repeating: 32, count: size - data.count))
            DecisionHTTP.answer = { ($0.url!, 200, [:], data) }
            do { let answer = try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: caps); XCTAssertEqual(size, 4_194_304); XCTAssertNotNil(answer.decision.wire["large_future"]) } catch { XCTAssertEqual(size, 4_194_305) }
        }
        for status in [302, 401, 403, 409, 500] { var calls = 0; DecisionHTTP.answer = { calls += 1; return ($0.url!, status, ["Location": "https://a.test"], Data()) }; do { _ = try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: caps); XCTFail("accepted refusal") } catch {}; XCTAssertEqual(calls, 1) }
        for key in ["file_id", "lifecycle_generation", "revision"] {
            let bad = try wire { var binding = $0["reference"] as! [String: Any]; binding[key] = key == "lifecycle_generation" ? 1 : "7"; $0["reference"] = binding }
            DecisionHTTP.answer = { ($0.url!, 200, [:], bad) }; do { _ = try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: caps); XCTFail("accepted foreign reference") } catch {}
        }
        DecisionHTTP.answer = { _ in (URL(string: "https://a.test" + self.base + "/decision")!, 200, [:], try self.wire()) }; do { _ = try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: caps); XCTFail("accepted foreign response URL") } catch {}
    }
    func testActualBlockedTaskCancelsOnAccountChangeAndCancellation() async throws {
        for accountChange in [true, false] {
            let context = try await context(), started = expectation(description: "request started"), stopped = expectation(description: "actual URLProtocol stopped")
            DecisionHTTP.answer = { _ in started.fulfill(); return nil }; DecisionHTTP.stopped = { stopped.fulfill() }
            let task = Task { try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: Caps.snapshot().document) }
            await fulfillment(of: [started], timeout: 3)
            if accountChange { Session.shared.setCredentials(origin: "https://new.test", token: "new-bearer") } else { task.cancel() }
            do { _ = try await task.value; XCTFail("published cancelled decision") } catch {}
            await fulfillment(of: [stopped], timeout: 3)
            if accountChange { XCTAssertEqual(Session.shared.credentials.token, "new-bearer"); Session.shared.setCredentials(origin: "https://b.test", token: "decision-bearer") }
            DecisionHTTP.stopped = nil
        }
    }
    func testClosedQueryOldContextAndRequestBodyCannotIssueRequest() async throws {
        let context = try await context(), caps = Caps.snapshot().document; var calls = 0
        DecisionHTTP.answer = { request in calls += 1; return (request.url!, 200, [:], try self.wire()) }
        let client = try SharedDecisionClient(testConfiguration: configuration)
        for query in [[URLQueryItem(name: "token", value: "x")], [URLQueryItem(name: "audio", value: "01")], [URLQueryItem(name: "subtitle", value: "-2")], [URLQueryItem(name: "audio_offset_ms", value: "15001")], [URLQueryItem(name: "audio", value: "1"), URLQueryItem(name: "audio", value: "2")]] {
            do { _ = try await client.decisionForTest(context: context, caps: caps, query: query); XCTFail("accepted unapproved query") } catch {}
        }
        var oldVersion = caps; oldVersion.v = 1
        do { _ = try await client.decisionForTest(context: context, caps: oldVersion); XCTFail("accepted v1 caps") } catch {}
        let large = DeviceCaps(client: .init(kind: caps.client.kind, build: caps.client.build, ua: String(repeating: "x", count: 131_073)), video: caps.video, audio: caps.audio, containers: caps.containers, transports: caps.transports, dvTransport: caps.dvTransport, display: caps.display)
        do { _ = try await client.decisionForTest(context: context, caps: large); XCTFail("accepted oversized caps") } catch {}
        Session.shared.setCredentials(origin: "https://b.test", token: "replacement")
        do { _ = try await SharedDecisionClient(testConfiguration: configuration).decisionForTest(context: context, caps: caps); XCTFail("accepted retired context") } catch {}
        XCTAssertEqual(calls, 0)
    }

    // Synthetic ordinary B reply through actual authenticated URLProtocol I/O;
    // this is client protocol evidence, not physical Source playback.
    func testAuthenticatedInitialStartRetainsWholeRequestAndBContext() async throws {
        let context = try await context("9223372036854775807"), session = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
        let reply: [String: Any] = ["session_id": session, "playlist_url": "/api/v1/hls/\(session)/master.m3u8?native=1&subtitle=2", "vod": true,
            "start_seconds": 0.0, "duration_ms": 90_000, "control": ["protocol": "plurx-playback-control-v1", "url": "/api/v1/hls/\(session)/control",
            "generation": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "control_epoch": 1, "next_exchange_ms": 5_000, "lease_timeout_ms": 300_000],
            "future": ["exact": Int64.max]]
        let bytes = try JSONSerialization.data(withJSONObject: reply)
        var sent: Data?
        DecisionHTTP.answer = { request in
            XCTAssertEqual(request.url?.path, self.base + "/hls/sessions"); XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer decision-bearer")
            XCTAssertEqual(request.timeoutInterval, 310); sent = try self.body(request)
            return (request.url!, 200, [:], bytes)
        }
        let request = CreateSessionRequest(playbackId: "shared-browser", requestId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc", height: 720, start: 12.5, audio: 2, copy: true, caps: Caps.snapshot().document)
        let result = try await SharedDecisionClient(testConfiguration: configuration).start(context: context, request: request)
        let subject = SharedPlaybackSubject(context: context, title: "Shared film", resumeMs: 12_500, watchSequence: 7)
        let sdrDecision = try SharedDecision.decode(wire("9223372036854775807") { $0["delivered_dynamic_range"] = "sdr"; $0["preserve_dolby_vision"] = false })
        let plan = try SharedPlaybackPlan(subject: subject, decision: sdrDecision, caps: request.caps!, request: request)
        XCTAssertEqual(plan.request.height, 720); XCTAssertEqual(plan.request.start, 12.5)
        XCTAssertEqual(plan.subject.context.sourceFileId, "9223372036854775807")
        var wrong = request; wrong.copy = false
        XCTAssertThrowsError(try SharedPlaybackPlan(subject: subject, decision: sdrDecision, caps: request.caps!, request: wrong))
        wrong = request; wrong.previousSessionId = ""
        XCTAssertThrowsError(try SharedPlaybackPlan(subject: subject, decision: sdrDecision, caps: request.caps!, request: wrong))
        XCTAssertThrowsError(try SharedPlaybackSubject(context: .local(0), title: "Local", resumeMs: 0, watchSequence: 0).validate())

        XCTAssertEqual(result.context.sourceFileId, "9223372036854775807"); XCTAssertEqual(result.context.reference, ref); XCTAssertEqual(result.context.sessionId, session)
        XCTAssertEqual(result.request.start, 12.5); XCTAssertEqual(result.request.height, 720); XCTAssertEqual(result.request.caps, request.caps)
        let raw = try JSONSerialization.jsonObject(with: XCTUnwrap(sent)) as! [String: Any]
        XCTAssertEqual(raw["request_id"] as? String, request.requestId); XCTAssertNil(raw["intent"]); XCTAssertNil(raw["previous_session_id"])
        XCTAssertEqual(result.start.wire["future"]?.object?["exact"], .integer(Int64.max)); XCTAssertThrowsError(try result.context.localID())
        let client = try SharedDecisionClient(testConfiguration: configuration)
        let beat = SharedProgressBeat(sessionId: session, sequence: 7, positionMs: 0, durationMs: 90_000, watched: false)
        var progressSent: Data?
        DecisionHTTP.answer = { request in
            XCTAssertEqual(request.url?.path, "/api/v1/shared/imports/\(self.ref.importId)/items/\(self.ref.itemId)/progress")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer decision-bearer")
            let encoded = try self.body(request)
            if let progressSent { XCTAssertEqual(progressSent, encoded) }; progressSent = encoded
            return (request.url!, 409, [:], Data("{\"code\":\"sharing_progress_stale\",\"current_sequence\":20}".utf8))
        }
        for _ in 0..<2 { let outcome = try await client.progress(playback: result, beat: beat); XCTAssertEqual(outcome, .resyncRequired(currentSequence: 20)) }
        let progressBody = try JSONSerialization.jsonObject(with: XCTUnwrap(progressSent)) as! [String: Any]
        XCTAssertEqual(Set(progressBody.keys), ["session_id", "sequence", "position_ms", "duration_ms", "watched"])
        XCTAssertEqual(progressBody["position_ms"] as? Int, 0)
        DecisionHTTP.answer = { ($0.url!, 409, [:], Data("{\"code\":\"sharing_progress_stale\",\"current_sequence\":\"20\"}".utf8)) }
        do { _ = try await client.progress(playback: result, beat: beat); XCTFail("accepted string sequence") } catch {}
        DecisionHTTP.answer = { ($0.url!, 200, [:], Data(repeating: 32, count: 16_385)) }
        do { _ = try await client.progress(playback: result, beat: beat); XCTFail("accepted oversized progress ACK") } catch {}
        var sequences: [Int] = []
        DecisionHTTP.answer = { request in
            let object = try JSONSerialization.jsonObject(with: self.body(request)) as! [String: Any]
            sequences.append(object["sequence"] as! Int)
            return (request.url!, 200, [:], Data("{}".utf8))
        }
        _ = try await client.orderedProgress(playback: result, initialWatchSequence: 7, positionMs: 0, durationMs: 90_000)
        _ = try await SharedDecisionClient(testConfiguration: configuration).orderedProgress(playback: result, initialWatchSequence: 0, positionMs: 500, durationMs: 90_000)
        XCTAssertEqual(sequences, [8, 9])
        let other = SharedPlaybackReference(importId: "dddddddd-dddd-4ddd-8ddd-dddddddddddd", serverId: ref.serverId, catalogueEpoch: ref.catalogueEpoch, libraryId: "0", itemId: ref.itemId)
        let otherBase = "/api/v1/shared/imports/\(other.importId)/files/" + String(repeating: "L", count: 236)
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        var otherBinding = try binding("0"); otherBinding["item"] = try JSONSerialization.jsonObject(with: encoder.encode(other))
        let detail = try JSONSerialization.data(withJSONObject: ["lifecycle_generation": Int64.max, "files": [["file_id": "0", "revision": String(repeating: "a", count: 64), "file_base": otherBase, "reference": otherBinding]]])
        DecisionHTTP.answer = { ($0.url!, 200, [:], detail) }
        let otherContext = try await PlaybackFileContext.authenticatedDetail(reference: other, fileId: "0", testTransport: URLSession(configuration: configuration))
        let otherSession = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        var otherReply = reply; otherReply["session_id"] = otherSession; otherReply["playlist_url"] = "/api/v1/hls/\(otherSession)/master.m3u8"
        var control = otherReply["control"] as! [String: Any]; control["url"] = "/api/v1/hls/\(otherSession)/control"; otherReply["control"] = control
        let otherBytes = try JSONSerialization.data(withJSONObject: otherReply)
        DecisionHTTP.answer = { ($0.url!, 200, [:], otherBytes) }
        let otherPlayback = try await client.start(context: otherContext, request: request)
        DecisionHTTP.answer = { request in
            XCTAssertEqual(request.url?.path, "/api/v1/shared/imports/\(other.importId)/items/\(other.itemId)/progress")
            let object = try JSONSerialization.jsonObject(with: self.body(request)) as! [String: Any]
            sequences.append(object["sequence"] as! Int)
            return (request.url!, 200, [:], Data("{}".utf8))
        }
        _ = try await client.orderedProgress(playback: otherPlayback, initialWatchSequence: 0, positionMs: 1000, durationMs: 90_000)
        XCTAssertEqual(sequences, [8, 9, 10])



    }
    func testInitialStartRefusesUnsupportedOriginalFieldsBeforeNetwork() async throws {
        let context = try await context(), client = try SharedDecisionClient(testConfiguration: configuration); var calls = 0
        DecisionHTTP.answer = { _ in calls += 1; return nil }
        let base = CreateSessionRequest(playbackId: "shared", requestId: "cccccccc-cccc-4ccc-8ccc-cccccccccccc", caps: Caps.snapshot().document)
        for index in 0..<6 {
            var request = base
            switch index {
            case 0: request.previousSessionId = ""
            case 1: request.controlSequence = 0
            case 2: request.reopenReason = "stall"
            case 3: request.subtitleBurn = 0
            case 4: request.preserveDolbyVision = true
            default: request.requestId = request.requestId!.uppercased()
            }
            do { _ = try await client.start(context: context, request: request); XCTFail("accepted unsupported initial Start") } catch {}
        }
        XCTAssertEqual(calls, 0)
    }

}
