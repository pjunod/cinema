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
        for status in [400, 401, 403, 404, 503] {
            let response = try XCTUnwrap(HTTPURLResponse(url: url, statusCode: status, httpVersion: nil, headerFields: nil))
            XCTAssertThrowsError(try PlurxAPI.check(response, data: data)) { error in
                guard case APIError.http(let code) = error else { return XCTFail("HTTP classification changed") }
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

    func testPlatformRestartMarkerStoreCanRoundTrip() throws {
        let store = LiveTvFileBarrierStore()
        try store.setPending(false)
        defer { try? store.setPending(false) }

        XCTAssertFalse(try store.pending())
        try store.setPending(true)
        XCTAssertTrue(try store.pending())
        try store.setPending(false)
        XCTAssertFalse(try store.pending())
    }

    private let channel = LiveTvChannel(id: "7.1", guideNumber: "7.1", guideName: "Local",
                                        favorite: false, drm: false, support: "ready",
                                        hd: nil, videoCodec: nil, audioCodec: nil)

    private func started(_ capability: String = "one") -> LiveTvStarted {
        LiveTvStarted(sessionId: capability, channel: channel, live: true)
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

    func testSwitchConfirmsReleaseBeforeStartingAnotherChannel() async throws {
        let requests = LiveTvMockRequests(result: started())
        let lease = LiveTvLease(requests: requests, barrier: LiveTvStartBarrier())
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
        let lease = LiveTvLease(requests: requests, barrier: LiveTvStartBarrier())
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
        let lease = LiveTvLease(requests: requests, barrier: LiveTvStartBarrier())
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

    func testTypedAmbiguousStartSurvivesProfileSwitchUntilMonotonicExpiry() async throws {
        var now = ContinuousClock.now
        let barrier = LiveTvStartBarrier(now: { now })
        let oldProfile = LiveTvMockRequests(result: started())
        oldProfile.startFailure = LiveTvFailure(code: "owner_unavailable")
        let oldLease = LiveTvLease(requests: oldProfile, barrier: barrier)
        do { _ = try await oldLease.start("first"); XCTFail("expected unknown result") }
        catch let failure as LiveTvFailure { XCTAssertEqual(failure.code, "start_outcome_unknown") }
        try await oldLease.stop()
        let newProfile = LiveTvMockRequests(result: started("new"))
        let newLease = LiveTvLease(requests: newProfile, barrier: barrier)
        do { _ = try await newLease.start("second"); XCTFail("profile switch must not bypass uncertainty") } catch {}
        XCTAssertEqual(newProfile.events, [])
        now = now.advanced(by: .seconds(89))
        XCTAssertTrue(barrier.pending)
        now = now.advanced(by: .seconds(2))
        XCTAssertFalse(barrier.pending)
        let result = try await newLease.start("third")
        XCTAssertEqual(result?.sessionId, "new")
        try await newLease.stop()
    }

    func testDefiniteCapacityRefusalDoesNotCreateUncertainty() async throws {
        let barrier = LiveTvStartBarrier()
        let requests = LiveTvMockRequests(result: started())
        requests.startFailure = LiveTvFailure(code: "tuner_capacity")
        let lease = LiveTvLease(requests: requests, barrier: barrier)
        do { _ = try await lease.start("first"); XCTFail("capacity refusal expected") } catch {}
        XCTAssertFalse(barrier.pending)
        XCTAssertNil(lease.current)
    }

    func testPendingPostAndFailedCleanupSurviveAppRestart() throws {
        var now = ContinuousClock.now
        let disk = LiveTvMemoryBarrierStore()
        let original = LiveTvStartBarrier(now: { now }, persistence: disk)
        try original.begin()
        XCTAssertTrue(disk.value, "marker must precede POST")
        let restarted = LiveTvStartBarrier(now: { now }, persistence: disk)
        XCTAssertThrowsError(try restarted.begin())
        now = now.advanced(by: .seconds(91))
        try restarted.begin()
        restarted.confirm()
        XCTAssertFalse(disk.value)
        try restarted.arm() // before a DELETE whose response never arrives
        let afterCleanupLoss = LiveTvStartBarrier(now: { now }, persistence: disk)
        XCTAssertTrue(afterCleanupLoss.pending)
        XCTAssertThrowsError(try afterCleanupLoss.begin())
    }

    func testFailedPersistencePreventsPostDispatch() async throws {
        let disk = LiveTvMemoryBarrierStore()
        disk.writeFails = true
        let barrier = LiveTvStartBarrier(persistence: disk)
        let requests = LiveTvMockRequests(result: started())
        let lease = LiveTvLease(requests: requests, barrier: barrier)
        do { _ = try await lease.start("one"); XCTFail("storage failure must precede POST") }
        catch let failure as LiveTvFailure { XCTAssertEqual(failure.code, "live_tv_storage_unavailable") }
        XCTAssertEqual(requests.events, [])
    }

    func testActiveOwnershipSurvivesCrashButNormalSwitchStillReleases() async throws {
        let disk = LiveTvMemoryBarrierStore()
        let barrier = LiveTvStartBarrier(persistence: disk)
        let requests = LiveTvMockRequests(result: started())
        let lease = LiveTvLease(requests: requests, barrier: barrier)
        _ = try await lease.start("one")
        XCTAssertFalse(barrier.pending, "known ownership is not an uncertain outcome")
        XCTAssertTrue(disk.value, "disk ownership survives a crash")
        let restarted = LiveTvStartBarrier(persistence: disk)
        XCTAssertTrue(restarted.pending)
        XCTAssertThrowsError(try restarted.begin())
        requests.result = started("two")
        _ = try await lease.start("two")
        XCTAssertEqual(requests.events, ["start:one", "release:one", "start:two"])
        try await lease.stop()
        XCTAssertFalse(disk.value)
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
        let config = try fields(.configure(ipv4: "192.168.4.100", owner: "new-owner", sessions: 2, height: 720))
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
        let dto = try decoder.decode(LiveTvSettings.self, from: Data(#"{"live_tv_enabled":false,"live_tv_device_ipv4":"192.168.4.100","live_tv_owner_node_id":"owner","live_tv_max_sessions":2,"live_tv_output_height":720,"live_tv_config_generation":12,"live_tv_transition_from_owner_node_id":"old","live_tv_transition_drain_before":8,"tmdb_api_key":"unused-secret"}"#.utf8))
        XCTAssertEqual(dto.liveTvDeviceIpv4, "192.168.4.100")
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
                enum CodingKeys: String, CodingKey {
                    case hideAfterMs = "hide_after_ms"
                    case channelCoalesceMs = "channel_coalesce_ms"
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
        XCTAssertTrue(source.contains("focusedControl = visible ? .guide : .reveal"))
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
        XCTAssertEqual(LiveTvPlayerController.guideRefreshNanoseconds, 20 * 60 * 1_000_000_000)
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
        for style in [".font(.subheadline", ".font(.title2", ".font(.title3",
                      ".font(.callout", ".font(.headline"] {
            XCTAssertFalse(source.contains(style),
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
}

/// Counts what actually reached the server. The coalescing test asserted a
/// constant before, which would have passed with `requestChannel` deleted.
private final class LiveTvCountingRequests: LiveTvRequests, @unchecked Sendable {
    private(set) var starts = 0
    func start(_ channel: String) async throws -> LiveTvStarted {
        starts += 1
        return LiveTvStarted(
            sessionId: "cap-\(starts)",
            channel: LiveTvChannel(id: channel, guideNumber: channel, guideName: "Test",
                                   favorite: false, drm: false, support: "ready",
                                   hd: nil, videoCodec: nil, audioCodec: nil),
            live: true)
    }
    func release(_ capability: String) async throws {}
}

private final class LiveTvMemoryBarrierStore: LiveTvBarrierStore {
    var value = false
    var writeFails = false
    func pending() throws -> Bool { value }
    func setPending(_ value: Bool) throws {
        if writeFails { throw CocoaError(.fileWriteOutOfSpace) }
        self.value = value
    }
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

    init(result: LiveTvStarted) { self.result = result }
    func start(_ channel: String) async throws -> LiveTvStarted {
        events.append("start:\(channel)")
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
}
