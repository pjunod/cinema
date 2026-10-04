import Foundation
import XCTest
@testable import plurx

private final class ControlHTTP: URLProtocol {
    static var answer: ((URLRequest) throws -> (Int, Data))!
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            let (status, body) = try Self.answer(request)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: [:])!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

/// Synthetic B answers through actual authenticated URLProtocol I/O. These are
/// client protocol contracts, not physical Source or device evidence.
@MainActor
final class SharedPlaybackControlTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "7", itemId: "9007199254740993")
    private let session = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private let generation = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    private let instance = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/" + String(repeating: "L", count: 236) }
    private var configuration: URLSessionConfiguration { let value = URLSessionConfiguration.ephemeral; value.protocolClasses = [ControlHTTP.self]; return value }
    override func setUp() async throws { Session.shared.setCredentials(origin: "https://b.test", token: "control-bearer") }
    override func tearDown() async throws { ControlHTTP.answer = nil; Session.shared.setCredentials(origin: "", token: nil) }

    private func binding(_ file: String = "5") throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return ["item": try JSONSerialization.jsonObject(with: encoder.encode(ref)), "file_id": file, "revision": String(repeating: "a", count: 64), "lifecycle_generation": 3]
    }
    private func context() async throws -> PlaybackFileContext {
        let detail: [String: Any] = ["lifecycle_generation": 3, "files": [["file_id": "5", "revision": String(repeating: "a", count: 64), "file_base": base, "reference": try binding()]]]
        let data = try JSONSerialization.data(withJSONObject: detail)
        ControlHTTP.answer = { _ in (200, data) }
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: "5", testTransport: URLSession(configuration: configuration))
    }
    private func decision(method: String = "remux", change: (inout [String: Any]) -> Void = { _ in }) throws -> SharedDecision {
        let mode = method == "direct_play" ? "direct" : method
        var wire: [String: Any] = ["file_id": "5", "reference": try binding(), "method": method, "play_url": base + "/hls/sessions",
            "delivery": ["mode": mode, "sessions_url": base + "/hls/sessions"], "source": ["container": "mp4", "hdr": "sdr"],
            "audio": [["index": 1, "codec": "aac", "default": true], ["index": 2, "codec": "ac3", "default": false]],
            "subtitles": [["index": 3, "codec": "subrip", "default": false, "forced": false, "text": true, "native": true],
                          ["index": 4, "codec": "hdmv_pgs_subtitle", "default": false, "forced": false, "text": false]],
            "delivered_dynamic_range": "sdr"]
        change(&wire)
        return try SharedDecision.decode(JSONSerialization.data(withJSONObject: wire))
    }
    private func reply(_ id: String? = nil) throws -> Data {
        let id = id ?? session
        return try JSONSerialization.data(withJSONObject: ["session_id": id, "playlist_url": "/api/v1/hls/\(id)/master.m3u8", "vod": true,
            "start_seconds": 0.0, "duration_ms": 600_000, "control": ["protocol": "plurx-playback-control-v1", "url": "/api/v1/hls/\(id)/control",
            "generation": generation, "control_epoch": 4, "next_exchange_ms": 5_000, "lease_timeout_ms": 300_000]])
    }
    private func body(_ request: URLRequest) throws -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }; stream.open(); defer { stream.close() }
        var result = Data(), buffer = [UInt8](repeating: 0, count: 4096)
        while true { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; result.append(contentsOf: buffer.prefix(count)) }
        return result
    }
    private func started(height: Int? = nil, auto: Bool? = nil, copy: Bool? = true) async throws -> (SharedStartedPlayback, SharedPlaybackPlan, SharedDecisionClient) {
        let context = try await context(), bytes = try reply()
        ControlHTTP.answer = { _ in (200, bytes) }
        let caps = Caps.snapshot().document
        let request = CreateSessionRequest(playbackId: "shared-player", requestId: "dddddddd-dddd-4ddd-8ddd-dddddddddddd", height: height, qualityAuto: auto, start: 0, copy: copy, caps: caps)
        let client = try SharedDecisionClient(testConfiguration: configuration)
        let playback = try await client.start(context: context, request: request)
        let subject = SharedPlaybackSubject(context: context, title: "Shared", resumeMs: 0, watchSequence: 7)
        let plan = try SharedPlaybackPlan(subject: subject, decision: try decision(method: copy == true ? "remux" : "transcode"), caps: caps, request: request)
        return (playback, plan, client)
    }
    private func channel(_ playback: SharedStartedPlayback) throws -> SharedControlChannel {
        var capabilities = Caps.controlCapabilities(); capabilities.dualPlayerPreparation = false
        return try SharedControlChannel(playback: playback, clientInstanceId: instance, capabilities: capabilities)
    }
    private func answer(sequence: Int, preparation: String? = "none", action: String = "none", generation: String? = nil, epoch: Int = 4) throws -> Data {
        var delivery: [String: Any] = ["presentation": "vod", "producer_state": "vod"]
        if let preparation { delivery["preparation"] = preparation }
        var actionBody: [String: Any] = ["type": action]
        if action == "terminal" { actionBody["code"] = "failed"; actionBody["message"] = "ended" }
        return try JSONSerialization.data(withJSONObject: ["protocol": "plurx-playback-control-v1", "generation": generation ?? self.generation,
            "control_epoch": epoch, "accepted_sequence": sequence, "server_time_unix_ms": 1, "delivery": delivery, "action": actionBody,
            "effective_selection": ["quality_auto": false, "height": 0, "audio_offset_ms": 0, "codec": "source"]])
    }
    private let sample = SharedRendererSample(positionMs: 30_000, bufferedFromMs: 20_000, bufferedThroughMs: 42_000, playing: true)

    func testControlRequestsCarryTheExactBTupleOrderedSequenceAndFrozenRawSelection() async throws {
        let (playback, plan, _) = try await started(height: 144, auto: false, copy: false)
        var lane = try channel(playback)
        let frozen = try plan.frozenControlSelection()
        XCTAssertEqual(frozen.object?["quality"], .object(["mode": .string("manual"), "height": .integer(144)]))
        let pause = try lane.request(.pause, sample: sample, selection: frozen)
        let seek = try lane.request(.seek(targetMs: 900_000), sample: sample, selection: frozen)
        let play = try lane.request(.play, sample: SharedRendererSample(positionMs: 60_000, bufferedFromMs: 70_000, bufferedThroughMs: 10, playing: false), selection: frozen)
        XCTAssertEqual([pause.sequence, seek.sequence, play.sequence], [1, 2, 3])
        for request in [pause, seek, play] {
            XCTAssertEqual(request.generation, generation); XCTAssertEqual(request.controlEpoch, 4)
            XCTAssertEqual(request.clientInstanceId, instance); XCTAssertEqual(request.selection, frozen)
            XCTAssertEqual(request.supportedActions, ["terminal"])
        }
        XCTAssertNotNil(pause.capabilities); XCTAssertNil(seek.capabilities); XCTAssertNil(play.capabilities)
        XCTAssertEqual(pause.demand, .hold); XCTAssertEqual(pause.playbackRate, 0); XCTAssertEqual(pause.renderState, .rendering); XCTAssertNil(pause.seekTargetMs)
        XCTAssertEqual(seek.demand, .active); XCTAssertEqual(seek.renderState, .seeking); XCTAssertEqual(seek.seekTargetMs, 600_000)
        XCTAssertEqual(play.demand, .active); XCTAssertEqual(play.playbackRate, 1)
        XCTAssertEqual(play.positionMs, 60_000); XCTAssertEqual(play.bufferedThroughMs, 60_000); XCTAssertNil(play.bufferedFromMs)
        let wire = try JSONSerialization.jsonObject(with: pause.encoded()) as! [String: Any]
        XCTAssertEqual(Set(wire.keys), ["protocol", "generation", "control_epoch", "client_instance_id", "sequence", "demand", "position_ms",
            "buffered_from_ms", "buffered_through_ms", "playback_rate", "render_state", "selection", "capabilities", "supported_actions"])
        XCTAssertEqual(try pause.encoded(), try pause.encoded())
        XCTAssertThrowsError(try lane.request(.play, sample: sample, selection: .object(["quality": .object(["mode": .string("manual"), "height": .integer(72)])])))
        let auto = try await started(height: 720, auto: true, copy: true)
        XCTAssertEqual(try auto.1.frozenControlSelection().object?["quality"], .object(["mode": .string("auto"), "height": .integer(720)]))
    }

    func testAnswersBindOnlyToTheSentSequenceOnBsTuple() async throws {
        let (playback, plan, _) = try await started()
        var lane = try channel(playback)
        let request = try lane.request(.pause, sample: sample, selection: try plan.frozenControlSelection())
        let decoded = { (data: Data) in try PlaybackControl.decoder.decode(ControlResponse.self, from: data) }
        XCTAssertEqual(try lane.accept(decoded(answer(sequence: 1)), for: request), .accepted(preparation: "none"))
        XCTAssertEqual(try lane.accept(decoded(answer(sequence: 1, preparation: nil)), for: request), .accepted(preparation: nil))
        XCTAssertEqual(try lane.accept(decoded(answer(sequence: 1, action: "terminal")), for: request), .ended)
        XCTAssertThrowsError(try lane.accept(decoded(answer(sequence: 2)), for: request))
        XCTAssertThrowsError(try lane.accept(decoded(answer(sequence: 0)), for: request))
        XCTAssertThrowsError(try lane.accept(decoded(answer(sequence: 1, generation: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")), for: request))
        XCTAssertThrowsError(try lane.accept(decoded(answer(sequence: 1, epoch: 5)), for: request))
        XCTAssertThrowsError(try lane.accept(decoded(answer(sequence: 1, action: "prepare")), for: request))
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 410, code: "session_ended")), .ended)
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 410, code: "owner_lost")), .ended)
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 429, code: "control_rate_limited", retryAfterMs: 1_200)), .retry(afterMs: 1_200))
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 425, code: "owner_transition", retryAfterMs: 60_000)), .retry(afterMs: 5_000))
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 503, code: "control_unavailable")), .retry(afterMs: 500))
        XCTAssertEqual(SharedControlChannel.classify(.init(status: nil, code: nil)), .retry(afterMs: 500))
        // B's tuple never moves: a 409 naming a newer owner is not adopted.
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 409, code: "owner_changed", generation: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", controlEpoch: 9)), .refused)
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 409, code: "stale_control")), .refused)
        XCTAssertEqual(SharedControlChannel.classify(.init(status: 422, code: "shared_control_unsupported")), .refused)
    }

    func testRendererMovesOnlyAfterAcceptanceAndDirectedChangeReopensOnNone() {
        let accepted = SharedControlOutcome.accepted(preparation: "none")
        for intent in [SharedControlIntent.play, .pause, .seek(targetMs: 5_000)] {
            XCTAssertEqual(SharedControlStep.after(intent, outcome: accepted, playing: true, restartAllowed: true), .apply)
            XCTAssertEqual(SharedControlStep.after(intent, outcome: .refused, playing: true, restartAllowed: true), .refused)
            XCTAssertEqual(SharedControlStep.after(intent, outcome: .retry(afterMs: 500), playing: true, restartAllowed: true), .refused)
        }
        XCTAssertEqual(SharedControlStep.after(.pause, outcome: .ended, playing: true, restartAllowed: true), .pauseEnded)
        XCTAssertEqual(SharedControlStep.after(.play, outcome: .ended, playing: false, restartAllowed: true), .reopen(play: true))
        XCTAssertEqual(SharedControlStep.after(.seek(targetMs: 1), outcome: .ended, playing: false, restartAllowed: true), .reopen(play: false))
        XCTAssertEqual(SharedControlStep.after(.play, outcome: .ended, playing: false, restartAllowed: false), .ended)
        XCTAssertEqual(SharedControlStep.afterChange(accepted, playing: true), .reopen(play: true))
        XCTAssertEqual(SharedControlStep.afterChange(.ended, playing: false), .reopen(play: false))
        XCTAssertEqual(SharedControlStep.afterChange(.accepted(preparation: nil), playing: true), .refused)
        XCTAssertEqual(SharedControlStep.afterChange(.accepted(preparation: "staging"), playing: true), .refused)
        XCTAssertEqual(SharedControlStep.afterChange(.refused, playing: true), .refused)
    }

    func testActualControlExchangeUsesBRouteAndClosedRefusals() async throws {
        let (playback, plan, client) = try await started()
        var lane = try channel(playback)
        var sent: [Data] = []
        ControlHTTP.answer = { request in
            XCTAssertEqual(request.httpMethod, "POST"); XCTAssertEqual(request.url?.path, "/api/v1/hls/\(self.session)/control")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer control-bearer")
            sent.append(try self.body(request)); return (200, try self.answer(sequence: 1))
        }
        let first = try lane.request(.seek(targetMs: 12_000), sample: sample, selection: try plan.frozenControlSelection())
        let outcome = try await client.control(playback: playback, channel: lane, request: first)
        XCTAssertEqual(outcome, .accepted(preparation: "none"))
        let echoed = try JSONSerialization.jsonObject(with: XCTUnwrap(sent.first)) as! [String: Any]
        XCTAssertEqual(echoed["seek_target_ms"] as? Int, 12_000); XCTAssertEqual(echoed["sequence"] as? Int, 1)
        for (status, body, expected) in [
            (429, #"{"code":"control_rate_limited","retry_after_ms":800}"#, SharedControlOutcome.retry(afterMs: 800)),
            (410, #"{"code":"session_ended"}"#, .ended),
            (409, #"{"code":"owner_changed","generation":"eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee","control_epoch":9}"#, .refused),
            (503, #"{"code":"control_unavailable"}"#, .retry(afterMs: 500)),
            (422, #"{"code":"shared_control_unsupported"}"#, .refused),
        ] {
            ControlHTTP.answer = { _ in (status, Data(body.utf8)) }
            let result = try await client.control(playback: playback, channel: lane, request: first)
            XCTAssertEqual(result, expected, "status \(status)")
        }
        ControlHTTP.answer = { _ in (200, try self.answer(sequence: 1, generation: "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee")) }
        do { _ = try await client.control(playback: playback, channel: lane, request: first); XCTFail("accepted a foreign tuple") } catch {}
        ControlHTTP.answer = { _ in throw URLError(.networkConnectionLost) }
        let lost = try await client.control(playback: playback, channel: lane, request: first)
        XCTAssertEqual(lost, .retry(afterMs: 500))
        Session.shared.setCredentials(origin: "https://b.test", token: "replacement")
        var calls = 0; ControlHTTP.answer = { _ in calls += 1; return (200, Data()) }
        do { _ = try await client.control(playback: playback, channel: lane, request: first); XCTFail("sent control under a replaced login") } catch {}
        XCTAssertEqual(calls, 0)
    }

    func testDirectedSelectionReplacesOnlyTheChangedAxis() async throws {
        let (_, plan, _) = try await started(auto: false)
        let frozen = try plan.frozenControlSelection()
        XCTAssertEqual(plan.rawQuality, .original)
        XCTAssertEqual(try plan.directedSelection(SharedDirectedChange()), frozen)
        XCTAssertEqual(try plan.directedSelection(SharedDirectedChange(quality: .original)), frozen)
        let manual = try plan.directedSelection(SharedDirectedChange(quality: .p720))
        XCTAssertEqual(manual.object?["quality"], .object(["mode": .string("manual"), "height": .integer(720)]))
        XCTAssertEqual(manual.object?["subtitle"], frozen.object?["subtitle"])
        let audio = try plan.directedSelection(SharedDirectedChange(audioIndex: .some(2)))
        XCTAssertEqual(audio.object?["audio_track"], .integer(2)); XCTAssertEqual(audio.object?["quality"], frozen.object?["quality"])
        let subtitle = try plan.directedSelection(SharedDirectedChange(subtitleIndex: .some(3)))
        XCTAssertEqual(subtitle.object?["subtitle"], .object(["mode": .string("native"), "track": .integer(3)]))
        XCTAssertEqual(try plan.directedSelection(SharedDirectedChange(subtitleIndex: .some(nil))).object?["subtitle"], .object(["mode": .string("off")]))
        XCTAssertThrowsError(try plan.directedSelection(SharedDirectedChange(audioIndex: .some(2_000))))
    }

    func testReopenPlanKeepsPlaybackIdHasNoLineageAndChoosesDirectOnlyWhenAVPlayerCanTakeTheBytes() async throws {
        let context = try await context(), caps = Caps.snapshot().document
        let subject = SharedPlaybackSubject(context: context, title: "Shared", resumeMs: 61_500, watchSequence: 7)
        let direct = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play"), caps: caps, quality: .auto, playbackId: "player-1")
        XCTAssertEqual(direct.request.presentation, "direct"); XCTAssertEqual(direct.request.playbackId, "player-1")
        XCTAssertEqual(direct.request.start, 61.5)
        XCTAssertNil(direct.request.copy); XCTAssertNil(direct.request.height); XCTAssertNil(direct.request.aac); XCTAssertNil(direct.request.audio)
        XCTAssertNil(direct.request.previousSessionId); XCTAssertNil(direct.request.reopenReason); XCTAssertNil(direct.request.controlSequence); XCTAssertNil(direct.request.intent)
        let again = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play"), caps: caps, quality: .auto, playbackId: "player-1")
        XCTAssertNotEqual(again.request.requestId, direct.request.requestId)
        let dv = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play") { $0["source"] = ["container": "mp4", "hdr": "dolby_vision"] }, caps: caps, quality: .auto)
        XCTAssertEqual(dv.request.presentation, "vod"); XCTAssertEqual(dv.request.copy, true)
        let nonDefault = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play") { $0["delivery"] = ["mode": "direct", "audio": 2] }, caps: caps, quality: .auto)
        XCTAssertEqual(nonDefault.request.presentation, "vod")
        let chosenAudio = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play"), caps: caps, quality: .auto, audioIndex: 2)
        XCTAssertEqual(chosenAudio.request.presentation, "vod"); XCTAssertEqual(chosenAudio.request.audio, 2)
        let native = try SharedPlaybackPlan.make(subject: subject, decision: try decision(), caps: caps, quality: .original, subtitleIndex: 3)
        XCTAssertEqual(native.request.nativeSubtitles, true); XCTAssertEqual(native.request.subtitle, 3); XCTAssertEqual(native.rawSubtitleIndex, 3)
        XCTAssertThrowsError(try SharedPlaybackPlan.make(subject: subject, decision: try decision(), caps: caps, quality: .original, subtitleIndex: 4))
        let rung = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "transcode"), caps: caps, quality: .p480)
        XCTAssertEqual(rung.request.height, 480); XCTAssertNotEqual(rung.request.copy, true); XCTAssertEqual(rung.rawQuality, .p480)
        var smuggled = direct.request; smuggled.copy = true
        XCTAssertThrowsError(try SharedPlaybackPlan(subject: subject, decision: try decision(method: "direct_play"), caps: caps, request: smuggled))
        XCTAssertThrowsError(try SharedPlaybackPlan(subject: subject, decision: try decision(), caps: caps, request: direct.request))
    }

    func testDirectStartPlaysTheBoundAliasWithoutAccountHeadersAndEndsThroughB() async throws {
        let context = try await context(), caps = Caps.snapshot().document
        let subject = SharedPlaybackSubject(context: context, title: "Shared", resumeMs: 0, watchSequence: 7)
        let plan = try SharedPlaybackPlan.make(subject: subject, decision: try decision(method: "direct_play"), caps: caps, quality: .auto)
        let url = "\(base)/direct?session=\(session)"
        var good: [String: Any] = ["presentation": "direct", "session_id": session, "url": url, "length": 9_007_199_254_740_991, "mime": "video/mp4"]
        var sent: Data?
        ControlHTTP.answer = { request in
            XCTAssertEqual(request.url?.path, self.base + "/hls/sessions"); sent = try self.body(request)
            return (200, try JSONSerialization.data(withJSONObject: good))
        }
        let client = try SharedDecisionClient(testConfiguration: configuration)
        let media = try await client.startMedia(context: context, request: plan.request)
        guard case .direct(let direct) = media else { return XCTFail("direct ask answered as HLS") }
        let raw = try JSONSerialization.jsonObject(with: XCTUnwrap(sent)) as! [String: Any]
        XCTAssertEqual(raw["presentation"] as? String, "direct")
        for absent in ["copy", "height", "native_subtitles", "subtitle", "aac", "previous_session_id", "control_sequence", "reopen_reason"] { XCTAssertNil(raw[absent], absent) }
        XCTAssertEqual(direct.context.sessionId, session); XCTAssertEqual(direct.start.length, 9_007_199_254_740_991)
        XCTAssertEqual(try client.directURL(direct).absoluteString, "https://b.test" + url)
        XCTAssertNil(media.durationMs)
        let changes: [(inout [String: Any]) -> Void] = [{ $0["future"] = 1 }, { $0["url"] = url + "&token=x" }, { $0["mime"] = "text/html" },
            { $0["length"] = -1 }, { $0["length"] = 1.5 }, { $0["presentation"] = "vod" }, { _ = $0.removeValue(forKey: "mime") }]
        for change in changes {
            var bad = good; change(&bad); let bytes = try JSONSerialization.data(withJSONObject: bad)
            XCTAssertThrowsError(try SharedDirectStart.decode(bytes, context: context))
        }
        let hlsBytes = try reply()
        ControlHTTP.answer = { _ in (200, hlsBytes) }
        do { _ = try await client.startMedia(context: context, request: plan.request); XCTFail("accepted HLS reply to a direct ask") } catch {}
        good["session_id"] = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        ControlHTTP.answer = { _ in (200, try JSONSerialization.data(withJSONObject: good)) }
        do { _ = try await client.startMedia(context: context, request: plan.request); XCTFail("accepted a URL for another session") } catch {}
        var heads: [String?] = []
        for status in [404, 410, 200] {
            ControlHTTP.answer = { request in
                XCTAssertEqual(request.httpMethod, "HEAD"); heads.append(request.value(forHTTPHeaderField: "Authorization"))
                XCTAssertEqual(request.url?.absoluteString, "https://b.test" + url); return (status, Data())
            }
            let gone = try await client.directSessionGone(direct)
            XCTAssertEqual(gone, status != 200)
        }
        XCTAssertEqual(heads, [nil, nil, nil])
        var deleted: String?
        ControlHTTP.answer = { request in
            XCTAssertEqual(request.httpMethod, "DELETE"); XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer control-bearer")
            deleted = request.url?.path; return (204, Data())
        }
        try await client.end(media: media)
        XCTAssertEqual(deleted, "/api/v1/hls/\(session)")
    }

    func testOrderedProgressCarriesAcrossAReopenedSession() async throws {
        let (playback, _, client) = try await started()
        let context = playback.context
        var sequences: [Int] = [], sessions: [String] = []
        ControlHTTP.answer = { request in
            let object = try JSONSerialization.jsonObject(with: self.body(request)) as! [String: Any]
            sequences.append(object["sequence"] as! Int); sessions.append(object["session_id"] as! String)
            return (200, Data("{}".utf8))
        }
        _ = try await client.orderedProgress(media: .hls(playback), initialWatchSequence: 7, positionMs: 1_000, durationMs: 600_000)
        let second = "ffffffff-ffff-4fff-8fff-ffffffffffff"
        let unbound = try await self.context()
        let url = "\(base)/direct?session=\(second)"
        let bytes = try JSONSerialization.data(withJSONObject: ["presentation": "direct", "session_id": second, "url": url, "length": 10, "mime": "video/mp4"])
        let (start, bound) = try SharedDirectStart.decode(bytes, context: unbound)
        let reopened = SharedStartedMedia.direct(SharedStartedDirect(start: start, context: bound, request: playback.request))
        ControlHTTP.answer = { request in
            let object = try JSONSerialization.jsonObject(with: self.body(request)) as! [String: Any]
            sequences.append(object["sequence"] as! Int); sessions.append(object["session_id"] as! String)
            return (200, Data("{}".utf8))
        }
        _ = try await client.orderedProgress(media: reopened, initialWatchSequence: 0, positionMs: 2_000, durationMs: nil)
        XCTAssertEqual(sequences, [8, 9]); XCTAssertEqual(sessions, [session, second])
        XCTAssertEqual(context.sessionId, session)
    }

    func testStatusRendersOnlyValidatedTokensBoundToTheRetainedPlayback() async throws {
        let (playback, _, client) = try await started()
        var metrics: [String: Any] = Dictionary(uniqueKeysWithValues: SharedPlaybackStatus.requiredCounters.map { ($0, 0) })
        metrics["target_height"] = 720; metrics["encoder"] = "h264_videotoolbox"; metrics["playlist_shape"] = "vod"
        metrics["producer_state"] = "running"; metrics["server_ready_state"] = "ready"; metrics["ahead_seconds"] = 42
        metrics["admitted"] = true; metrics["suspended"] = false; metrics["final"] = false
        var wire: [String: Any] = ["subject": "shared", "reference": try binding(), "session_id": session, "incarnation_id": generation, "control_epoch": 4, "status": metrics]
        let bytes = try JSONSerialization.data(withJSONObject: wire)
        ControlHTTP.answer = { request in XCTAssertEqual(request.url?.path, "/api/v1/hls/\(self.session)/status"); return (200, bytes) }
        let status = try await client.status(playback: playback)
        XCTAssertEqual(status.summary, "Shared HLS · 720p · h264_videotoolbox · running · 42 s ahead")
        XCTAssertEqual(status.serverReadyState, "ready")
        for prose in ["copy encoder", "/srv/media/film.mkv", "x\u{1}", "café", String(repeating: "a", count: 33)] {
            var bad = metrics; bad["encoder"] = prose; wire["status"] = bad
            XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback), prose)
        }
        wire["status"] = metrics; wire["session_id"] = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback))
        wire["session_id"] = session; wire["subject"] = "local"
        XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback))
    }
}
