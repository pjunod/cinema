import AVFoundation
import Combine
import Foundation
import XCTest
@testable import plurx

@MainActor
final class LiveTvTests: XCTestCase {
    func testLibraryChannelProgressCallbacksFollowCurrentAttachment() async {
        let controller = LibraryChannelPlayerController()
        let item = AVPlayerItem(asset: AVMutableComposition())
        controller.player.replaceCurrentItem(with: item)
        var captures = 0
        let update = controller.makeProgressObservation(item, sequence: 0) { captures += 1 }
        update()
        update()
        XCTAssertEqual(captures, 2, "ongoing progress must refresh the reporter's stored snapshot")
        controller.player.replaceCurrentItem(with: AVPlayerItem(asset: AVMutableComposition()))
        update()
        XCTAssertEqual(captures, 2, "a queued predecessor callback cannot report a successor's position")
        controller.player.replaceCurrentItem(with: item)
        await controller.stop()
        update()
        XCTAssertEqual(captures, 2, "stop revokes queued progress callbacks")
    }

    func testLibraryChannelBufferingKeepsProductionActive() async throws {
        let controller = LibraryChannelPlayerController()
        controller.player.replaceCurrentItem(with: AVPlayerItem(asset: AVMutableComposition()))
        XCTAssertEqual(controller.player.rate, 0)
        let observation = try XCTUnwrap(controller.controlObservation())
        XCTAssertFalse(observation.isPaused, "a zero decoder rate is not a viewer pause")
        XCTAssertEqual(PlaybackControlMapping.demand(observation, terminallyEnded: false), .active,
                       "buffering must keep the server producing fresh playlist segments")
        await controller.stop()
        XCTAssertNil(controller.controlObservation())
    }

    func testLibraryChannelCapturesFailureNotificationError() async {
        let controller = LibraryChannelPlayerController()
        let item = AVPlayerItem(asset: AVMutableComposition())
        controller.player.replaceCurrentItem(with: item)
        controller.observeFailure(item, sequence: 0)
        let received = expectation(description: "notification failure is visible")
        let observation = controller.$playbackError.compactMap { $0 }.first().sink { message in
            XCTAssertTrue(message.contains("NSOSStatusErrorDomain -12880"))
            received.fulfill()
        }
        let error = NSError(domain: "NSOSStatusErrorDomain", code: -12880)
        XCTAssertNil(item.error, "a failure notification can arrive without item.error")
        NotificationCenter.default.post(name: .AVPlayerItemFailedToPlayToEndTime, object: item,
                                        userInfo: [AVPlayerItemFailedToPlayToEndTimeErrorKey: error])
        await fulfillment(of: [received], timeout: 2)
        observation.cancel()
        controller.handleItemFailure(item, sequence: 0)
        XCTAssertTrue(controller.playbackError?.contains("NSOSStatusErrorDomain -12880") == true,
                      "a later callback without an error must not erase the notification's code")
        await controller.stop()
    }

    func testLibraryChannelExecutesPlaybackDecision() throws {
        let caps = Caps.snapshot().document
        func request(_ decision: Decision) -> CreateSessionRequest {
            LibraryChannelPlayerController.playbackRequest(
                decision: decision, caps: caps, playbackId: "channel-player", positionMs: 3_217_072
            )
        }
        var decision = Decision(fileId: 5355, method: "direct_play", playUrl: "/file")
        decision.audio = [AudioTrack(index: 3, codec: "ac3", default: true)]
        let direct = request(decision)
        XCTAssertEqual(direct.copy, true, "scheduled sessions must copy a direct-play source")
        XCTAssertEqual(direct.aac, false)
        XCTAssertEqual(direct.audio, 3, "carry the server's default track, not the muxer's first track")
        XCTAssertEqual(direct.caps, caps)
        XCTAssertEqual(direct.start, 3217.072)
        XCTAssertEqual(direct.presentation, "vod")
        XCTAssertNil(direct.height, "Auto quality remains server-owned")

        decision.delivery = Delivery(mode: "remux", aac: true, preserveDolbyVision: true, audio: 5)
        decision.audio?.append(AudioTrack(index: 5, codec: "truehd", default: false))
        let remux = request(decision)
        XCTAssertEqual(remux.copy, true)
        XCTAssertEqual(remux.aac, true, "unsupported audio must not force a video transcode")
        XCTAssertEqual(remux.audio, 5, "the explicit delivery plan outranks the default track")
        XCTAssertEqual(remux.preserveDolbyVision, true)
        XCTAssertNil(remux.hdr10)

        decision.delivery = Delivery(mode: "transcode")
        decision.deliveredDynamicRange = "hdr10"
        let transcode = request(decision)
        XCTAssertNil(transcode.copy, "an incompatible source still follows the transcode verdict")
        XCTAssertNil(transcode.aac)
        XCTAssertNil(transcode.preserveDolbyVision)
        XCTAssertEqual(transcode.hdr10, true)
        XCTAssertEqual(transcode.caps, caps)
        decision.deliveredDynamicRange = "sdr"
        XCTAssertNil(request(decision).hdr10)

        // Inspect the actual wire fields; nil copy must remain absent while
        // direct/remux must explicitly opt into the copy route.
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(remux)) as? [String: Any])
        XCTAssertEqual(wire["copy"] as? Bool, true)
        XCTAssertEqual(wire["aac"] as? Bool, true)
        XCTAssertEqual(wire["preserve_dolby_vision"] as? Bool, true)
        XCTAssertEqual(wire["playback_id"] as? String, "channel-player")
    }

    func testLibraryChannelFailureIncludesAppleCodes() {
        let underlying = NSError(domain: "NSOSStatusErrorDomain", code: -12880,
                                 userInfo: ["URL": "private-capability"])
        let error = NSError(domain: "AVFoundationErrorDomain", code: -11800, userInfo: [
            NSLocalizedDescriptionKey: "Cannot Complete Action",
            NSUnderlyingErrorKey: underlying
        ])
        XCTAssertEqual(LibraryChannelPlayerController.playbackFailureDescription(error),
                       "Cannot Complete Action (AVFoundationErrorDomain -11800; NSOSStatusErrorDomain -12880)")
    }

    func testLibraryChannelFailureIsVisibleAndFenced() async {
        let controller = LibraryChannelPlayerController()
        let current = AVPlayerItem(asset: AVMutableComposition())
        let predecessor = AVPlayerItem(asset: AVMutableComposition())
        controller.player.replaceCurrentItem(with: current)

        controller.handleItemFailure(predecessor, sequence: 0)
        XCTAssertNil(controller.playbackError, "an old item must not overwrite the current channel")
        controller.handleItemFailure(current, sequence: 1)
        XCTAssertNil(controller.playbackError, "an old tune callback must be ignored")
        controller.handleItemFailure(current, sequence: 0)
        XCTAssertEqual(controller.playbackError,
                       "The channel stream could not be played. Try Watch live again.")
        XCTAssertFalse(controller.busy)

        await controller.stop()
        XCTAssertNil(controller.playbackError)
        controller.handleItemFailure(current, sequence: 0)
        XCTAssertNil(controller.playbackError, "a stopped tune cannot restore its failure")
    }

    func testAppleTvLibraryChannelsOfferFullscreenWithoutRetuning() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LibraryChannels.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertTrue(source.contains("Button(\"Fullscreen\") { fullscreen = true }"))
        XCTAssertTrue(source.contains(".fullScreenCover(isPresented: $fullscreen)"))
        XCTAssertTrue(source.contains("VideoPlayer(player: controller.player).ignoresSafeArea()"),
                      "fullscreen must reuse the active channel player instead of opening another session")
        XCTAssertTrue(source.contains("if controller.watching != nil && !fullscreen {"),
                      "only one surface should render the shared player while fullscreen is open")
        XCTAssertTrue(source.contains("Button(\"Return to channels\") { fullscreen = false }"))
        XCTAssertTrue(source.contains("if !fullscreen { Task { await controller.stop() } }"),
                      "presenting the cover must not stop the session it displays")
    }

    func testTypedPlaybackConflictShowsReasonWithoutChangingAuthHandling() throws {
        let data = Data(#"{"code":"vod_source_rescan_required","message":"The source probe needs refreshing."}"#.utf8)
        let url = try XCTUnwrap(URL(string: "http://127.0.0.1/api/v1/library-channels/test/sessions"))
        let conflict = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: 409, httpVersion: nil, headerFields: nil))
        XCTAssertThrowsError(try PlurxAPI.check(conflict, data: data)) { error in
            XCTAssertEqual(error.localizedDescription,
                           "The source probe needs refreshing. (vod_source_rescan_required, HTTP 409)")
        }
        // Authentication stays status-shaped, and deliberately: the body is not
        // read at all for a 401 or a 403, so `AppModel.isSessionExpired` keeps
        // matching however talkative the server becomes about its refusals.
        for status in [401, 403] {
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: nil))
            XCTAssertThrowsError(try PlurxAPI.check(response, data: data)) { error in
                guard case APIError.http(let code) = error else { return XCTFail("auth classification changed") }
                XCTAssertEqual(code, status)
                XCTAssertTrue(AppModel.isSessionExpired(error))
            }
        }
        // Every other refusal the server explained is kept whole, because the
        // playback surface contract classifies on the code
        // (PLAYBACK-SURFACE-CONTRACT.md §3.3). A Live TV session create refused
        // with `vod_source_rescan_required` is the same answer whatever status
        // carries it.
        for status in [400, 404, 503] {
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: nil))
            XCTAssertThrowsError(try PlurxAPI.check(response, data: data)) { error in
                guard case APIError.refused(let refusedStatus, let code, let message, let positionMs) = error else {
                    return XCTFail("a typed refusal must survive the throw")
                }
                XCTAssertEqual(refusedStatus, status)
                XCTAssertEqual(code, "vod_source_rescan_required")
                XCTAssertEqual(message, "The source probe needs refreshing.")
                XCTAssertNil(positionMs)
                XCTAssertFalse(AppModel.isSessionExpired(error))
            }
        }
        // …and a body with no code is still just a status.
        for status in [400, 404, 503] {
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: nil))
            let legacy = Data(#"{"error":"no code here"}"#.utf8)
            XCTAssertThrowsError(try PlurxAPI.check(response, data: legacy)) { error in
                guard case APIError.http(let code) = error else { return XCTFail("a bodiless answer changed shape") }
                XCTAssertEqual(code, status)
            }
        }
        XCTAssertThrowsError(try PlurxAPI.check(conflict, data: Data("not JSON".utf8))) { error in
            guard case APIError.http(409) = error else { return XCTFail("untyped conflict classification changed") }
        }
    }

    func testDeliveryLabelsDistinguishCopiedAndTranscodedTracks() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let wire = #"{"output":{"container":"mpegts","video_codec":"h264","audio_codec":"aac","width":1920,"height":1080,"audio_channels":2},"video_action":"copy","audio_action":"copy","packaging":"mpegts"}"#
        func delivery(video: String, audio: String) throws -> LiveTvDelivery {
            let data = wire.replacingOccurrences(of: "\"video_action\":\"copy\"", with: "\"video_action\":\"\(video)\"")
                .replacingOccurrences(of: "\"audio_action\":\"copy\"", with: "\"audio_action\":\"\(audio)\"")
            return try decoder.decode(LiveTvDelivery.self, from: Data(data.utf8))
        }
        let copied = try delivery(video: "copy", audio: "copy")
        XCTAssertEqual(copied.playbackMethod, "Direct stream · no transcoding")
        XCTAssertEqual(copied.videoDescription, "Copied unchanged · H264 · 1920×1080")
        XCTAssertEqual(copied.audioDescription, "Copied unchanged · AAC · Stereo")
        let audio = try delivery(video: "copy", audio: "encode")
        XCTAssertEqual(audio.playbackMethod, "Audio transcoding · original video")
        XCTAssertEqual(audio.audioDescription, "Transcoded · AAC · Stereo")
        XCTAssertEqual(try delivery(video: "encode", audio: "copy").playbackMethod,
                       "Video transcoding · original audio")
        XCTAssertEqual(try delivery(video: "encode", audio: "encode").playbackMethod,
                       "Video and audio transcoding")
        XCTAssertEqual(try delivery(video: "future", audio: "copy").playbackMethod,
                       "Playback method unavailable")
        XCTAssertTrue(liveTvTechnicalSummary(channel, status: nil, delivery: audio)
            .hasPrefix("Audio transcoding · original video"))
        XCTAssertTrue(liveTvTechnicalSummary(channel, status: nil)
            .hasPrefix("Playback method unavailable"))
    }

    func testTheHintStoreRoundTripsAndTreatsAMissingFileAsNoHint() throws {
        let store = LiveTvStartHintStore()
        store.clear()
        defer { store.clear() }

        // The whole of the tvOS Caches case: a purged file is "no hint", never
        // an error and never a reason to refuse a start.
        XCTAssertNil(store.read())
        let id = LiveTvStartReducer.newRequestId()
        try store.write(LiveTvStartHint(requestId: id, touchedAt: 1_788_998_400_000))
        let read = try XCTUnwrap(store.read())
        XCTAssertEqual(read.requestId, id)
        XCTAssertEqual(read.touchedAt, 1_788_998_400_000)
        store.clear()
        XCTAssertNil(store.read())
    }

    func testTheHintIsSnakeCasedJsonAndNothingElse() throws {
        let store = LiveTvMemoryHintStore()
        try store.write(LiveTvStartHint(requestId: String(repeating: "a", count: 32), touchedAt: 7))
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let hint = try XCTUnwrap(store.value)
        let wire = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(hint)) as? [String: Any])
        XCTAssertEqual(Set(wire.keys), ["request_id", "touched_at"],
                       "a hint is a request id and a timestamp — never a capability or a token")
        XCTAssertEqual(wire["request_id"] as? String, String(repeating: "a", count: 32))
    }

    func testARequestIdIsThirtyTwoLowerCaseHexCharacters() {
        for _ in 0..<64 {
            let id = LiveTvStartReducer.newRequestId()
            XCTAssertEqual(id.count, 32, id)
            XCTAssertTrue(LiveTvStartReducer.isRequestId(id), id)
        }
        XCTAssertFalse(LiveTvStartReducer.isRequestId(String(repeating: "A", count: 32)))
        XCTAssertFalse(LiveTvStartReducer.isRequestId(String(repeating: "a", count: 31)))
        XCTAssertFalse(LiveTvStartReducer.isRequestId(String(repeating: "g", count: 32)))
        XCTAssertFalse(LiveTvStartReducer.isRequestId(""))
    }

    func testTheOldRestartBarrierIsGoneFromTheSource() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        for file in ["LiveTv.swift", "LiveTvView.swift"] {
            let source = try String(
                contentsOf: testsDirectory.appendingPathComponent("../Sources/\(file)").standardizedFileURL,
                encoding: .utf8)
            XCTAssertFalse(source.contains("start_outcome_unknown"),
                           "\(file): the text in Paul's screenshot cannot be renderable")
            XCTAssertFalse(source.contains("LiveTvStartBarrier"), file)
            XCTAssertFalse(source.contains("LiveTvFileBarrierStore"), file)
            XCTAssertFalse(source.contains("Wait 90 seconds"), file)
            XCTAssertFalse(source.contains("live_tv_storage_unavailable"),
                           "\(file): failing to persist a hint is not a reason to refuse a start")
        }
        // And the leftover marker file is cleaned up rather than left behind.
        let sources = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTv.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertTrue(sources.contains("live-tv-start.pending"),
                      "the legacy marker is named exactly once, to delete it")
        XCTAssertTrue(sources.contains("removeItem(at: Self.url(Self.legacyBarrierMarker))"))
    }

    func testEveryRenderKeyTheFixtureNamesHasCopy() throws {
        let cases = try startCases()
        var keys = Set(cases.answers.map(\.render))
        keys.insert("no_answer")
        for key in keys.sorted() {
            let description = try XCTUnwrap(LiveTvFailure(code: key).errorDescription, key)
            XCTAssertFalse(description.isEmpty, key)
            if key != "owner_unavailable" {
                XCTAssertNotEqual(description, LiveTvFailure(code: "unrecognised-code").errorDescription,
                                  "\(key) must have copy of its own, not the fallback")
            }
        }
        XCTAssertEqual(LiveTvFailure(code: "no_answer").errorDescription,
                       "The server did not answer. Press the channel again.")
    }

    // ---- the shared start-outcome fixture -------------------------------
    // tests/playback/live-tv-start-cases.json is the same file the web and
    // Android lease suites read. Three clients, one reducer: if this file and
    // tests/web/live-tv.test.js disagree, one of them is keeping a hint the
    // other throws away, and a tuner stays held for nothing.

    private struct StartCases: Decodable {
        struct Body: Decodable {
            let code: String?
            let retry: String?
            let ownerDecided: Bool?
        }
        struct Answer: Decodable {
            let `case`: String
            let transport: String?
            let status: Int?
            let body: Body?
            let render: String
            let offerRetry: Bool
            let keepHint: Bool
            let replay: Bool?
        }
        struct ResumeBody: Decodable {
            let outcome: String?
            /// The fixture's `live` session is a real activation document, so
            /// it decodes into the very type a successful start answers — which
            /// is what "shaped exactly like a successful start" has to mean if
            /// the resume is to enter playback through the same path.
            let session: LiveTvStarted?
        }
        struct ResumeCase: Decodable {
            let `case`: String
            let transport: String?
            let body: ResumeBody?
            let then: String
        }
        struct ChannelsResponse: Decodable {
            let protocols: [Int]?
        }
        struct ProtocolCase: Decodable {
            let `case`: String
            let channelsResponse: ChannelsResponse
            let requestId: Bool
            let recoveryRoutes: Bool
        }
        let answers: [Answer]
        let resume: [ResumeCase]
        let protocols: [ProtocolCase]
    }

    /// `.convertFromSnakeCase` over the whole document, exactly as the client
    /// decodes a live server response — which is also what proves the answer
    /// shape this reducer reads is the shape the server sends.
    private func startCases() throws -> StartCases {
        let url = try XCTUnwrap(
            Bundle(for: LiveTvTests.self).url(forResource: "live-tv-start-cases", withExtension: "json")
        )
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(StartCases.self, from: Data(contentsOf: url))
    }

    func testTheStartReducerAnswersEverySharedCase() throws {
        let cases = try startCases()
        XCTAssertFalse(cases.answers.isEmpty, "the fixture lost its answer cases")
        for row in cases.answers {
            // A timeout produced no body at all; an untyped HTTP failure
            // produced one with no `code`. Both reach the reducer as "nothing
            // typed a verdict".
            let answer = LiveTvStartAnswer(
                status: row.status,
                code: row.transport == "timeout" ? nil : row.body?.code,
                retry: row.body?.retry,
                ownerDecided: row.body?.ownerDecided ?? false)
            let verdict = LiveTvStartReducer.verdict(answer)
            XCTAssertEqual(verdict.render, row.render, row.`case`)
            XCTAssertEqual(verdict.offerRetry, row.offerRetry, row.`case`)
            XCTAssertEqual(verdict.keepHint, row.keepHint, row.`case`)
            XCTAssertEqual(verdict.replay, row.replay ?? false, row.`case`)
        }
        // The fixture is the contract, but these two are the point of it: a
        // code nobody has ever heard of is not a verdict, and no answer at all
        // is never a refusal.
        XCTAssertEqual(LiveTvStartReducer.verdict(LiveTvStartAnswer(status: 503, code: "from_the_future")).render,
                       "owner_unavailable")
        XCTAssertTrue(LiveTvStartReducer.verdict(LiveTvStartAnswer(status: 503, code: "from_the_future")).keepHint)
        XCTAssertEqual(LiveTvStartReducer.verdict(LiveTvStartAnswer()), LiveTvStartReducer.noAnswer)
    }

    func testAThrownFailureBecomesTheAnswerTheReducerReads() {
        // The path a real POST takes: `LiveTvAPI.request` throws a typed
        // failure carrying the envelope's two flat fields, or `no_answer`, or
        // a bare URLError. All three must reduce the fixture's way.
        let typed = LiveTvStartAnswer(error: LiveTvFailure(
            code: "tuner_unavailable", retry: "now", ownerDecided: true, status: 503))
        XCTAssertEqual(LiveTvStartReducer.verdict(typed).render, "tuner_unavailable")
        XCTAssertFalse(LiveTvStartReducer.verdict(typed).keepHint)

        let untyped = LiveTvStartAnswer(error: LiveTvFailure(code: "no_answer", status: 404))
        XCTAssertNil(untyped.code, "`no_answer` is this client's word, not a code the server sent")
        XCTAssertEqual(LiveTvStartReducer.verdict(untyped), LiveTvStartReducer.noAnswer)

        let dropped = LiveTvStartAnswer(error: URLError(.timedOut))
        XCTAssertNil(dropped.code)
        XCTAssertEqual(LiveTvStartReducer.verdict(dropped), LiveTvStartReducer.noAnswer)
    }

    func testTheResumeReducerAnswersEverySharedCase() throws {
        let cases = try startCases()
        XCTAssertFalse(cases.resume.isEmpty, "the fixture lost its resume cases")
        for row in cases.resume {
            let outcome = row.transport == "timeout" ? nil : row.body?.outcome
            XCTAssertEqual(LiveTvStartReducer.resumeAction(outcome: outcome).rawValue, row.then, row.`case`)
            guard row.then == "reattach" else {
                XCTAssertNil(row.body?.session, "only a live answer carries a session: \(row.`case`)")
                continue
            }
            // The reattach row must decode into the same document a start
            // answers, or `attach` could not take it: capability, channel and
            // the live flag, with no client-side guessing in between.
            let session = try XCTUnwrap(row.body?.session, row.`case`)
            XCTAssertFalse(session.sessionId.isEmpty)
            XCTAssertTrue(session.live)
            XCTAssertFalse(session.channel.id.isEmpty)
            XCTAssertTrue(session.channel.watchable, "a resumed session is one the viewer can watch")
            XCTAssertEqual(session.channel.title, "7.1 · WABC")
        }
        // An outcome from a newer server keeps the hint and shows the list: it
        // is not proof the session ended.
        XCTAssertEqual(LiveTvStartReducer.resumeAction(outcome: "something_new"), .keepHintWait)
    }

    func testProtocolNegotiationDecidesRequestIdsAndRecoveryRoutes() throws {
        let cases = try startCases()
        XCTAssertEqual(cases.protocols.count, 3)
        for row in cases.protocols {
            let negotiated = LiveTvStartReducer.negotiatesRecovery(
                protocols: row.channelsResponse.protocols)
            XCTAssertEqual(negotiated, row.requestId, row.`case`)
            XCTAssertEqual(negotiated, row.recoveryRoutes, row.`case`)
        }
        // A channels response with no `protocols` field decodes to nil rather
        // than to an empty list, which is what makes an older ingress legacy
        // rather than a server that negotiated nothing.
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let legacy = try decoder.decode(LiveTvLineup.self, from: Data(#"""
        {"channels":[],"freshness":"fresh","age_seconds":1}
        """#.utf8))
        XCTAssertNil(legacy.protocols)
        let negotiated = try decoder.decode(LiveTvLineup.self, from: Data(#"""
        {"channels":[],"freshness":"fresh","age_seconds":1,"protocols":[1,2,3]}
        """#.utf8))
        XCTAssertEqual(negotiated.protocols, [1, 2, 3])
    }

    // ---- the advisory enable card ---------------------------------------

    /// Paul's standing rule: nothing in the code gates a feature; the Developer
    /// tab says what is needed and whether each part is met, and never refuses
    /// the enable. These two tests are that rule, pinned.
    func testTheDeveloperCardDrawsEveryReadinessRowTheServerSendsAndGatesNothing() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvDeveloperView.swift")
                .standardizedFileURL,
            encoding: .utf8)
        // Both cards iterate the server's array rather than naming rows. That
        // is what makes `start_recovery` — and the next row nobody has written
        // yet — appear without a client change.
        XCTAssertEqual(source.components(separatedBy: "ForEach(readiness.checks) { check in").count - 1, 1)
        XCTAssertEqual(source.components(separatedBy: "ForEach(guideReadiness.checks) { check in").count - 1, 1)
        XCTAssertFalse(source.contains("check.id =="),
                       "a hand-written subset silently drops the check nobody remembered")
        XCTAssertFalse(source.contains("\"start_recovery\""),
                       "start_recovery needs no special case; it is a row like any other")
        XCTAssertFalse(source.contains("\"guide_age\""))
        // Met/unmet colouring, and nothing else, hangs off `ready`.
        XCTAssertEqual(
            source.components(separatedBy: "systemImage: check.ready ? \"checkmark.circle\" : \"exclamationmark.triangle\"").count - 1,
            2)
        // Advisory means advisory: no readiness value may reach a `disabled`.
        for gate in ["disabled(!readiness", "disabled(readiness", "disabled(!guideReadiness",
                     "disabled(guideReadiness", "readiness.ready ||", "!readiness.ready"] {
            XCTAssertFalse(source.contains(gate), "\(gate) would let an advisory check block an operator")
        }
        // The enable and save controls gate on exactly what they always did:
        // an in-flight request and unsaved edits. Never on a check.
        XCTAssertTrue(source.contains("Button(saved.liveTvEnabled ? \"Disable Live TV and drain sessions\" : \"Enable Live TV\")"))
        XCTAssertTrue(source.contains("}.disabled(busy || dirty)"))
        // And a guide card that cannot be read is an empty card.
        XCTAssertTrue(source.contains("guideReadiness = try? await api.guideReadiness()"))
    }

    func testTheGuideReadinessCardSurvivesAServerThatSendsMoreThanThisBuildKnows() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let card = try decoder.decode(LiveTvGuideReadiness.self, from: Data(#"""
        {"advisory":true,"source":"schedules_direct","guide_hours":6,"freshness":"fresh",
         "age_seconds":42,"fetched_at":1700000000,"matched_channels":11,"lineup_channels":12,
         "programmes":480,"refresh_interval_seconds":1200,"a_field_from_the_future":{"x":1},
         "checks":[
           {"id":"guide_age","ready":true,"message":"The cached guide is 42 seconds old."},
           {"id":"guide_next_refresh","ready":true,"message":"The owner's loop next runs in 58 seconds."},
           {"id":"guide_persisted","ready":true,"message":"A copy is on disk."},
           {"id":"guide_last_error","ready":false,"message":"The last refresh failed: upstream 502"},
           {"id":"a_check_from_the_future","ready":false,"message":"Something this build has never heard of."}
         ]}
        """#.utf8))
        XCTAssertTrue(card.advisory)
        XCTAssertEqual(card.source, "schedules_direct")
        XCTAssertEqual(card.programmes, 480)
        XCTAssertEqual(card.matchedChannels, 11)
        XCTAssertEqual(card.checks.map(\.id),
                       ["guide_age", "guide_next_refresh", "guide_persisted", "guide_last_error",
                        "a_check_from_the_future"],
                       "every row the server sends is kept, including one this build cannot name")
        XCTAssertFalse(card.allMet)
        XCTAssertTrue(card.checks.filter(\.ready).count == 3)
        // A sparse or hostile document is an empty card, never a thrown error:
        // the operator must still be able to save and enable.
        let sparse = try decoder.decode(LiveTvGuideReadiness.self, from: Data(#"{}"#.utf8))
        XCTAssertTrue(sparse.checks.isEmpty)
        XCTAssertTrue(sparse.allMet, "no rows is not an unmet row")
        XCTAssertTrue(sparse.advisory)
        let wrongTypes = try decoder.decode(LiveTvGuideReadiness.self, from: Data(#"""
        {"advisory":"yes","source":7,"programmes":"many","checks":[]}
        """#.utf8))
        XCTAssertEqual(wrongTypes.source, "unknown")
        XCTAssertEqual(wrongTypes.programmes, 0)
    }

    /// The session card already answered `start_recovery` without a change:
    /// `POST /live-tv/readiness/refresh` and `GET /live-tv/readiness` are the
    /// same document, and the view draws its whole `checks` array.
    func testTheSessionReadinessCardCarriesStartRecoveryWithoutAClientChange() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let readiness = try decoder.decode(LiveTvReadiness.self, from: Data(#"""
        {"ready":false,"enabled":true,"owner_node_id":"owner","generation":12,"checks":[
          {"id":"configuration","ready":true,"message":"The saved address is valid"},
          {"id":"start_recovery","ready":false,"message":"Start recovery needs protocol 3 on both sides."}
        ]}
        """#.utf8))
        XCTAssertEqual(readiness.checks.map(\.id), ["configuration", "start_recovery"])
        XCTAssertEqual(readiness.checks.last?.ready, false)
        XCTAssertEqual(readiness.generation, 12)
    }

    // ---- the press, the open, and the release ---------------------------

    func testSwitchConfirmsReleaseBeforeStartingAnotherChannel() async throws {
        let requests = LiveTvMockRequests(result: started())
        let lease = LiveTvLease(requests: requests, hints: LiveTvMemoryHintStore())
        _ = try await lease.start("first")
        requests.result = started("two")
        _ = try await lease.start("second")
        try await lease.stop()
        try await lease.stop()
        XCTAssertEqual(requests.events, ["start:first", "release:one", "start:second", "release:two"])
        XCTAssertNil(lease.current)
    }

    func testFailedReleaseRetainsCapabilityAndBlocksNewAllocation() async throws {
        let requests = LiveTvMockRequests(result: started())
        let lease = LiveTvLease(requests: requests, hints: LiveTvMemoryHintStore())
        _ = try await lease.start("first")
        requests.releaseFails = true
        do { _ = try await lease.start("second"); XCTFail("cleanup must block a new tuner") } catch {}
        XCTAssertEqual(lease.current?.sessionId, "one")
        XCTAssertEqual(requests.events, ["start:first", "release:one"])
        requests.releaseFails = false
        try await lease.stop()
        XCTAssertNil(lease.current)
    }

    func testLateStartAfterCloseIsReleasedBeforeCloseCompletes() async throws {
        let requests = LiveTvMockRequests(result: started())
        let entered = expectation(description: "POST entered")
        requests.started = entered
        requests.holdStart = true
        let lease = LiveTvLease(requests: requests, hints: LiveTvMemoryHintStore())
        let opening = Task { try await lease.start("first") }
        await fulfillment(of: [entered], timeout: 2)
        let closing = Task { try await lease.stop() }
        // Give the close operation a turn to advance the generation; no real
        // time or network timing is needed to order the mocked start response.
        await Task.yield()
        requests.continuation?.resume(returning: started())
        let result = try await opening.value
        try await closing.value
        XCTAssertNil(result)
        XCTAssertNil(lease.current)
        XCTAssertEqual(requests.events, ["start:first", "release:one"])
    }

    func testAnAnswerThatNeverCameIsReplayedOnceWithTheSameIdAndKeepsItsHint() async throws {
        let hints = LiveTvMemoryHintStore()
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]
        requests.startFailure = URLError(.timedOut)
        let lease = LiveTvLease(requests: requests, hints: hints)
        do { _ = try await lease.start("first"); XCTFail("a lost answer is still a failure to render") }
        catch let failure as LiveTvFailure { XCTAssertEqual(failure.code, "no_answer") }
        XCTAssertEqual(requests.events, ["start:first", "start:first"],
                       "start_replay_attempts is one: the POST is sent twice, never three times")
        XCTAssertEqual(Set(requests.startRequestIds).count, 1,
                       "the replay carries the SAME request id, so the owner joins it to one session")
        let id = try XCTUnwrap(requests.startRequestIds.first ?? nil)
        XCTAssertTrue(LiveTvStartReducer.isRequestId(id))
        XCTAssertEqual(hints.value?.requestId, id, "a lost answer keeps the only handle on the start")
    }

    func testAnOwnerDecidedRefusalForgetsTheHintAndNeverRefusesTheNextPress() async throws {
        let hints = LiveTvMemoryHintStore()
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]
        requests.startFailure = LiveTvFailure(code: "tuner_unavailable", retry: "now",
                                              ownerDecided: true, status: 503)
        let lease = LiveTvLease(requests: requests, hints: hints)
        do { _ = try await lease.start("first"); XCTFail("a typed refusal is still a failure") }
        catch let failure as LiveTvFailure { XCTAssertEqual(failure.code, "tuner_unavailable") }
        XCTAssertEqual(requests.events, ["start:first"], "a typed verdict is not replayed")
        XCTAssertNil(hints.value, "the owner decided, so there is nothing left to retire")

        // And the press after it goes straight to the server. No barrier, no
        // monotonic wait, no quarantine.
        requests.startFailure = nil
        let info = try await lease.start("second")
        XCTAssertEqual(info?.sessionId, "one")
        XCTAssertEqual(requests.events, ["start:first", "start:second"])
    }

    func testALegacyIngressGetsNoRequestIdAndKeepsAnyHintForLater() async throws {
        let hints = LiveTvMemoryHintStore()
        let stale = LiveTvStartHint(requestId: String(repeating: "b", count: 32), touchedAt: 1)
        try hints.write(stale)
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = nil            // an ingress older than the contract
        let lease = LiveTvLease(requests: requests, hints: hints)
        _ = try await lease.start("first")
        XCTAssertEqual(requests.startRequestIds, [nil],
                       "an older ingress rejects the field outright — it must not be sent")
        XCTAssertFalse(requests.events.contains { $0.hasPrefix("retire:") },
                       "there are no recovery routes to call")
        XCTAssertEqual(hints.value, stale, "the hint is kept for a server that later negotiates 3")
        try await lease.stop()
    }

    func testAHintLeftByAKilledProcessIsRetiredInTheBackgroundAndDoesNotDelayThePress() async throws {
        let hints = LiveTvMemoryHintStore()
        let orphan = String(repeating: "c", count: 32)
        // Touched a second ago: exactly the process that was killed while
        // playing, which is the case that must be retired rather than left to
        // the owner's 45 s reap.
        try hints.write(LiveTvStartHint(
            requestId: orphan,
            touchedAt: Int64(Date().timeIntervalSince1970 * 1000) - 1_000))
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]
        let entered = expectation(description: "retire entered")
        requests.retireEntered = entered
        requests.holdRetire = true
        let lease = LiveTvLease(requests: requests, hints: hints)

        let info = try await lease.start("first")
        XCTAssertEqual(info?.sessionId, "one", "the press completes while the retire is still in flight")
        XCTAssertFalse(requests.retireFinished, "the retire is fire-and-forget")
        await fulfillment(of: [entered], timeout: 2)
        XCTAssertTrue(requests.events.contains("retire:\(orphan)"),
                      "the orphan a killed process left behind is retired")
        // The new press owns the hint now; a late retire answer must not take
        // it away.
        let pressed = try XCTUnwrap(hints.value)
        XCTAssertNotEqual(pressed.requestId, orphan)
        requests.retireContinuation?.resume(returning: ())
        try await Task.sleep(nanoseconds: 50_000_000)
        XCTAssertTrue(requests.retireFinished)
        XCTAssertEqual(hints.value?.requestId, pressed.requestId,
                       "a retire that belongs to the previous start cannot forget this one's hint")
        try await lease.stop()
    }

    func testAConfirmedReleaseForgetsTheHintSoACleanBackgroundShowsTheList() async throws {
        let hints = LiveTvMemoryHintStore()
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]
        let lease = LiveTvLease(requests: requests, hints: hints)
        _ = try await lease.start("first")
        XCTAssertNotNil(hints.value, "a live session keeps its handle until DELETE confirms")
        try await lease.stop()
        XCTAssertNil(hints.value, "a confirmed release is a confirmed end — nothing to resume")
    }

    func testResumeReattachesALiveSessionAndClearsTheHintOnlyWhenTheOwnerSaysItIsOver() async throws {
        let hints = LiveTvMemoryHintStore()
        let id = String(repeating: "d", count: 32)
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]

        // live → the same stream back, with no press and no start.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeAnswer = LiveTvResumeAnswer(outcome: "live", session: started("resumed"))
        let live = LiveTvLease(requests: requests, hints: hints)
        let recovered = await live.resumeIfRecent()
        XCTAssertEqual(recovered?.sessionId, "resumed")
        XCTAssertEqual(live.current?.sessionId, "resumed")
        XCTAssertEqual(requests.events, ["resume:\(id)"], "resuming is not starting")
        XCTAssertEqual(hints.value?.requestId, id)
        try await live.stop()
        XCTAssertNil(hints.value)

        // pending → keep the hint, show the list, touch nothing.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeAnswer = LiveTvResumeAnswer(outcome: "pending")
        let pending = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(pending)
        XCTAssertEqual(hints.value?.requestId, id)

        // ended → forget it; the first press starts a fresh session.
        requests.resumeAnswer = LiveTvResumeAnswer(outcome: "ended")
        let ended = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(ended)
        XCTAssertNil(hints.value)

        // A resume that never answered keeps the hint, exactly as a timeout on
        // a start does.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeAnswer = nil
        requests.resumeFailure = URLError(.timedOut)
        let unanswered = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(unanswered)
        XCTAssertEqual(hints.value?.requestId, id)
    }

    func testARefusedResumeForgetsTheHintOnlyWhenTheOwnerDecidedIt() async throws {
        let hints = LiveTvMemoryHintStore()
        let id = String(repeating: "f", count: 32)
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]

        // The owner decided: this request id is spent. Keeping the hint would
        // make the next press waste a retire on it and would run this resume
        // again at every launch, for ever.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeFailure = LiveTvFailure(code: "channel_not_found", retry: "never",
                                               ownerDecided: true, status: 409)
        let decided = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(decided)
        XCTAssertNil(hints.value, "an owner-decided refusal spends the id")

        // The ingress refused the request itself before any owner saw it —
        // also spent, and for the same reason the start reducer says so.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeFailure = LiveTvFailure(code: "invalid_request", status: 400)
        let refusedById = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(refusedById)
        XCTAssertNil(hints.value)
    }

    func testARefusedResumeTheOwnerDidNotDecideKeepsTheHint() async throws {
        let hints = LiveTvMemoryHintStore()
        let id = String(repeating: "0", count: 32)
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]

        // A deploy blip. The ingress never reached the owner, so nothing here
        // proves the session is gone — and a peer failure is stamped
        // `owner_decided: false` precisely so this case keeps its handle.
        for refusal in [LiveTvFailure(code: "owner_unavailable", retry: "now",
                                      ownerDecided: false, status: 503),
                        // A typed refusal minted outside the Live TV module
                        // carries no verdict at all.
                        LiveTvFailure(code: "node_maintenance", status: 503),
                        // An older ingress has no resume route to answer with.
                        LiveTvFailure(code: "owner_unavailable", status: 404)] {
            try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
            requests.resumeFailure = refusal
            let kept = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
            XCTAssertNil(kept)
            XCTAssertEqual(hints.value?.requestId, id,
                           "\(refusal.code) is not proof the session is gone")
        }

        // And the transport failure the fixture already pins, for the same
        // reason: no answer is not a verdict.
        try hints.write(LiveTvStartHint(requestId: id, touchedAt: 1))
        requests.resumeFailure = URLError(.timedOut)
        let dropped = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(dropped)
        XCTAssertEqual(hints.value?.requestId, id)
    }

    func testResumeIsNotAttemptedWithoutAHintOrWithoutTheRecoveryRoutes() async throws {
        let hints = LiveTvMemoryHintStore()
        let requests = LiveTvMockRequests(result: started())
        requests.protocols = [1, 2, 3]
        let noHint = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(noHint)
        XCTAssertEqual(requests.events, [], "no hint, no question to ask")

        try hints.write(LiveTvStartHint(requestId: String(repeating: "e", count: 32), touchedAt: 1))
        requests.protocols = [1, 2]
        let legacy = await LiveTvLease(requests: requests, hints: hints).resumeIfRecent()
        XCTAssertNil(legacy)
        XCTAssertEqual(requests.events, [], "an owner that does not retire starts has no resume either")
        XCTAssertNotNil(hints.value, "and the hint survives for a server that later does")
    }

    private let channel = LiveTvChannel(id: "7.1", guideNumber: "7.1", guideName: "Local",
                                        favorite: false, drm: false, support: "ready",
                                        hd: nil, videoCodec: nil, audioCodec: nil)

    private func started(_ capability: String = "one") -> LiveTvStarted {
        LiveTvStarted(sessionId: capability, channel: channel, live: true)
    }

    private func liveTvViewSource() throws -> String {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        return try String(
            contentsOf: testsDirectory.appendingPathComponent(
                "../Sources/LiveTvView.swift"
            ).standardizedFileURL,
            encoding: .utf8
        )
    }

    private func themeSource() throws -> String {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        return try String(
            contentsOf: testsDirectory.appendingPathComponent(
                "../Sources/Theme.swift"
            ).standardizedFileURL,
            encoding: .utf8
        )
    }

    func testTheLiveSurfacePillRendererDoesNotShadowButtonStyleBody() throws {
        let source = try themeSource()
        let style = source
            .components(separatedBy: "struct LiveSurfacePillStyle: ButtonStyle {")[1]
            .components(separatedBy: "/// Compact trailing action")[0]
        XCTAssertTrue(style.contains("Renderer(configuration: configuration)"))
        XCTAssertTrue(style.contains("private struct Renderer: View"))
        XCTAssertFalse(style.contains("struct Body: View"))
    }

    func testProtectedChannelsStayVisibleAndUnwatchable() {
        let protected = LiveTvChannel(id: "107.1", guideNumber: "107.1", guideName: "Protected",
                                      favorite: false, drm: true, support: "drm_unsupported",
                                      hd: nil, videoCodec: nil, audioCodec: nil)
        XCTAssertFalse(protected.watchable)
        XCTAssertEqual(protected.title, "107.1 · Protected")
        XCTAssertTrue(channel.watchable)
    }

    func testLineupFormatsAndLiveSignalDecodeWithoutGuessing() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let formatted = try decoder.decode(LiveTvChannel.self, from: Data(#"""
        {"id":"7.1","guide_number":"7.1","guide_name":"Fixture News",
         "favorite":false,"drm":false,"support":"ready","hd":true,
         "video_codec":"HEVC","audio_codec":"AC4"}
        """#.utf8))
        XCTAssertEqual(formatted.formatBadges, ["HD", "HEVC", "AC4"])
        XCTAssertEqual(formatted.sourceFormatDescription, "HD · HEVC video · AC4 audio")
        XCTAssertTrue(channel.formatBadges.isEmpty)

        let measured = LiveTvChannel(
            id: "5.1", guideNumber: "5.1", guideName: "WXYZ", favorite: false,
            drm: false, support: "ready", hd: true, videoCodec: "hevc", audioCodec: "ac3",
            sourceFormat: LiveTvSourceFormat(
                videoWidth: 3840, videoHeight: 2160, scan: "progressive",
                audioChannels: 6, audioLayout: "5.1", observedAt: 1_788_998_400
            )
        )
        XCTAssertEqual(measured.formatBadges, ["4K", "HEVC", "AC3 5.1"])
        XCTAssertEqual(measured.sourceFormatDescription, "3840×2160p · HEVC video · AC3 5.1 audio")

        let malformed = try decoder.decode(LiveTvChannel.self, from: Data(#"""
        {"id":"9.1","guide_number":"9.1","guide_name":"Bad Optional",
         "source_format":{"video_width":-1,"video_height":1080,"scan":"wrong",
         "audio_channels":2,"audio_layout":"stereo","observed_at":1788998400}}
        """#.utf8))
        XCTAssertNil(malformed.sourceFormat?.videoWidth)
        XCTAssertEqual(malformed.sourceFormat?.videoHeight, 1080)
        XCTAssertNil(malformed.sourceFormat?.scan)
        XCTAssertEqual(malformed.sourceFormat?.audioLayout, "stereo")

        let status = try decoder.decode(LiveTvStatus.self, from: Data(#"""
        {"state":"active","owner_node_id":"owner","encoder":"vaapi","output_height":720,
         "signal":{"strength_percent":96,"quality_percent":89,"symbol_quality_percent":100}}
        """#.utf8))
        XCTAssertEqual(status.signal?.strengthPercent, 96)
        XCTAssertEqual(status.signal?.qualityPercent, 89)
        XCTAssertEqual(status.signal?.symbolQualityPercent, 100)
    }

    func testNoProgressBudgetUsesMonotonicTimeAndRejectsNaN() {
        var now = ContinuousClock.now
        var progress = LiveTvPlaybackWatchdog(now: { now })
        XCTAssertFalse(progress.observe(position: .nan))
        now = now.advanced(by: .seconds(20))
        XCTAssertTrue(progress.observe(position: 1))
        now = now.advanced(by: .seconds(29))
        XCTAssertFalse(progress.expired)
        XCTAssertFalse(progress.observe(position: 1))
        now = now.advanced(by: .seconds(1))
        XCTAssertTrue(progress.expired)
    }

    func testPlayerNetworkFailureIsNotMisreportedAsCodecFailure() {
        XCTAssertEqual(LiveTvPlayerController.playerFailure(URLError(.networkConnectionLost)).code, "stream_failed")
        XCTAssertEqual(LiveTvPlayerController.playerFailure(nil).code, "stream_failed")
        let decoder = NSError(domain: AVFoundationErrorDomain, code: AVError.Code.decoderNotFound.rawValue)
        XCTAssertEqual(LiveTvPlayerController.playerFailure(decoder).code, "codec_unsupported")
    }

    func testWaitingIsOnlyDrawnForAStallAndNeverForBufferingRateEvaluation() {
        XCTAssertTrue(LiveTvPlayerController.waitingDecision(
            status: .waitingToPlayAtSpecifiedRate,
            reason: .toMinimizeStalls
        ))
        for reason: AVPlayer.WaitingReason? in [
            .evaluatingBufferingRate, .noItemToPlay, nil,
        ] {
            XCTAssertFalse(LiveTvPlayerController.waitingDecision(
                status: .waitingToPlayAtSpecifiedRate,
                reason: reason
            ))
        }
        for status: AVPlayer.TimeControlStatus in [.playing, .paused] {
            for reason: AVPlayer.WaitingReason? in [
                .toMinimizeStalls, .evaluatingBufferingRate, .noItemToPlay, nil,
            ] {
                XCTAssertFalse(LiveTvPlayerController.waitingDecision(
                    status: status,
                    reason: reason
                ))
            }
        }
    }

    func testWaitingIsDebouncedAndClearsOnDetach() async {
        let controller = LiveTvPlayerController.testing(
            requests: LiveTvMockRequests(result: started())
        )
        controller.applyTimeControl(
            status: .waitingToPlayAtSpecifiedRate,
            reason: .toMinimizeStalls
        )
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertFalse(controller.waiting, "a 200 ms transport pause is not viewer-facing")
        controller.applyTimeControl(status: .playing, reason: nil)
        try? await Task.sleep(nanoseconds: 200_000_000)
        XCTAssertFalse(controller.waiting, "clearing a short wait must cancel its delayed publish")

        controller.applyTimeControl(
            status: .waitingToPlayAtSpecifiedRate,
            reason: .toMinimizeStalls
        )
        try? await Task.sleep(nanoseconds: 400_000_000)
        XCTAssertTrue(controller.waiting, "350 ms of sustained stall must become visible")
        await controller.stop()
        XCTAssertFalse(controller.waiting)
        XCTAssertNil(controller.behindEdgeSeconds)
        XCTAssertNil(controller.bufferedSeconds)
    }

    func testTheTelemetryStripOmitsWhatItDoesNotKnow() {
        let output = LiveTvDeliveryOutput(
            container: "mpegts", videoCodec: "mpeg2video", audioCodec: "ac3",
            width: 1920, height: 1080, bitDepth: nil, frameRate: nil,
            hdr: nil, audioChannels: 6
        )
        let copied = LiveTvDelivery(
            output: output, videoAction: "copy", audioAction: "copy",
            packaging: "mpegts"
        )
        let empty = LiveTvView.liveSurfaceChips(
            status: nil, delivery: nil, behindEdge: nil, paused: nil
        )
        XCTAssertNil(empty.first { $0.kind == .signal })
        XCTAssertNil(empty.first { $0.kind == .behind })

        let status = LiveTvStatus(
            state: "active", channel: nil, ownerNodeId: "owner",
            encoder: "pending", outputHeight: 720,
            signal: LiveTvSignal(
                strengthPercent: 92, qualityPercent: nil,
                symbolQualityPercent: nil
            )
        )
        let direct = LiveTvView.liveSurfaceChips(
            status: status, delivery: copied, behindEdge: nil, paused: nil
        )
        XCTAssertTrue(direct.first { $0.kind == .signal }?.text.contains("92% signal") == true)
        XCTAssertFalse(direct.first { $0.kind == .signal }?.text.contains("quality") == true)
        XCTAssertEqual(direct.first { $0.kind == .method }?.text, "direct mpeg2video")
        XCTAssertEqual(direct.first { $0.kind == .audio }?.text, "ac3 5.1 copied")

        let encoded = LiveTvDelivery(
            output: LiveTvDeliveryOutput(
                container: "mpegts", videoCodec: "h264", audioCodec: "aac",
                width: 1280, height: 720, bitDepth: nil, frameRate: nil,
                hdr: nil, audioChannels: 2
            ),
            videoAction: "encode", audioAction: "encode", packaging: "mpegts"
        )
        let transcode = LiveTvView.liveSurfaceChips(
            status: nil, delivery: encoded, behindEdge: 3.25, paused: nil
        )
        XCTAssertEqual(transcode.first { $0.kind == .method }?.text, "transcode 720p h264")
        XCTAssertEqual(transcode.first { $0.kind == .behind }?.text, "3.2 s behind the edge")
    }

    func testTheSurfaceMessageIsNotTheLineupMessage() async throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent(
                "../Sources/LiveTvView.swift"
            ).standardizedFileURL,
            encoding: .utf8
        )
        let load = source
            .components(separatedBy: "func load(origin: String, token: String?) async {")[1]
            .components(separatedBy: "private func resumeIfRecent")[0]
        XCTAssertFalse(load.contains("surfaceMessage"),
                       "loading lineup copy must never write on the picture")

        let controller = LiveTvPlayerController.testing(
            requests: LiveTvMockRequests(result: started())
        )
        XCTAssertNil(controller.surfaceMessage)
        await controller.watch(channel)
        XCTAssertEqual(controller.message, "Playing live · no recording or rewind")
        XCTAssertNil(controller.surfaceMessage, "attach clears stale surface copy")
        controller.togglePause()
        XCTAssertEqual(
            controller.surfaceMessage,
            "Paused. The tuner is released after 30 seconds without playback; resuming has no rewind guarantee."
        )
        await controller.stop()
        await controller.watch(channel)
        XCTAssertNil(controller.surfaceMessage, "a fresh attachment owns a fresh surface")
        await controller.stop()
    }

    func testTheRevealLayerIsNotFocusableWhileTheOverlayOrTheGuideIsVisible() throws {
        let source = try liveTvViewSource()
        let fullscreen = source
            .components(separatedBy: "private var fullscreenSurface: some View {")[1]
            .components(separatedBy: "private func applyLiveOutcome")[0]
        XCTAssertTrue(fullscreen.contains(
            ".focusable(!overlayVisible && !temporaryGuide)"
        ))
        XCTAssertFalse(fullscreen.contains(".focusable(true)"))
    }

    func testFullscreenFocusDefaultsToPauseAndReturnsThereFromTheRevealLayer() throws {
        let source = try liveTvViewSource()
        let fullscreen = source
            .components(separatedBy: "private var fullscreenSurface: some View {")[1]
            .components(separatedBy: "private func applyLiveOutcome")[0]
        XCTAssertTrue(fullscreen.contains(
            ".onAppear { focusedControl = overlayVisible ? .play : .reveal }"
        ))
        XCTAssertTrue(fullscreen.contains(
            "target == .reveal { focusedControl = .play }"
        ))
        for guardPart in [
            "guard fullscreen",
            "!overlayVisible",
            "!temporaryGuide",
            "!showingInfo",
            "!showingMore",
            "!showingLayout",
            "detail == nil",
        ] {
            XCTAssertTrue(fullscreen.contains(guardPart), guardPart)
        }
        let infoDismissal = fullscreen
            .components(separatedBy: ".onChange(of: showingInfo)")[1]
            .components(separatedBy: ".onChange(of: showingMore)")[0]
        XCTAssertTrue(infoDismissal.contains(
            "guard !visible, fullscreen, overlayVisible else { return }"
        ))
        XCTAssertTrue(infoDismissal.contains("focusedControl = nil"))
        XCTAssertTrue(infoDismissal.contains("await Task.yield()"))
        XCTAssertTrue(infoDismissal.contains("focusedControl = .play"))
    }

    func testTheProgressRowSurvivesAMissingNextProgramme() {
        let programme = LiveTvProgramme(
            start: 1_700_000_000,
            end: 1_700_001_800,
            title: "The programme"
        )
        let values = LiveTvView.liveProgressText(
            airing: LiveTvAiring(now: programme, next: nil, progress: 0.5),
            now: 1_700_000_600
        )
        XCTAssertEqual(values.count, 3)
        XCTAssertFalse(values.contains { $0.contains("Next") })
        XCTAssertEqual(values.last, "20 min left")
    }

    func testFiveFullscreenActionsAndNoFavorite() throws {
        let source = try liveTvViewSource()
        let buttons = source
            .components(separatedBy: "private var liveSurfaceButtons: some View {")[1]
            .components(separatedBy: "#endif")[0]
        XCTAssertEqual(buttons.components(separatedBy: "Label(").count - 1, 5)
        for action in [
            "\"Play live\" : \"Pause\"",
            "Label(\"Guide\"",
            "Label(\"Channels\"",
            "Label(\"Info\"",
            "Label(\"More\"",
        ] {
            XCTAssertTrue(buttons.contains(action), action)
        }
        XCTAssertFalse(buttons.contains("Favorite"))
    }

    func testTheStreamInfoPanelReadsEveryFieldTheModelsCarryAndSumsTheAccessLog() {
        #if os(tvOS)
        let current = LiveTvProgramme(
            start: 1_700_000_000,
            end: 1_700_001_800,
            title: "The Late Edition",
            episodeTitle: "Tuesday",
            episode: "S12 E184",
            synopsis: "Local news and weather.",
            originalAirDate: "2026-09-13",
            filters: ["TV-PG"]
        )
        let next = LiveTvProgramme(
            start: 1_700_001_800,
            end: 1_700_003_600,
            title: "Local Weather Tonight"
        )
        let source = LiveTvSourceFormat(
            videoWidth: 1920,
            videoHeight: 1080,
            scan: "interlaced",
            audioChannels: 6,
            audioLayout: "5.1",
            observedAt: 1_700_000_000
        )
        let channel = LiveTvChannel(
            id: "7.1",
            guideNumber: "7.1",
            guideName: "WPLX-DT",
            favorite: true,
            drm: false,
            support: "ready",
            hd: true,
            videoCodec: "mpeg2video",
            audioCodec: "ac3",
            sourceFormat: source
        )
        let delivery = LiveTvDelivery(
            output: LiveTvDeliveryOutput(
                container: "mpegts",
                videoCodec: "h264",
                audioCodec: "ac3",
                width: 1280,
                height: 720,
                bitDepth: 8,
                frameRate: nil,
                hdr: nil,
                audioChannels: 6
            ),
            videoAction: "encode",
            audioAction: "copy",
            packaging: "mpegts"
        )
        let status = LiveTvStatus(
            state: "active",
            channel: channel,
            ownerNodeId: "media1",
            encoder: "nvenc",
            outputHeight: 720,
            signal: LiveTvSignal(
                strengthPercent: 92,
                qualityPercent: 100,
                symbolQualityPercent: 100
            ),
            delivery: delivery
        )
        let asOf = Date(timeIntervalSince1970: 1_700_000_900)
        let player = LiveTvPlayerFacts.from(
            events: [
                LiveTvAccessEventFacts(
                    observedBitrate: 6_000_000,
                    droppedFrames: 2,
                    stalls: 1
                ),
                LiveTvAccessEventFacts(
                    observedBitrate: 8_000_000,
                    droppedFrames: 3,
                    stalls: 2
                ),
            ],
            behindEdgeSeconds: 4.2,
            bufferedSeconds: 3.8,
            attachedAt: Date(timeIntervalSince1970: 1_700_000_000),
            asOf: asOf
        )
        let rows = LiveTvStreamInfoPanel.rows(
            programme: LiveTvAiring(now: current, next: next, progress: 0.5),
            channel: channel,
            status: status,
            delivery: delivery,
            player: player
        )
        XCTAssertEqual(rows.map(\.label), [
            "title", "episode", "airing", "next", "synopsis", "aired",
            "channel", "source", "observed",
            "method", "video", "audio", "stream",
            "strength", "quality", "symbol",
            "behind the edge · buffered", "rate", "session",
        ])
        let rate = rows.first { $0.label == "rate" }?.value
        XCTAssertTrue(rate?.contains("8.0 Mb/s observed") == true)
        XCTAssertTrue(rate?.contains("5 dropped frames") == true)
        XCTAssertTrue(rate?.contains("3 stalls") == true)
        XCTAssertFalse(rows.first { $0.label == "stream" }?.value.contains("segments") == true)

        let unknown = LiveTvPlayerFacts.from(
            events: [
                LiveTvAccessEventFacts(
                    observedBitrate: -1,
                    droppedFrames: -1,
                    stalls: -1
                ),
            ],
            behindEdgeSeconds: nil,
            bufferedSeconds: nil,
            attachedAt: nil,
            asOf: asOf
        )
        let sparse = LiveTvStreamInfoPanel.rows(
            programme: .none,
            channel: self.channel,
            status: nil,
            delivery: nil,
            player: unknown
        )
        XCTAssertEqual(sparse.map(\.label), ["channel", "rate"])
        XCTAssertEqual(
            sparse.last?.value,
            "unknown dropped frames · unknown stalls"
        )
        #endif
    }

    func testLiveTvOwnsThePlaybackAudioSessionForItsSeparatePlayer() async {
        let requests = LiveTvMockRequests(result: started())
        var audioEvents: [String] = []
        let controller = LiveTvPlayerController.testing(
            requests: requests,
            activateAudioSession: { audioEvents.append("activate") },
            deactivateAudioSession: { audioEvents.append("deactivate") }
        )

        await controller.stop()
        XCTAssertEqual(audioEvents, [], "an idle Live TV controller does not release another player's audio")
        await controller.watch(channel)
        XCTAssertEqual(audioEvents, ["activate"], "audio is active before live playback begins")
        await controller.stop()
        XCTAssertEqual(audioEvents, ["activate", "deactivate"])
    }

    func testSettingsWritesSeparateConfigEnableAndExactPhysicalRecovery() throws {
        func fields(_ change: LiveTvSettingsChange) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: change.body(generation: 12)) as? [String: Any])
        }
        let config = try fields(.configure(ipv4: "10.42.4.100", owner: "new-owner", sessions: 2, height: 720))
        XCTAssertNil(config["live_tv_enabled"])
        XCTAssertEqual(config["live_tv_config_generation"] as? Int, 12)
        XCTAssertEqual(config["live_tv_max_output_height"] as? Int, 0)
        let enable = try fields(.enabled(true))
        XCTAssertEqual(Set(enable.keys), ["live_tv_enabled", "live_tv_config_generation"])
        let recovery = try fields(.fencedOwner(owner: "original-owner", cutoff: 8))
        XCTAssertEqual(Set(recovery.keys), ["live_tv_fenced_owner", "live_tv_config_generation"])
        let proof = try XCTUnwrap(recovery["live_tv_fenced_owner"] as? [String: Any])
        XCTAssertEqual(proof["owner_node_id"] as? String, "original-owner")
        XCTAssertEqual(proof["drain_before_generation"] as? Int, 8)
        XCTAssertEqual(proof["stopped_and_restart_prevented"] as? Bool, true)
    }

    func testServerSettingsSnakeCaseContractDecodesWithoutUnrelatedSecrets() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let dto = try decoder.decode(LiveTvSettings.self, from: Data(#"{"live_tv_enabled":false,"live_tv_device_ipv4":"10.42.4.100","live_tv_owner_node_id":"owner","live_tv_max_sessions":2,"live_tv_output_height":720,"live_tv_config_generation":12,"live_tv_transition_from_owner_node_id":"old","live_tv_transition_drain_before":8,"tmdb_api_key":"unused-secret"}"#.utf8))
        XCTAssertEqual(dto.liveTvDeviceIpv4, "10.42.4.100")
        XCTAssertEqual(dto.liveTvConfigGeneration, 12)
        XCTAssertEqual(dto.liveTvTransitionFromOwnerNodeId, "old")
    }

    func testCapabilityPlaylistIsLocalAndNeverContainsAccountToken() throws {
        let api = LiveTvAPI(origin: "https://media.example", token: "account-secret")
        let playlist = try api.playlistURL("cap/part?query")
        XCTAssertEqual(playlist.host, "media.example")
        XCTAssertNil(playlist.query)
        XCTAssertTrue(playlist.absoluteString.contains("cap%2Fpart%3Fquery"))
        XCTAssertFalse(playlist.absoluteString.contains("account-secret"))
        XCTAssertThrowsError(try api.playlistURL(""))
    }

    // ---- the programme guide -------------------------------------------
    // tests/playback/live-tv-guide-cases.json is the same fixture the web and
    // Android suites read. Three reducers, one truth: if this file and
    // tests/web/live-tv.test.js disagree, one of the clients is lying about
    // what is on.

    /// No explicit `CodingKeys` anywhere below. The decoder applies
    /// `.convertFromSnakeCase` to the whole document — exactly as the client
    /// decodes a live response — so by the time a key reaches a `CodingKey`
    /// it is already camelCase. Spelling the snake_case original here meant
    /// every lookup was `keyNotFound` and the whole fixture decode threw, so
    /// none of the parity tests below had ever run.
    private struct GuideCases: Decodable {
        struct Expect: Decodable {
            let nowTitle: String?
            let nextTitle: String?
            let progress: Double?
        }
        struct AiringCase: Decodable {
            let name: String
            let channel: String
            let now: Int
            let expect: Expect
        }
        struct GridCell: Decodable {
            let title: String
            let left: Double
            let width: Double
            let airing: Bool
            let clipped: Bool
        }
        struct GridRow: Decodable {
            let channel: String
            let cells: [GridCell]
        }
        struct GridExpect: Decodable {
            let nowLineX: Double
            let totalWidth: Double
            let rows: [GridRow]
        }
        struct Grid: Decodable {
            let window: LiveTvGuideWindow
            let now: Int
            let slotSeconds: Int
            let pxPerSlot: Double
            let expect: GridExpect
        }
        struct FilterOptions: Decodable {
            let query: String
            let filter: String
            let hideProtected: Bool
        }
        struct FilterCase: Decodable {
            let name: String
            let opts: FilterOptions
            let expect: [String]
        }
        struct AdjacentCase: Decodable {
            let name: String
            let visible: [String]
            let current: String
            let delta: Int
            let expect: String?
        }
        let lineup: [LiveTvChannel]
        let guide: LiveTvGuide
        let programmeAt: [AiringCase]
        let grid: Grid
        let filters: [FilterCase]
        let adjacent: [AdjacentCase]
    }

    /// `.convertFromSnakeCase` on the whole document, exactly as the client
    /// decodes a live server response — which is also what proves the wire
    /// model in `LiveTvGuide.swift` matches the shape the server sends.
    private func guideCases() throws -> GuideCases {
        let url = try XCTUnwrap(
            Bundle(for: LiveTvTests.self).url(forResource: "live-tv-guide-cases", withExtension: "json")
        )
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(GuideCases.self, from: Data(contentsOf: url))
    }

    func testAiringAnswersEverySharedNowNextProgressCase() throws {
        let cases = try guideCases()
        XCTAssertFalse(cases.programmeAt.isEmpty, "the fixture lost its airing cases")
        for row in cases.programmeAt {
            let answer = LiveTvGuideReducer.airing(cases.guide, channelId: row.channel, now: row.now)
            XCTAssertEqual(answer.now?.title, row.expect.nowTitle, row.name)
            XCTAssertEqual(answer.next?.title, row.expect.nextTitle, row.name)
            if let expected = row.expect.progress {
                let actual = try XCTUnwrap(answer.progress, row.name)
                XCTAssertEqual(actual, expected, accuracy: 1e-9, row.name)
            } else {
                XCTAssertNil(answer.progress, row.name)
            }
        }
    }

    func testGridLayoutPlacesEveryCellWhereTheSharedCasesSay() throws {
        let cases = try guideCases()
        let grid = cases.grid
        let layout = LiveTvGuideReducer.gridLayout(
            guide: cases.guide, channels: cases.lineup, window: grid.window,
            now: grid.now, slotSeconds: grid.slotSeconds, pxPerSlot: grid.pxPerSlot)
        XCTAssertEqual(layout.nowX, grid.expect.nowLineX)
        XCTAssertEqual(layout.totalWidth, grid.expect.totalWidth)
        XCTAssertEqual(layout.rows.count, cases.lineup.count, "a channel never loses its row")
        for expected in grid.expect.rows {
            let row = try XCTUnwrap(layout.rows.first { $0.channel.id == expected.channel },
                                    "no row for \(expected.channel)")
            XCTAssertEqual(row.cells.count, expected.cells.count, expected.channel)
            for (cell, want) in zip(row.cells, expected.cells) {
                let where_ = "\(expected.channel)/\(want.title)"
                XCTAssertEqual(cell.programme.title, want.title, where_)
                XCTAssertEqual(cell.left, want.left, accuracy: 1e-9, where_)
                XCTAssertEqual(cell.width, want.width, accuracy: 1e-9, where_)
                XCTAssertEqual(cell.airing, want.airing, where_)
                XCTAssertEqual(cell.clipped, want.clipped, where_)
            }
            // Geometry the fixture cannot state row by row: cells advance and
            // never overlap, so the grid can never draw two programmes on top
            // of one another.
            var edge = -Double.infinity
            for cell in row.cells {
                XCTAssertGreaterThanOrEqual(cell.left, edge - 1e-9, "\(expected.channel): cells overlap")
                XCTAssertGreaterThan(cell.width, 0, "\(expected.channel): a zero-width cell")
                edge = cell.left + cell.width
            }
        }
        // A lineup channel the guide does not carry keeps its row, empty.
        let protectedRow = try XCTUnwrap(layout.rows.first { $0.channel.id == "68.1" })
        XCTAssertTrue(protectedRow.cells.isEmpty)
        XCTAssertEqual(LiveTvGuideReducer.gridSlots(window: grid.window,
                                                    slotSeconds: grid.slotSeconds).count, 4)
        XCTAssertNotNil(LiveTvGuideReducer.guideEnds(cases.guide, channelId: "11.1", window: grid.window))
        XCTAssertNil(LiveTvGuideReducer.guideEnds(cases.guide, channelId: "7.1", window: grid.window))
    }

    func testFilteringAndChannelAdjacencyAnswerTheSharedCases() throws {
        let cases = try guideCases()
        for row in cases.filters {
            let visible = LiveTvGuideReducer.filter(
                channels: cases.lineup, guide: cases.guide,
                options: LiveTvChannelFilter(query: row.opts.query,
                                             favoritesOnly: row.opts.filter == "favorites",
                                             hideProtected: row.opts.hideProtected),
                now: cases.grid.now)
            XCTAssertEqual(visible.map(\.id), row.expect, row.name)
        }
        for row in cases.adjacent {
            XCTAssertEqual(
                LiveTvGuideReducer.adjacent(visible: row.visible, current: row.current, delta: row.delta),
                row.expect, row.name)
        }
    }

    func testAGuideThatIsOffOrEmptyIsARenderedStateRatherThanAFailure() throws {
        let cases = try guideCases()
        let empty = LiveTvGuide(source: "off", freshness: "unavailable", ageSeconds: 0,
                                fetchedAt: nil,
                                window: LiveTvGuideWindow(start: 0, end: 0), refreshError: nil,
                                matchedChannels: 0, lineupChannels: 0, channels: [])
        XCTAssertFalse(empty.hasProgrammes)
        XCTAssertEqual(LiveTvGuideReducer.airing(empty, channelId: "7.1", now: cases.grid.now), .none)
        XCTAssertEqual(LiveTvGuideReducer.airing(nil, channelId: "7.1", now: 0), .none)
        // Every channel still renders, with a row and no cells, and filtering
        // still works with no guide behind it.
        let layout = LiveTvGuideReducer.gridLayout(
            guide: empty, channels: cases.lineup, window: cases.grid.window, now: cases.grid.now,
            slotSeconds: 1800, pxPerSlot: 240)
        XCTAssertEqual(layout.rows.count, cases.lineup.count)
        XCTAssertTrue(layout.rows.allSatisfy { $0.cells.isEmpty })
        XCTAssertEqual(
            LiveTvGuideReducer.filter(channels: cases.lineup, guide: empty,
                                      options: LiveTvChannelFilter(query: "wabc"), now: 0).map(\.id),
            ["7.1"])
    }

    func testGuideFocusMovesInOrderAndHandsTheTopBoundaryToTheToolbar() throws {
        func guideChannel(_ id: String) -> LiveTvChannel {
            LiveTvChannel(id: id, guideNumber: id, guideName: "Channel \(id)",
                          favorite: false, drm: false, support: "ready",
                          hd: nil, videoCodec: nil, audioCodec: nil)
        }
        func programme(_ start: Int, _ end: Int, _ title: String) -> LiveTvProgramme {
            LiveTvProgramme(start: start, end: end, title: title)
        }
        func cell(_ programme: LiveTvProgramme) -> LiveTvGridCell {
            LiveTvGridCell(programme: programme, left: Double(programme.start),
                           width: Double(programme.end - programme.start),
                           airing: false, clipped: false)
        }

        let first = programme(0, 1_800, "First")
        let second = programme(1_800, 3_600, "Second")
        let spanning = programme(0, 3_600, "Spanning")
        let layout = LiveTvGridLayout(rows: [
            LiveTvGridRow(channel: guideChannel("1"), cells: [cell(first), cell(second)]),
            LiveTvGridRow(channel: guideChannel("2"), cells: [cell(spanning)]),
            LiveTvGridRow(channel: guideChannel("3"), cells: []),
        ], totalWidth: 3_600, nowX: nil)

        var position = LiveTvGuideFocusPosition(
            channelId: "1", programmeStart: first.start,
            channelHeader: false, anchorTime: 900)
        for expected in ["2", "3"] {
            let move = LiveTvGuideFocusNavigator.move(
                layout: layout, current: position, direction: .down, fallbackAnchor: 0)
            guard case .focus(let next) = move else {
                return XCTFail("down should focus channel \(expected), got \(move)")
            }
            XCTAssertEqual(next.channelId, expected)
            XCTAssertEqual(next.anchorTime, 900, "vertical moves preserve one UTC anchor")
            position = next
        }
        XCTAssertNil(position.programmeStart, "an empty guide row remains focusable")

        position = LiveTvGuideFocusPosition(
            channelId: "1", programmeStart: first.start,
            channelHeader: false, anchorTime: 900)
        guard case .focus(let header) = LiveTvGuideFocusNavigator.move(
            layout: layout, current: position, direction: .left, fallbackAnchor: 0)
        else { return XCTFail("left from the first programme should focus its channel header") }
        XCTAssertTrue(header.channelHeader)
        guard case .focus(let restored) = LiveTvGuideFocusNavigator.move(
            layout: layout, current: header, direction: .right, fallbackAnchor: 0)
        else { return XCTFail("right from the channel header should return to its first programme") }
        XCTAssertEqual(restored.programmeStart, first.start)

        XCTAssertEqual(
            LiveTvGuideFocusNavigator.move(
                layout: layout, current: restored, direction: .up, fallbackAnchor: 0),
            .toolbar,
            "the grid must relinquish focus before the toolbar claims it"
        )
    }

    func testFocusCoordinatorRejectsStaleRestoresAndTransfersTheBoundaryAtomically() throws {
        func guideChannel(_ id: String) -> LiveTvChannel {
            LiveTvChannel(id: id, guideNumber: id, guideName: "Channel \(id)",
                          favorite: false, drm: false, support: "ready",
                          hd: nil, videoCodec: nil, audioCodec: nil)
        }
        let programme = LiveTvProgramme(start: 0, end: 1_800, title: "First")
        let layout = LiveTvGridLayout(rows: [
            LiveTvGridRow(
                channel: guideChannel("1"),
                cells: [LiveTvGridCell(programme: programme, left: 0, width: 1_800,
                                       airing: false, clipped: false)]
            ),
        ], totalWidth: 1_800, nowX: nil)
        let position = LiveTvGuideFocusPosition(
            channelId: "1", programmeStart: 0, channelHeader: false, anchorTime: 900)

        var guide = LiveTvGuideFocusCoordinator()
        let entry = try XCTUnwrap(guide.beginRestore(request: 1, ownerRequested: true))
        XCTAssertTrue(guide.permits(entry, ownerRequested: true))

        // Applying focus changes the revision. A task that captured the entry
        // ticket before a newer focus event can no longer write FocusState.
        guide.focusChanged(active: true)
        XCTAssertFalse(guide.permits(entry, ownerRequested: true))
        let refresh = try XCTUnwrap(guide.beginRestore(request: 1, ownerRequested: true))

        // Up from the first row is one ordered adapter transition: clear the
        // grid owner, then hand focus to the toolbar. It also invalidates a
        // refresh already queued against the old focus intent.
        XCTAssertEqual(
            guide.move(layout: layout, current: position, direction: .up, fallbackAnchor: 0),
            [.clearGrid, .focusToolbar]
        )
        XCTAssertFalse(guide.permits(refresh, ownerRequested: true))
        XCTAssertNil(guide.beginRestore(request: 1, ownerRequested: true),
                     "a data refresh cannot reclaim an abandoned request")
        XCTAssertNotNil(guide.beginRestore(request: 2, ownerRequested: true),
                        "only a newer explicit grid-entry request may restore focus")

        // On now uses the same ticket fence around its Task.yield restoration.
        var channels = LiveTvFocusRestoreCoordinator()
        let channelRestore = try XCTUnwrap(channels.beginRestore(request: 1, ownerRequested: true))
        channels.leave()
        XCTAssertFalse(channels.permits(channelRestore, ownerRequested: true))
        XCTAssertNil(channels.beginRestore(request: 1, ownerRequested: true))
        XCTAssertNotNil(channels.beginRestore(request: 2, ownerRequested: true))
    }

    func testGuideWireShapeDecodesWithSnakeCaseAndIgnoresWhatItShouldNotSee() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let guide = try decoder.decode(LiveTvGuide.self, from: Data(#"""
        {"source":"hdhomerun","freshness":"fresh","age_seconds":412,"fetched_at":1789000000,
         "window":{"start":1788996400,"end":1789086400},"refresh_error":null,
         "matched_channels":12,"lineup_channels":12,"device_auth":"MUST-NOT-BE-READ",
         "channels":[{"id":"7.1","guide_number":"7.1","affiliate":"ABC","image_url":"https://x/i.png",
           "programmes":[{"start":1789000800,"end":1789002600,"title":"City Beat",
             "episode_title":"Pier 40","episode":"S3E14","synopsis":"…",
             "original_air_date":"2026-09-07","filters":["News"]}]}]}
        """#.utf8))
        XCTAssertEqual(guide.matchedChannels, 12)
        XCTAssertEqual(guide.channels.first?.affiliate, "ABC")
        XCTAssertEqual(guide.channels.first?.programmes.first?.episodeTitle, "Pier 40")
        XCTAssertEqual(guide.channels.first?.programmes.first?.originalAirDate, "2026-09-07")
        XCTAssertTrue(guide.hasProgrammes)
    }

    // ---- the live input table ------------------------------------------

    private struct LiveContractFixture: Decodable {
        struct Live: Decodable {
            struct Timings: Decodable {
                let hideAfterMs: Int
                let channelCoalesceMs: Int
                let guidePollUnavailableS: Int
                let guidePollMinS: Int
                let guidePollAfterNextRefreshS: Int
                let retireLivenessProbeMs: Int
                let retireOrphanAfterKeepalives: Int
                let startReplayAttempts: Int
                let guidePollCeilingS: Int
                enum CodingKeys: String, CodingKey {
                    case hideAfterMs = "hide_after_ms"
                    case channelCoalesceMs = "channel_coalesce_ms"
                    case guidePollUnavailableS = "guide_poll_unavailable_s"
                    case guidePollMinS = "guide_poll_min_s"
                    case guidePollAfterNextRefreshS = "guide_poll_after_next_refresh_s"
                    case retireLivenessProbeMs = "retire_liveness_probe_ms"
                    case retireOrphanAfterKeepalives = "retire_orphan_after_keepalives"
                    case startReplayAttempts = "start_replay_attempts"
                    case guidePollCeilingS = "guide_poll_ceiling_s"
                }
            }
            // Spelled out rather than decoded with `.convertFromSnakeCase`:
            // that strategy rewrites dictionary KEYS too, so the routing
            // table's inputs would arrive as `playPause`/`tapSurface` and stop
            // matching the contract's raw values — which is the whole point.
            let routing: [String: [String: [String: String]]]
            let inputs: [String]
            let timings: Timings
        }
        let live: Live
    }

    func testLiveInputRoutingMatchesTheSharedContract() throws {
        let url = try XCTUnwrap(
            Bundle(for: LiveTvTests.self).url(forResource: "player-input-contract", withExtension: "json")
        )
        let fixture = try JSONDecoder().decode(LiveContractFixture.self, from: Data(contentsOf: url))
        XCTAssertEqual(Set(fixture.live.inputs),
                       Set(LiveTvContractInput.allCases.map(\.rawValue)),
                       "the fixture's live input names are the enum's raw values")
        XCTAssertEqual(Set(fixture.live.routing.keys),
                       Set(LiveTvInputSurface.allCases.map(\.rawValue)))
        for (surfaceName, states) in fixture.live.routing {
            let surface = try XCTUnwrap(LiveTvInputSurface(rawValue: surfaceName))
            XCTAssertEqual(Set(states.keys), Set(LiveTvInputState.allCases.map(\.rawValue)), surfaceName)
            for (stateName, row) in states {
                let state = try XCTUnwrap(LiveTvInputState(rawValue: stateName))
                for (inputName, expected) in row {
                    let input = try XCTUnwrap(LiveTvContractInput(rawValue: inputName))
                    XCTAssertEqual(
                        LiveTvInputRouting.route(surface: surface, state: state, input: input).rawValue,
                        expected, "\(surfaceName)/\(stateName)/\(inputName)")
                }
            }
        }
        XCTAssertEqual(LiveTvInputRouting.overlayAutoHideNanoseconds,
                       UInt64(fixture.live.timings.hideAfterMs) * 1_000_000)
        XCTAssertEqual(LiveTvInputRouting.channelCoalesceMilliseconds,
                       fixture.live.timings.channelCoalesceMs)
        // The six start/guide timings reach Swift by exactly the route
        // `channel_coalesce_ms` does, and are pinned here by the same test.
        XCTAssertEqual(LiveTvInputRouting.guidePollUnavailableSeconds,
                       fixture.live.timings.guidePollUnavailableS)
        XCTAssertEqual(LiveTvInputRouting.guidePollMinSeconds,
                       fixture.live.timings.guidePollMinS)
        XCTAssertEqual(LiveTvInputRouting.guidePollAfterNextRefreshSeconds,
                       fixture.live.timings.guidePollAfterNextRefreshS)
        XCTAssertEqual(LiveTvInputRouting.retireLivenessProbeMilliseconds,
                       fixture.live.timings.retireLivenessProbeMs)
        XCTAssertEqual(LiveTvInputRouting.retireOrphanAfterKeepalives,
                       fixture.live.timings.retireOrphanAfterKeepalives)
        XCTAssertEqual(LiveTvInputRouting.startReplayAttempts,
                       fixture.live.timings.startReplayAttempts)
        // The far end of the guide-poll clamp is a contract number like the
        // rest: a missing `guide_poll_ceiling_s` now fails the decode rather
        // than passing silently, which is the whole point of pinning it.
        XCTAssertEqual(LiveTvInputRouting.guidePollCeilingSeconds,
                       fixture.live.timings.guidePollCeilingS)
    }

    // ---- the two narrowings, pinned in the source they live in ----------

    func testTheLiveSurfaceAllowsPictureInPictureAndReleasesOnlyWhenItIsNotActive() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        // The mutually exclusive iOS inline, tvOS browse-picture, and
        // fullscreen sites all hand the element to AVKit.
        XCTAssertEqual(source.components(separatedBy: "allowsPictureInPicture: true").count - 1, 3,
                       "every live picture surface must allow picture-in-picture")
        XCTAssertFalse(source.contains("allowsPictureInPicture: false"))
        // Entering PiP backgrounds the app. Stopping on that would kill the one
        // case PiP exists for, so every release path consults it — and consults
        // `isStarting` too, because automatic PiP has not set `isActive` yet at
        // the `.inactive` edge where the old rule fired.
        XCTAssertTrue(source.contains(
            "private var mayRelease: Bool { !pictureInPicture.isActive && !pictureInPicture.isStarting }"))
        XCTAssertTrue(source.contains("if phase == .background && mayRelease"))
        XCTAssertTrue(source.contains("if !fullscreen && mayRelease"))
        // And the reverse: PiP ending while this screen is gone is the only
        // moment left that can give the tuner back.
        XCTAssertTrue(source.contains("if !active && !onScreen && live.playing { Task { await live.stop() } }"))
        // Only the inline surface is built while the cover is up, so the two
        // never fight over one PictureInPictureController.
        XCTAssertTrue(source.contains("if live.playing && !fullscreen {"))
        // Exit leaves the presentation. It does not release the lease.
        XCTAssertFalse(source.contains("onDismiss: { if mayRelease"))
    }

    /// The cover's reveal layer takes every direction through
    /// `onMoveCommand`, so while it holds focus the engine never sees one.
    /// `focusable(false)` does not relocate focus until the next focus
    /// update, and the adapter stays attached meanwhile — so the layer can
    /// still hold focus for a moment after the overlay is back.
    ///
    /// It used to assert `.fullscreenHidden` through that moment, and
    /// `fullscreenHidden` answers every direction *and* Select with `reveal`.
    /// So the moment never ended: each press re-showed an overlay that was
    /// already up, focus never left a transparent view, nothing highlighted
    /// and nothing activated. Reporting the real state routes those presses
    /// to `focusControl`, which `applyLiveOutcome` refuses so the framework
    /// moves focus off.
    ///
    /// Select is asserted too, because the layer dispatches it through
    /// `onTapGesture`; a directions-only test would miss a table that gave
    /// `fullscreenHidden × select` a real outcome.
    func testTheRevealLayerReportsTheStateItIsInRatherThanTheOneItWants() throws {
        let presses: [LiveTvContractInput] = [.left, .right, .up, .down, .select]
        for state in LiveTvInputState.allCases {
            let everyPressReveals = presses.allSatisfy {
                LiveTvInputRouting.route(surface: .tenFoot, state: state, input: $0) == .reveal
            }
            XCTAssertEqual(
                everyPressReveals,
                state == .fullscreenHidden,
                "\(state.rawValue): only the state with nothing else on screen may eat every press"
            )
        }
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertFalse(
            source.contains("state: { .fullscreenHidden }"),
            "the reveal adapter must report liveInputState, never assert a state it cannot know"
        )
        XCTAssertEqual(
            source.components(separatedBy: "detail == nil, live.playing, !live.paused").count - 1,
            2,
            """
            `detail` belongs in BOTH auto-hide guards, entry and post-sleep. \
            In one only, a programme sheet opened during those four seconds \
            still lets the chrome hide underneath it
            """
        )
        // Both directions of the overlay change defer past the update that
        // inserts the view they then want focused, because tvOS drops an
        // assignment aimed at a view that does not exist yet. Undeferred, the
        // reveal assignment was dropped and focus stayed on the layer - and
        // having not changed, it never tripped the bounce in
        // `onChange(of: focusedControl)`. The hide direction already deferred;
        // these are its two guards, one per direction.
        XCTAssertTrue(
            source.contains("guard fullscreen, overlayVisible else { return }"),
            "revealing the overlay must wait for its buttons before naming one"
        )
        XCTAssertTrue(
            source.contains("guard fullscreen, !overlayVisible, !temporaryGuide,"),
            "hiding it must wait for the reveal layer before naming that"
        )
    }

    func testTheTenFootSurfaceKeepsSomethingFocusableWhileTheOverlayIsHidden() throws {
        // tvOS delivers move/exit/playPause only to the focused view and its
        // ancestors, and PlayerSurfaceView refuses focus — so once the overlay
        // auto-hid, the cover held nothing focusable and every `hidden →
        // reveal` row of the contract became unreachable.
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertTrue(source.contains(".focused($focusedControl, equals: FocusTarget.reveal)"))
        // Was `focusedControl = visible ? .guide : .reveal`, which the
        // rebuilt surface split into the two branches of
        // `onChange(of: overlayVisible)` and retargeted to `.play`. This
        // assertion had been failing on main ever since: the fast lane
        // compiles the Apple target and does not run it.
        XCTAssertTrue(source.contains(".focusable(!overlayVisible && !temporaryGuide)"))
        XCTAssertTrue(source.contains("focusedControl = .reveal"))
        // Tuning stays inside the selected browse layout. The viewer chooses
        // fullscreen explicitly with Return to live or by selecting the
        // already-playing channel.
        XCTAssertFalse(source.contains(".onChange(of: live.playing) { _, playing in if playing { fullscreen = true } }"))
        // Search must be explicit on a television. Applying `.searchable` to
        // its List focused the field at entry and covered half the page with a
        // keyboard before the viewer asked for one.
        XCTAssertTrue(source.contains(".sheet(isPresented: $showingSearch)"))
        // tvOS 26's default tint can make a label and its capsule the same
        // accent colour. Page actions and rows own their focus contrast.
        XCTAssertTrue(source.contains(".buttonStyle(TVReadableButtonStyle(prominent: favoritesOnly, compact: true))"))
        XCTAssertTrue(source.contains(".buttonStyle(LiveTvChannelButtonStyle())"))
    }

    func testAppleTvLiveNavigationUsesScrollingHeadersAndReadablePanels() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)

        // A tvOS `List` row stretches to its container and fights a fixed
        // column, so the 620 pt On now list is a LazyVStack that keeps the
        // row width — and keeps the focus binding the restore coordinator
        // keys on.
        XCTAssertTrue(source.contains("LazyVStack(spacing: 4)"),
                      "the television list owns its own width")
        XCTAssertTrue(source.contains(".focused($focusedChannelId, equals: channel.id)"),
                      "the restore coordinator keys on the row's focus binding")
        XCTAssertTrue(source.contains("private struct LiveTvGuideButtonStyle: ButtonStyle"),
                      "the selected programme needs visible focus chrome")
        XCTAssertTrue(source.contains(".offset(x: -scrollOrigin.x)"),
                      "channel headers should pin only horizontally while rows scroll vertically")

        let gridStart = try XCTUnwrap(source.range(of: "struct LiveTvGuideGrid: View")?.lowerBound)
        let scrollEnd = try XCTUnwrap(source.range(
            of: ".coordinateSpace(name: \"live-tv-guide-scroll\")",
            range: gridStart..<source.endIndex
        )?.lowerBound)
        let scrollingContent = String(source[gridStart..<scrollEnd])
        XCTAssertTrue(scrollingContent.contains("channelHeader: true"),
                      "focusable channel headers belong to the vertically scrolling rows")

        func section(_ start: String, _ end: String) throws -> String {
            let lower = try XCTUnwrap(source.range(of: start)?.lowerBound)
            let upper = try XCTUnwrap(source.range(of: end, range: lower..<source.endIndex)?.lowerBound)
            return String(source[lower..<upper])
        }
        let more = try section("private var morePanel", "private var liveToolbar")
        XCTAssertEqual(more.components(separatedBy: "Button(").count - 1, 8)
        XCTAssertEqual(more.components(separatedBy: ".buttonStyle(TVReadableButtonStyle").count - 1, 8,
                       "every More action owns a readable foreground/background pair")
        let layout = try section("private var layoutPanel", "private var morePanel")
        XCTAssertEqual(layout.components(separatedBy: ".buttonStyle(TVReadableButtonStyle").count - 1, 2,
                       "the layout choice and Close action must not inherit the red tint")
    }

    func testTuningFromTheOverlayDoesNotDismissTheSurfaceMidStart() throws {
        // `watch()` detaches before it awaits, so `playing` goes false in the
        // middle of a tune. Closing the cover on that ran `onDismiss`, stopped
        // the session still being granted, and left nothing playing — which on
        // tvOS was the only tune path there was.
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertFalse(source.contains(".onChange(of: live.playing) { _, playing in if !playing { fullscreen = false } }"))
        XCTAssertTrue(source.contains("if !busy && !live.playing { fullscreen = false }"))
    }

    func testTheGuideRefreshAndHeartbeatOutliveTheViewThatStartedThem() throws {
        // Both are Tasks on the shared controller rather than `.task {}` on a
        // view, so SwiftUI cannot cancel them when the view leaves the
        // hierarchy — which is what keeps a lease alive in picture-in-picture.
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertTrue(source.contains("private var guideRefresh: Task<Void, Never>?"))
        XCTAssertTrue(source.contains("guideRefresh = Task { @MainActor [weak self] in"))
        XCTAssertTrue(source.contains("heartbeat = Task { @MainActor [weak self] in"))
        // The loop reschedules from the answered guide rather than from a
        // constant of its own; the ceiling is the only fixed number left, and
        // it is a clamp, not a cadence.
        XCTAssertFalse(source.contains("guideRefreshNanoseconds"),
                       "a fixed twenty-minute cadence cannot see an owner that came back in one")
        XCTAssertTrue(source.contains("seconds = Self.guidePollSeconds("))
        // A failed read is paced like an unavailable document that says
        // nothing about coming back — the same information, the same wait.
        XCTAssertTrue(
            source.contains("var seconds = LiveTvInputRouting.guidePollUnavailableSeconds"),
            "a failed guide read must not poll a struggling owner at the floor")
        // The poll ceiling is a contract number transcribed beside its six
        // siblings, not one invented in the view. (The `20 * 60` still in this
        // file is the unrelated source-format TTL.)
        XCTAssertFalse(source.contains("static let guidePollCeilingSeconds"),
                       "the ceiling must not be redefined here")
        XCTAssertTrue(source.contains("let ceiling = LiveTvInputRouting.guidePollCeilingSeconds"))
    }

    /// The poll lands where the owner said it should, floored, ceilinged, and
    /// never computed from a cadence this client invented.
    func testTheGuidePollFollowsTheOwnersClockWithinItsFloorAndCeiling() {
        func poll(_ next: Int?, _ freshness: String = "fresh", now: Int = 1_700_000_000) -> Int {
            LiveTvPlayerController.guidePollSeconds(nextRefreshAt: next, freshness: freshness, now: now)
        }
        let now = 1_700_000_000
        // next_refresh_at + guide_poll_after_next_refresh_s, when that is past
        // the floor.
        XCTAssertEqual(poll(now + 100), 105)
        // ... and the floor when it is not — including a next_refresh_at that
        // has already passed, which is what a just-restarted owner publishes.
        XCTAssertEqual(poll(now + 1), LiveTvInputRouting.guidePollMinSeconds)
        XCTAssertEqual(poll(now - 3600), LiveTvInputRouting.guidePollMinSeconds)
        // No next_refresh_at at all: unavailable asks sooner than the owner's
        // cadence, everything else takes the floor.
        XCTAssertEqual(poll(nil, "unavailable"), LiveTvInputRouting.guidePollUnavailableSeconds)
        XCTAssertEqual(poll(nil, "stale"), LiveTvInputRouting.guidePollMinSeconds)
        XCTAssertEqual(poll(nil, "fresh"), LiveTvInputRouting.guidePollMinSeconds)
        // The owner's clock wins whenever it exists: an unavailable guide that
        // DOES say when it comes back is polled on that, not on the flat
        // unavailable cadence. A recorded deviation from the plan's §3.16,
        // shared with the web and Android reducers.
        XCTAssertEqual(poll(now + 100, "unavailable"), 105)
        XCTAssertEqual(poll(now + 1, "unavailable"), LiveTvInputRouting.guidePollMinSeconds,
                       "the floor still applies to an unavailable document")
        // Nonsense off the wire can neither spin the loop nor park it.
        XCTAssertEqual(poll(Int.max), LiveTvInputRouting.guidePollCeilingSeconds)
        XCTAssertEqual(poll(Int.min), LiveTvInputRouting.guidePollMinSeconds)
        XCTAssertEqual(poll(now + 86_400), LiveTvInputRouting.guidePollCeilingSeconds)
        // The clamp guards the wire, not the contract: it is applied to the
        // `next_refresh_at` branch only. The two constants come back verbatim,
        // because a client that silently rewrote them would disagree with the
        // value the shared fixture pins.
        XCTAssertEqual(poll(nil, "unavailable"), LiveTvInputRouting.guidePollUnavailableSeconds)
        XCTAssertEqual(poll(nil, "fresh"), LiveTvInputRouting.guidePollMinSeconds)
        // Whatever comes off the wire, the answer lands inside the contract's
        // own bounds — and never under the floor, which is the direction that
        // would poll a struggling owner harder than the contract allows.
        for next in [nil, Int.min, Int.min + 1, -1, 0, now - 86_400, now - 1, now, now + 14,
                     now + 15, now + 1_195, now + 1_200, now + 86_400, Int.max - 1, Int.max] {
            for freshness in ["fresh", "stale", "unavailable"] {
                let seconds = poll(next, freshness)
                let where_ = "\(String(describing: next))/\(freshness)"
                XCTAssertGreaterThanOrEqual(seconds, LiveTvInputRouting.guidePollMinSeconds, where_)
                XCTAssertLessThanOrEqual(seconds, LiveTvInputRouting.guidePollCeilingSeconds, where_)
            }
        }
    }

    /// The floor is applied last on purpose. `min(max(gap, floor), ceiling)`
    /// looks equivalent and is not: if the two contract numbers ever crossed it
    /// would return a delay *below* `guide_poll_min_s`. Identical for every
    /// value the contract actually carries, which is why only the source says
    /// so — and why it would be swapped back by someone tidying up.
    func testTheGuidePollAppliesItsFloorLastSoTheFloorWins() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        XCTAssertTrue(
            source.contains("return max(min(gap + LiveTvInputRouting.guidePollAfterNextRefreshSeconds, ceiling), floor)"),
            "the floor must be the outermost clamp")
        XCTAssertFalse(source.contains("floor), ceiling)"),
                       "clamping the ceiling last lets a crossed pair poll below the floor")
        XCTAssertLessThan(LiveTvInputRouting.guidePollMinSeconds,
                          LiveTvInputRouting.guidePollCeilingSeconds,
                          "the contract's own pair must not be crossed")
    }

    func testTheGuideDocumentCarriesTheOwnersNextRefreshTime() throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let withIt = try decoder.decode(LiveTvGuide.self, from: Data(#"""
        {"source":"owner","freshness":"stale","age_seconds":90,"fetched_at":1700000000,
         "next_refresh_at":1700000060,"window":{"start":0,"end":0},"channels":[]}
        """#.utf8))
        XCTAssertEqual(withIt.nextRefreshAt, 1_700_000_060)
        // Absent from an owner whose loop has not completed a tick, and from
        // one older than this contract: a rendered state, not a decode failure.
        let without = try decoder.decode(LiveTvGuide.self, from: Data(#"""
        {"source":"owner","freshness":"unavailable","age_seconds":0,
         "window":{"start":0,"end":0},"channels":[]}
        """#.utf8))
        XCTAssertNil(without.nextRefreshAt)
        // And the shared guide fixture, which predates the field, still loads.
        XCTAssertNil(try guideCases().guide.nextRefreshAt)
    }

    func testAHeldChannelKeyIsOneTunerStart() async throws {
        // The coalescing window is the guardrail against a channel-surf storm.
        // Superseding requests must cancel, not queue. Asserting the constant
        // alone proved nothing — it never checked that three requests produce
        // one start — so this counts the starts a stub lease actually sees.
        let requests = LiveTvCountingRequests()
        let controller = LiveTvPlayerController.testing(requests: requests)
        controller.requestChannel(channel)
        controller.requestChannel(channel)
        controller.requestChannel(channel)
        try await Task.sleep(nanoseconds: UInt64(LiveTvInputRouting.channelCoalesceMilliseconds + 250) * 1_000_000)
        XCTAssertEqual(requests.starts, 1, "three presses inside the window are one tuner start")
        XCTAssertEqual(LiveTvInputRouting.channelCoalesceMilliseconds, 350)
    }

    /// A hard-coded 300 pt slot filled 58% of a 1920 pt screen and could never
    /// show two hours. The television's slot width follows the width the grid
    /// actually got; the phone's fixed columns do not move.
    func testGuideSlotWidthFollowsTheScreenOnTelevisionAndNotOnThePhone() throws {
        #if os(tvOS)
        XCTAssertEqual(LiveTvGridMetrics.pxPerSlot(contentWidth: 1776), 390, accuracy: 0.001)
        XCTAssertEqual(LiveTvGridMetrics.channelColumnWidth, 200)
        XCTAssertEqual(LiveTvGridMetrics.rowHeight, 74)
        XCTAssertEqual(LiveTvGridMetrics.visibleSlots, 4)
        let dimensions = LiveTvGridMetrics.dimensions(contentWidth: 1776)
        XCTAssertEqual(dimensions.slotWidth, 390, accuracy: 0.001)
        // The grid pads its own content by 8 pt on each side, so what the
        // slots and the channel column share is the content width less that
        // inset. Getting this wrong is how the fourth half-hour ends up 16 pt
        // off the right edge with a horizontal scroll nobody asked for.
        XCTAssertEqual(
            LiveTvGridMetrics.horizontalInset
                + dimensions.channelColumnWidth
                + dimensions.slotWidth * Double(LiveTvGridMetrics.visibleSlots),
            1776, accuracy: 0.001,
            "the inset, the channel column and the visible slots must be exactly the content width")
        // Four half-hour slots is the two-hour page the paging chips move by.
        let window = LiveTvGridMetrics.window(start: 1_700_000_000)
        XCTAssertEqual(window.end - window.start, 4 * LiveTvGridMetrics.slotSeconds)
        #else
        XCTAssertEqual(LiveTvGridMetrics.pxPerSlot(contentWidth: 1776), 160, accuracy: 0.001)
        XCTAssertEqual(LiveTvGridMetrics.pxPerSlot(contentWidth: 390), 160, accuracy: 0.001)
        XCTAssertEqual(LiveTvGridMetrics.rowHeight, 56)
        XCTAssertEqual(LiveTvGridMetrics.channelColumnWidth, 128)
        XCTAssertEqual(LiveTvGridMetrics.visibleSlots, 8)
        #endif
        // Six hours still covers a two-hour page, the server's hour of
        // backfill and the partial slot.
        XCTAssertEqual(LiveTvGridMetrics.requestedHours, 6)
    }

    /// Two entries, three stored values. An existing `channel_browser`
    /// preference must keep decoding and must keep its raw value.
    func testTheChannelBrowserLayoutIsPresentedAsPreviewWithoutRewritingIt() throws {
        let stored = try XCTUnwrap(TvLiveLayout(rawValue: "channel_browser"))
        XCTAssertEqual(stored.presented, .guidePreview)
        XCTAssertEqual(TvLiveLayout.guidePreview.presented, .guidePreview)
        XCTAssertEqual(TvLiveLayout.guideOverlay.presented, .guideOverlay)
        XCTAssertEqual(TvLiveLayout.offered, [.guidePreview, .guideOverlay])
        XCTAssertEqual(TvLiveLayout.offered.map(\.label), ["Preview", "Over picture"])
        XCTAssertEqual(TvLiveLayout.allCases.count, 3, "the third case still decodes")
        // Nothing writes the aliased value back. The sheet only ever assigns a
        // member of `offered`, so a stored `channel_browser` survives until the
        // viewer deliberately picks a layout.
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "live-tv-layout-alias"))
        defaults.removePersistentDomain(forName: "live-tv-layout-alias")
        defaults.set("channel_browser", forKey: "plurx.liveTvLayout")
        let read = TvLiveLayout(rawValue: defaults.string(forKey: "plurx.liveTvLayout") ?? "")
        XCTAssertEqual(read?.presented, .guidePreview)
        XCTAssertEqual(defaults.string(forKey: "plurx.liveTvLayout"), "channel_browser")
        defaults.removePersistentDomain(forName: "live-tv-layout-alias")
    }

    /// tvOS resolves the semantic text styles two to two and a half times
    /// larger than iOS does — `.subheadline` is 38 pt there, `.title2` 57 —
    /// which is what made the channel rows three feet long. Live TV sizes
    /// itself through `LiveTvType` instead.
    func testLiveTvTextGoesThroughItsOwnScaleRatherThanSemanticStyles() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        // Touch-only branches use semantic styles for Dynamic Type. Apply
        // this existing ten-foot guard to the source active on tvOS only.
        var branchConditions: [Bool] = []
        var televisionLines: [String] = []
        for line in source.components(separatedBy: "\n") {
            let directive = line.trimmingCharacters(in: .whitespaces)
            if directive == "#if os(tvOS)" {
                branchConditions.append(true)
            } else if directive == "#if os(iOS)" {
                branchConditions.append(false)
            } else if directive == "#else", !branchConditions.isEmpty {
                branchConditions[branchConditions.count - 1].toggle()
            } else if directive == "#endif", !branchConditions.isEmpty {
                branchConditions.removeLast()
            } else if branchConditions.allSatisfy({ $0 }) {
                televisionLines.append(line)
            }
        }
        let televisionSource = televisionLines.joined(separator: "\n")
        for style in [".font(.subheadline", ".font(.title2", ".font(.title3",
                      ".font(.callout", ".font(.headline"] {
            XCTAssertFalse(televisionSource.contains(style),
                           "\(style) inflates on tvOS — use LiveTvType")
        }
        XCTAssertTrue(source.contains("enum LiveTvType"))
        XCTAssertTrue(source.contains(".font(LiveTvType."))
        // The toolbar is one row of compact buttons; sheets keep the tall pair.
        XCTAssertTrue(source.contains("TVReadableButtonStyle(prominent: false, compact: true)"))
    }

    /// The arrangement itself, not just the constants behind it: each of these
    /// would still be true if the numbers changed, and each is false if the
    /// rewrite is reverted.
    func testTheLiveTvArrangementIsWiredToTheDerivedGeometry() throws {
        let testsDirectory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        let source = try String(
            contentsOf: testsDirectory.appendingPathComponent("../Sources/LiveTvView.swift").standardizedFileURL,
            encoding: .utf8)
        // The grid draws from the dimensions it is handed, never from the
        // statics — that is what makes the slot width follow the screen.
        let gridStart = try XCTUnwrap(source.range(of: "struct LiveTvGuideGrid: View")?.lowerBound)
        let gridEnd = try XCTUnwrap(source.range(of: "struct LiveTvView: View")?.lowerBound)
        let grid = String(source[gridStart..<gridEnd])
        XCTAssertTrue(grid.contains("dimensions.slotWidth"))
        XCTAssertTrue(grid.contains("dimensions.rowHeight"))
        XCTAssertTrue(grid.contains("dimensions.channelColumnWidth"))
        XCTAssertFalse(grid.contains("LiveTvGridMetrics.pxPerSlot"),
                       "the grid must not reach past its own dimensions")
        XCTAssertFalse(grid.contains("LiveTvGridMetrics.rowHeight"))
        // The list owns its width, the picture is a focus target, and the
        // fixed grid frame that pushed rows off the bottom is gone.
        XCTAssertTrue(source.contains("static let tvListColumnWidth: CGFloat = 620"))
        XCTAssertTrue(source.contains("Button { fullscreen = true } label: {"))
        XCTAssertFalse(source.contains("LiveTvGridMetrics.rowHeight + 54"))
        // One toolbar, one status line, and none of the bands they replaced.
        XCTAssertTrue(source.contains("private var liveToolbar: some View"))
        XCTAssertTrue(source.contains("accessibilityIdentifier(\"live-tv-status\")"))
        XCTAssertFalse(source.contains("private var nowBar"))
        XCTAssertFalse(source.contains("private var actionBar"))
        XCTAssertFalse(source.contains("private var filterBar"))
        // Every focusable inside the guide's remote adapter owns a focus key,
        // or a direction press on it is swallowed with nowhere to go.
        XCTAssertTrue(source.contains("private func movePagingFocus"))
        XCTAssertTrue(source.contains("channelHeader: false, paging: index"))
    }

    /// A server-shaped recording row, built as JSON rather than through the
    /// memberwise initialiser so the test also proves the wire names map.
    private func dvrRecordingJSON(
        channel: String, start: Int, state: String, ruleId: String? = nil,
        captureStart: Int? = nil, captureEnd: Int? = nil
    ) -> [String: Any] {
        var row: [String: Any] = [
            "id": "\(channel)@\(start)",
            "origin": ruleId == nil ? "manual" : "rule",
            "rule_id": NSNull(),
            "requested_by_user_id": 1,
            "channel_id": channel,
            "guide_number": channel,
            "channel_name": "WTEST",
            "airing_start": start,
            "airing_end": start + 1_800,
            "capture_start": captureStart ?? start - 60,
            "capture_end": captureEnd ?? start + 1_920,
            "title": "Kitchen Table",
            "episode_title": NSNull(),
            "episode": NSNull(),
            "synopsis": NSNull(),
            "image_url": NSNull(),
            "original_air_date": NSNull(),
            "series_id": NSNull(),
            "programme_id": NSNull(),
            "state": state,
            "state_reason": NSNull(),
            "attempt": 0,
            "gap_s": 0,
            "late_start_s": 0,
            "tuner_owner_node_id": NSNull(),
            "path": NSNull(),
            "bytes": 0,
            "last_progress_ms": NSNull(),
            "stop_requested_at_ms": NSNull(),
            "stop_requested_by_user_id": NSNull(),
            "item_id": NSNull(),
            "file_id": NSNull(),
            "started_at_ms": NSNull(),
            "finished_at_ms": NSNull(),
            "stopped_by_user_id": NSNull(),
            "created_at_ms": 0,
            "updated_at_ms": 0,
        ]
        if let ruleId { row["rule_id"] = ruleId }
        return row
    }

    private func dvrReminderJSON(
        id: String, channel: String, start: Int, state: String, leadS: Int = 300
    ) -> [String: Any] {
        [
            "id": id,
            "user_id": 1,
            "channel_id": channel,
            "guide_number": channel,
            "airing_start": start,
            "airing_end": start + 1_800,
            "title": "Kitchen Table",
            "lead_s": leadS,
            "state": state,
            "fired_at_ms": NSNull(),
            "acked_at_ms": NSNull(),
            "created_at_ms": 0,
            "updated_at_ms": 0,
            "covered_by_recording": false,
        ]
    }

    private func dvrDecoder() -> JSONDecoder {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }

    func testGuideMarksRankARecordingAboveAReminderAndIgnoreASkippedAiring() throws {
        let decoder = dvrDecoder()
        let schedule = try decoder.decode([DvrRecording].self, from: JSONSerialization.data(
            withJSONObject: [
                dvrRecordingJSON(channel: "7.1", start: 1_000, state: "scheduled"),
                dvrRecordingJSON(channel: "7.1", start: 3_000, state: "scheduled", ruleId: "rule-1"),
                dvrRecordingJSON(channel: "5.1", start: 1_000, state: "conflict"),
                dvrRecordingJSON(channel: "5.1", start: 3_000, state: "withdrawn"),
                dvrRecordingJSON(channel: "9.1", start: 1_000, state: "cancelled"),
                dvrRecordingJSON(channel: "9.1", start: 3_000, state: "recording",
                                 captureStart: 2_940, captureEnd: 4_920),
            ]))
        let reminders = try decoder.decode([DvrReminder].self, from: JSONSerialization.data(
            withJSONObject: [
                dvrReminderJSON(id: "r1", channel: "3.1", start: 5_000, state: "armed"),
                // The same airing as the running capture.
                dvrReminderJSON(id: "r2", channel: "9.1", start: 3_000, state: "armed"),
            ]))

        let marks = DvrMarks(schedule: schedule, reminders: reminders)
        XCTAssertEqual(marks.mark(channelId: "7.1", airingStart: 1_000), .scheduled(series: false))
        XCTAssertEqual(marks.mark(channelId: "7.1", airingStart: 3_000), .scheduled(series: true),
                       "a rule row is two dots, not one")
        XCTAssertEqual(marks.mark(channelId: "5.1", airingStart: 1_000), .conflict)
        XCTAssertEqual(marks.mark(channelId: "5.1", airingStart: 3_000), .lapsed)
        XCTAssertNil(marks.mark(channelId: "9.1", airingStart: 1_000),
                     "a skipped airing is a decision already taken, not a mark")
        XCTAssertEqual(marks.mark(channelId: "9.1", airingStart: 3_000),
                       .recording(captureStart: 2_940, captureEnd: 4_920),
                       "a recording is the stronger promise, so it outranks the reminder")
        XCTAssertEqual(marks.mark(channelId: "3.1", airingStart: 5_000), .reminder)
        XCTAssertNil(marks.mark(channelId: "3.1", airingStart: 1_000))
    }

    #if os(iOS)
    /// The mirror is a reconciliation. A reminder that moved loses the
    /// notification it had, one that is already right is left alone, and a
    /// pending request this app did not write is never touched.
    func testLocalRemindersReconcileRatherThanAppend() throws {
        let reminders = try dvrDecoder().decode([DvrReminder].self, from: JSONSerialization.data(
            withJSONObject: [
                dvrReminderJSON(id: "keep", channel: "7.1", start: 1_000, state: "armed"),
                dvrReminderJSON(id: "moved", channel: "5.1", start: 4_000, state: "armed"),
                dvrReminderJSON(id: "past", channel: "9.1", start: 100, state: "armed"),
                dvrReminderJSON(id: "acked", channel: "3.1", start: 9_000, state: "acked"),
            ]))
        let wanted = LocalReminderPlan.requests(for: reminders, now: 0)
        XCTAssertEqual(wanted.map(\.reminderId), ["keep", "moved"],
                       "only an armed reminder that has not yet fired is mirrored")
        XCTAssertEqual(wanted.map(\.fireAt), [700, 3_700], "the lead is subtracted, once")

        let plan = LocalReminderPlan.reconcile(
            pending: [
                LocalReminderPlan.identifier(reminderId: "keep", fireAt: 700),
                // The same reminder at the time it used to start.
                LocalReminderPlan.identifier(reminderId: "moved", fireAt: 2_500),
                LocalReminderPlan.identifier(reminderId: "gone", fireAt: 8_000),
                "somebody.elses.request",
            ],
            wanted: wanted)
        XCTAssertEqual(plan.cancel, [
            LocalReminderPlan.identifier(reminderId: "moved", fireAt: 2_500),
            LocalReminderPlan.identifier(reminderId: "gone", fireAt: 8_000),
        ], "a moved or deleted reminder loses its pending request; a stranger's does not")
        XCTAssertEqual(plan.schedule.map(\.reminderId), ["moved"],
                       "the one already scheduled at the right instant is left alone")
    }
    #endif
}

/// Counts what actually reached the server. The coalescing test asserted a
/// constant before, which would have passed with `requestChannel` deleted.
private final class LiveTvCountingRequests: LiveTvRequests, @unchecked Sendable {
    private(set) var starts = 0
    /// Legacy by default, so nothing in these tests touches the real hint file.
    func recoveryRoutesAvailable() async -> Bool { false }
    func start(_ channel: String, requestId: String?) async throws -> LiveTvStarted {
        starts += 1
        return LiveTvStarted(
            sessionId: "cap-\(starts)",
            channel: LiveTvChannel(id: channel, guideNumber: channel, guideName: "Test",
                                   favorite: false, drm: false, support: "ready",
                                   hd: nil, videoCodec: nil, audioCodec: nil),
            live: true)
    }
    func release(_ capability: String) async throws {}
    func resume(_ requestId: String) async throws -> LiveTvResumeAnswer { LiveTvResumeAnswer(outcome: "retired") }
    func retire(_ requestId: String) async throws {}
}

/// The hint store with the disk taken out. Nothing in the suite may write the
/// app container's real `live-tv-start.hint` except the round-trip test that
/// cleans up after itself.
private final class LiveTvMemoryHintStore: LiveTvHintStore, @unchecked Sendable {
    var value: LiveTvStartHint?
    var writeFails = false
    func read() -> LiveTvStartHint? { value }
    func write(_ hint: LiveTvStartHint) throws {
        if writeFails { throw CocoaError(.fileWriteOutOfSpace) }
        value = hint
    }
    func clear() { value = nil }
}

@MainActor
private final class LiveTvMockRequests: LiveTvRequests {
    var result: LiveTvStarted
    var events: [String] = []
    var releaseFails = false
    var startFailure: Error?
    var holdStart = false
    var started: XCTestExpectation?
    var continuation: CheckedContinuation<LiveTvStarted, Error>?
    /// What the last channels response listed. `nil` is an ingress older than
    /// the contract; the default is that same legacy shape, so a test that
    /// says nothing about protocols exercises the legacy path.
    var protocols: [Int]?
    /// Every `request_id` a start POST carried, in order — `nil` for a body
    /// that deliberately omitted the field.
    var startRequestIds: [String?] = []
    var resumeAnswer: LiveTvResumeAnswer?
    var resumeFailure: Error?
    var holdRetire = false
    var retireEntered: XCTestExpectation?
    var retireContinuation: CheckedContinuation<Void, Error>?
    var retireFinished = false

    init(result: LiveTvStarted) { self.result = result }

    func recoveryRoutesAvailable() async -> Bool {
        LiveTvStartReducer.negotiatesRecovery(protocols: protocols)
    }

    func start(_ channel: String, requestId: String?) async throws -> LiveTvStarted {
        events.append("start:\(channel)")
        startRequestIds.append(requestId)
        if holdStart {
            return try await withCheckedThrowingContinuation { continuation in
                self.continuation = continuation
                started?.fulfill()
            }
        }
        if let startFailure { throw startFailure }
        return result
    }
    func release(_ capability: String) async throws {
        events.append("release:\(capability)")
        if releaseFails { throw LiveTvFailure(code: "owner_unavailable") }
    }
    func resume(_ requestId: String) async throws -> LiveTvResumeAnswer {
        events.append("resume:\(requestId)")
        if let resumeFailure { throw resumeFailure }
        guard let resumeAnswer else { throw LiveTvFailure(code: "owner_unavailable") }
        return resumeAnswer
    }
    func retire(_ requestId: String) async throws {
        events.append("retire:\(requestId)")
        if holdRetire {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                self.retireContinuation = continuation
                retireEntered?.fulfill()
            }
        }
        retireFinished = true
    }
}
