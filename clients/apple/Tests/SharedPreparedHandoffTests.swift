import Foundation
import XCTest
@testable import plurx

private final class PreparedHTTP: URLProtocol {
    static var answer: ((URLRequest, Data) throws -> (Int, Data))!
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            var body = request.httpBody ?? Data()
            if body.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 4096)
                while true { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; body.append(contentsOf: buffer.prefix(count)) }
            }
            let (status, data) = try Self.answer(request, body)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: [:])!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: data); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

/// Synthetic B answers through actual authenticated URLProtocol I/O for the
/// Shared prepared successor (B P1/P2). Client protocol contracts only; the
/// two-player switch itself needs a device.
@MainActor
final class SharedPreparedHandoffTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "7", itemId: "9007199254740993")
    private let predecessor = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private let generation = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    private let successor = "dddddddd-dddd-4ddd-8ddd-dddddddddddd"
    private let successorGeneration = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
    private let actionId = "f0f0f0f0-f0f0-40f0-80f0-f0f0f0f0f0f0"
    private let instance = "cccccccc-cccc-4ccc-8ccc-cccccccccccc"
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/" + String(repeating: "L", count: 236) }
    private var configuration: URLSessionConfiguration { let value = URLSessionConfiguration.ephemeral; value.protocolClasses = [PreparedHTTP.self]; return value }
    override func setUp() async throws { Session.shared.setCredentials(origin: "https://b.test", token: "prepared-bearer") }
    override func tearDown() async throws { PreparedHTTP.answer = nil; Session.shared.setCredentials(origin: "", token: nil) }

    private func binding() throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return ["item": try JSONSerialization.jsonObject(with: encoder.encode(ref)), "file_id": "5", "revision": String(repeating: "a", count: 64), "lifecycle_generation": 3]
    }
    private func context() async throws -> PlaybackFileContext {
        let detail: [String: Any] = ["lifecycle_generation": 3, "files": [["file_id": "5", "revision": String(repeating: "a", count: 64), "file_base": base, "reference": try binding()]]]
        let data = try JSONSerialization.data(withJSONObject: detail)
        PreparedHTTP.answer = { _, _ in (200, data) }
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: "5", testTransport: URLSession(configuration: configuration))
    }
    private func decision() throws -> SharedDecision {
        try SharedDecision.decode(JSONSerialization.data(withJSONObject: ["file_id": "5", "reference": try binding(), "method": "remux",
            "play_url": base + "/hls/sessions", "delivery": ["mode": "remux", "sessions_url": base + "/hls/sessions"],
            "source": ["container": "mkv", "hdr": "sdr", "video_codec": "hevc"],
            "audio": [["index": 1, "codec": "aac", "default": true], ["index": 2, "codec": "ac3", "default": false]],
            "subtitles": [["index": 3, "codec": "subrip", "default": false, "forced": false, "text": true, "native": true]],
            "delivered_dynamic_range": "sdr"]))
    }
    private func started() async throws -> (SharedStartedPlayback, SharedPlaybackPlan, SharedDecisionClient) {
        let context = try await context()
        let reply = try JSONSerialization.data(withJSONObject: ["session_id": predecessor, "playlist_url": "/api/v1/hls/\(predecessor)/master.m3u8", "vod": true,
            "start_seconds": 0.0, "duration_ms": 600_000, "control": ["protocol": "plurx-playback-control-v1", "url": "/api/v1/hls/\(predecessor)/control",
            "generation": generation, "control_epoch": 4, "next_exchange_ms": 5_000, "lease_timeout_ms": 300_000]])
        PreparedHTTP.answer = { _, _ in (200, reply) }
        let caps = Caps.snapshot().document
        let request = CreateSessionRequest(playbackId: "shared-player", requestId: "dddddddd-dddd-4ddd-8ddd-ddddddddddd0", qualityAuto: false, start: 0, copy: true, caps: caps)
        let client = try SharedDecisionClient(testConfiguration: configuration)
        let playback = try await client.start(context: context, request: request)
        let subject = SharedPlaybackSubject(context: context, title: "Shared", resumeMs: 0, watchSequence: 7)
        return (playback, try SharedPlaybackPlan(subject: subject, decision: try decision(), caps: caps, request: request), client)
    }
    private func channel(_ playback: SharedStartedPlayback, prepares: Bool = true) throws -> SharedControlChannel {
        var capabilities = Caps.controlCapabilities(); capabilities.dualPlayerPreparation = prepares
        return try SharedControlChannel(playback: playback, clientInstanceId: instance, capabilities: capabilities)
    }
    private func control(_ session: String? = nil, generation: String? = nil, url: String? = nil, cadence: Int = 5_000) -> [String: Any] {
        let session = session ?? successor
        return ["protocol": "plurx-playback-control-v1", "url": url ?? "/api/v1/hls/\(session)/control", "generation": generation ?? successorGeneration,
                "control_epoch": 2, "next_exchange_ms": cadence, "lease_timeout_ms": 300_000]
    }
    private func prepare(session: String? = nil, playlist: String? = nil, control: [String: Any]?) -> [String: Any] {
        let session = session ?? successor
        var action: [String: Any] = ["type": "prepare", "action_id": actionId, "session_id": session,
            "playlist_url": playlist ?? "/api/v1/hls/\(session)/index.m3u8", "media_origin_ms": 0,
            "effective_selection": ["quality_auto": false, "height": 720, "audio_track": 1, "audio_offset_ms": 0, "codec": "server_selected", "dynamic_range": "sdr"]]
        if let control { action["control"] = control }
        return action
    }
    private func answer(sequence: Int, preparation: String?, action: [String: Any] = ["type": "none"], generation: String? = nil, epoch: Int = 4) throws -> Data {
        var delivery: [String: Any] = ["presentation": "vod"]
        if let preparation { delivery["preparation"] = preparation }
        return try JSONSerialization.data(withJSONObject: ["protocol": "plurx-playback-control-v1", "generation": generation ?? self.generation, "control_epoch": epoch,
            "accepted_sequence": sequence, "server_time_unix_ms": 1, "delivery": delivery, "action": action,
            "effective_selection": ["quality_auto": false, "height": 0, "audio_offset_ms": 0, "codec": "source"]])
    }
    private let sample = SharedRendererSample(positionMs: 30_000, bufferedFromMs: 20_000, bufferedThroughMs: 42_000, playing: true)

    func testPreparedChannelDeclaresBothNamesAndCarriesAcknowledgementsOnThePredecessor() async throws {
        let (playback, plan, _) = try await started()
        var lane = try channel(playback)
        XCTAssertTrue(lane.prepares)
        let frozen = try plan.frozenControlSelection()
        let first = try lane.request(nil, sample: sample, selection: frozen)
        XCTAssertEqual(first.supportedActions, ["terminal", "prepare_replacement", "shared_prepare_replacement"])
        XCTAssertEqual(first.capabilities?.dualPlayerPreparation, true); XCTAssertNil(first.acknowledgement)
        let commit = ActionAcknowledgement(actionId: actionId, state: .committed, committedMediaOriginMs: 0, firstFrameUnixMs: 1_800_000_000_000)
        let settle = try lane.request(nil, sample: sample, selection: frozen, acknowledgement: commit)
        XCTAssertEqual(settle.sequence, 2); XCTAssertEqual(settle.generation, generation); XCTAssertNil(settle.capabilities)
        let wire = try JSONSerialization.jsonObject(with: settle.encoded()) as! [String: Any]
        let ack = try XCTUnwrap(wire["acknowledgement"] as? [String: Any])
        XCTAssertEqual(ack["action_id"] as? String, actionId); XCTAssertEqual(ack["state"] as? String, "committed")
        XCTAssertEqual(ack["committed_media_origin_ms"] as? Int, 0); XCTAssertEqual(ack["first_frame_unix_ms"] as? Int, 1_800_000_000_000)
        XCTAssertNil(ack["buffered_through_ms"])
        // A replay of the same sequence is byte-identical, which is what B
        // answers exactly.
        XCTAssertEqual(try settle.encoded(), try settle.encoded())
        XCTAssertThrowsError(try lane.request(nil, sample: sample, selection: frozen, acknowledgement: ActionAcknowledgement(actionId: actionId, state: .committed)))
        var passive = try channel(playback, prepares: false)
        XCTAssertEqual(try passive.request(nil, sample: sample, selection: frozen).supportedActions, ["terminal"])
        XCTAssertThrowsError(try passive.request(nil, sample: sample, selection: frozen, acknowledgement: ActionAcknowledgement(actionId: actionId, state: .aborted)))
    }

    func testOfferBindsOnlyBsOwnSuccessorAndItsControlBootstrap() async throws {
        let (playback, plan, client) = try await started()
        var lane = try channel(playback)
        let request = try lane.request(nil, sample: sample, selection: try plan.directedSelection(SharedDirectedChange(quality: .p720)))
        let good = try answer(sequence: 1, preparation: "offered", action: prepare(control: control()))
        PreparedHTTP.answer = { request, _ in
            XCTAssertEqual(request.url?.path, "/api/v1/hls/\(self.predecessor)/control"); return (200, good)
        }
        let outcome = try await client.control(playback: playback, channel: lane, request: request)
        guard case .offered(let offer, let preparation) = outcome else { return XCTFail("offer not bound: \(outcome)") }
        XCTAssertEqual(preparation, "offered"); XCTAssertEqual(offer.action.sessionId, successor)
        XCTAssertEqual(offer.action.actionId, actionId); XCTAssertEqual(offer.control.generation, successorGeneration)
        XCTAssertEqual(SharedControlStep.after(.play, outcome: outcome, playing: true, restartAllowed: true), .apply)
        let refused: [(String, [String: Any])] = [
            ("missing control", prepare(control: nil)),
            ("control on the predecessor", prepare(control: control(url: "/api/v1/hls/\(predecessor)/control"))),
            ("foreign cadence", prepare(control: control(cadence: 1_000))),
            ("generation not v4", prepare(control: control(generation: "eeeeeeee-eeee-1eee-8eee-eeeeeeeeeeee"))),
            ("predecessor named", prepare(session: predecessor, control: control(predecessor))),
            ("absolute playlist", prepare(playlist: "https://evil.test/api/v1/hls/\(successor)/index.m3u8", control: control())),
            ("segment playlist", prepare(playlist: "/api/v1/hls/\(successor)/seg1.m4s", control: control())),
        ]
        for (name, action) in refused {
            let bytes = try answer(sequence: 1, preparation: "offered", action: action)
            PreparedHTTP.answer = { _, _ in (200, bytes) }
            do { let value = try await client.control(playback: playback, channel: lane, request: request); XCTFail("\(name) bound: \(value)") } catch {}
        }
        // A channel that never declared the Shared name never binds a prepare.
        var passive = try channel(playback, prepares: false)
        let passiveRequest = try passive.request(nil, sample: sample, selection: try plan.frozenControlSelection())
        PreparedHTTP.answer = { _, _ in (200, good) }
        do { _ = try await client.control(playback: playback, channel: passive, request: passiveRequest); XCTFail("passive channel bound a prepare") } catch {}
    }

    func testReusedOfferWaitStagesThenTakesTheOfferAndDeclinesOnlyOnALaterNone() throws {
        let offer = try SharedPreparedOffer.decode(ControlAction(type: "prepare", actionId: actionId, sessionId: successor,
            playlistUrl: "/api/v1/hls/\(successor)/index.m3u8", mediaOriginMs: 0,
            effectiveSelection: EffectiveSelection(qualityAuto: false, height: 720, audioTrack: 1, audioOffsetMs: 0, codec: "server_selected")),
            raw: answer(sequence: 3, preparation: "offered", action: prepare(control: control())), predecessor: predecessor)
        var wait = PreparedOfferWait(tappedAtMs: 0, floorSequence: 2)
        XCTAssertEqual(wait.observe(answer: SharedPreparedHandoff.answer(.accepted(preparation: "staging"), sequence: 2), nowMs: 10), .keepWaiting(nextExchangeMs: 1_000))
        XCTAssertNil(SharedPreparedHandoff.answer(.retry(afterMs: 500), sequence: 3))
        XCTAssertNil(SharedPreparedHandoff.answer(.refused, sequence: 3))
        XCTAssertEqual(wait.observe(answer: nil, nowMs: 1_000), .keepWaiting(nextExchangeMs: 1_000))
        XCTAssertEqual(wait.observe(answer: SharedPreparedHandoff.answer(.offered(offer, preparation: "offered"), sequence: 3), nowMs: 2_000), .offered(offer.action))
        var declined = PreparedOfferWait(tappedAtMs: 0, floorSequence: 2)
        XCTAssertEqual(declined.observe(answer: SharedPreparedHandoff.answer(.accepted(preparation: "none"), sequence: 2), nowMs: 10), .keepWaiting(nextExchangeMs: 1_000))
        XCTAssertEqual(declined.observe(answer: SharedPreparedHandoff.answer(.accepted(preparation: "none"), sequence: 3), nowMs: 1_100), .reopen(reason: "declined"))
        var absent = PreparedOfferWait(tappedAtMs: 0, floorSequence: 2)
        XCTAssertEqual(absent.observe(answer: SharedPreparedHandoff.answer(.accepted(preparation: nil), sequence: 3), nowMs: 1_100), .keepWaiting(nextExchangeMs: 1_000))
        XCTAssertEqual(absent.observe(answer: nil, nowMs: PreparedOfferWait.boundMs), .reopen(reason: "timed_out"))
        XCTAssertTrue(SharedPreparedHandoff.settled(.accepted(preparation: nil)))
        XCTAssertTrue(SharedPreparedHandoff.settled(.offered(offer, preparation: "offered")))
        for unsettled in [SharedControlOutcome.ended, .refused, .retry(afterMs: 500)] { XCTAssertFalse(SharedPreparedHandoff.settled(unsettled)) }
    }

    func testCommittedSuccessorBecomesThePlayersSessionAskAndProgress() async throws {
        let (playback, plan, client) = try await started()
        let offer = try SharedPreparedOffer.decode(ControlAction(type: "prepare", actionId: actionId, sessionId: successor,
            playlistUrl: "/api/v1/hls/\(successor)/index.m3u8", mediaOriginMs: 0,
            effectiveSelection: EffectiveSelection(qualityAuto: false, height: 720, audioTrack: 2, audioOffsetMs: 0, codec: "server_selected")),
            raw: answer(sequence: 3, preparation: "offered", action: prepare(control: control())), predecessor: predecessor)
        let change = SharedDirectedChange(quality: .p720, audioIndex: .some(2), subtitleIndex: .some(3))
        let asked = try plan.directedSelection(change)
        let adopted = try plan.adopting(offer, change: change, predecessor: playback, positionMs: 61_500)
        XCTAssertEqual(adopted.playback.context.sessionId, successor)
        XCTAssertEqual(adopted.playback.start.response.playlistUrl, "/api/v1/hls/\(successor)/index.m3u8")
        XCTAssertEqual(adopted.playback.start.response.control?.generation, successorGeneration)
        XCTAssertEqual(adopted.playback.start.response.durationMs, 600_000)
        // The ask the successor was staged for is the plan's frozen ask now.
        XCTAssertEqual(try adopted.plan.frozenControlSelection(), asked)
        XCTAssertEqual(adopted.plan.rawQuality, .p720); XCTAssertEqual(adopted.plan.rawAudioIndex, 2); XCTAssertEqual(adopted.plan.rawSubtitleIndex, 3)
        let request = adopted.plan.request
        XCTAssertEqual(request.playbackId, "shared-player"); XCTAssertNotEqual(request.requestId, plan.request.requestId)
        XCTAssertEqual(request.start, 61.5); XCTAssertEqual(request.copy, false); XCTAssertEqual(request.height, 720)
        XCTAssertNil(request.previousSessionId); XCTAssertNil(request.controlSequence); XCTAssertNil(request.reopenReason); XCTAssertNil(request.intent)
        XCTAssertEqual(adopted.plan.subject.context, plan.subject.context); XCTAssertNil(adopted.plan.subject.context.sessionId)
        // Control moves to the successor's own tuple, starting a fresh order.
        var next = try channel(adopted.playback)
        let first = try next.request(.pause, sample: sample, selection: try adopted.plan.frozenControlSelection())
        XCTAssertEqual(first.generation, successorGeneration); XCTAssertEqual(first.controlEpoch, 2); XCTAssertEqual(first.sequence, 1)
        XCTAssertEqual(first.clientInstanceId, instance); XCTAssertNotNil(first.capabilities)
        var routed: [String] = [], sessions: [String] = [], sequences: [Int] = []
        PreparedHTTP.answer = { request, body in
            routed.append(request.url!.path)
            if request.url!.path.hasSuffix("/progress") {
                let object = try JSONSerialization.jsonObject(with: body) as! [String: Any]
                sessions.append(object["session_id"] as! String); sequences.append(object["sequence"] as! Int)
                return (200, Data("{}".utf8))
            }
            return (200, try self.answer(sequence: 1, preparation: nil, generation: self.successorGeneration, epoch: 2))
        }
        _ = try await client.orderedProgress(media: .hls(playback), initialWatchSequence: 7, positionMs: 60_000, durationMs: 600_000)
        _ = try await client.orderedProgress(media: .hls(adopted.playback), initialWatchSequence: 7, positionMs: 62_000, durationMs: 600_000)
        XCTAssertEqual(sessions, [predecessor, successor]); XCTAssertEqual(sequences, [8, 9])
        let accepted = try await client.control(playback: adopted.playback, channel: next, request: first)
        XCTAssertEqual(accepted, .accepted(preparation: nil))
        XCTAssertEqual(routed.last, "/api/v1/hls/\(successor)/control")
        // An offer whose playlist carries anything B's Start grammar refuses
        // is never adopted.
        let tokened = try SharedPreparedOffer.decode(ControlAction(type: "prepare", actionId: actionId, sessionId: successor,
            playlistUrl: "/api/v1/hls/\(successor)/index.m3u8?token=x", mediaOriginMs: 0,
            effectiveSelection: EffectiveSelection(qualityAuto: false, height: 720, audioTrack: 2, audioOffsetMs: 0, codec: "server_selected")),
            raw: answer(sequence: 3, preparation: "offered", action: prepare(control: control())), predecessor: predecessor)
        XCTAssertThrowsError(try plan.adopting(tokened, change: change, predecessor: playback, positionMs: 0))
    }

    func testSuccessorRequestMirrorsBsSelectionRewrite() async throws {
        let (_, plan, _) = try await started()
        let auto = try SharedPreparedHandoff.successorRequest(plan.request, selection: try plan.directedSelection(SharedDirectedChange(quality: .auto)), positionMs: 0)
        XCTAssertEqual(auto.qualityAuto, true); XCTAssertNil(auto.copy); XCTAssertNil(auto.height)
        let manual = try SharedPreparedHandoff.successorRequest(plan.request, selection: try plan.directedSelection(SharedDirectedChange(quality: .p480)), positionMs: 1_000)
        XCTAssertEqual(manual.qualityAuto, false); XCTAssertEqual(manual.copy, false); XCTAssertEqual(manual.height, 480); XCTAssertEqual(manual.start, 1)
        let off = try SharedPreparedHandoff.successorRequest(plan.request, selection: try plan.directedSelection(SharedDirectedChange(subtitleIndex: .some(nil))), positionMs: 0)
        XCTAssertNil(off.subtitle); XCTAssertNil(off.nativeSubtitles); XCTAssertEqual(off.copy, true); XCTAssertEqual(off.qualityAuto, false)
        XCTAssertThrowsError(try SharedPreparedHandoff.successorRequest(plan.request, selection: .object(["quality": .object(["mode": .string("burn")])]), positionMs: 0))
    }
}
