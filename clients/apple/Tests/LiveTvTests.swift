import AVFoundation
import Foundation
import XCTest
@testable import plurx

@MainActor
final class LiveTvTests: XCTestCase {
    private let channel = LiveTvChannel(id: "7.1", guideNumber: "7.1", guideName: "Local",
                                        favorite: false, drm: false, support: "ready")

    private func started(_ capability: String = "one") -> LiveTvStarted {
        LiveTvStarted(sessionId: capability, channel: channel, live: true)
    }

    func testProtectedChannelsStayVisibleAndUnwatchable() {
        let protected = LiveTvChannel(id: "107.1", guideNumber: "107.1", guideName: "Protected",
                                      favorite: false, drm: true, support: "drm_unsupported")
        XCTAssertFalse(protected.watchable)
        XCTAssertEqual(protected.title, "107.1 · Protected")
        XCTAssertTrue(channel.watchable)
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

    func testSettingsWritesSeparateConfigEnableAndExactPhysicalRecovery() throws {
        func fields(_ change: LiveTvSettingsChange) throws -> [String: Any] {
            try XCTUnwrap(JSONSerialization.jsonObject(with: change.body(generation: 12)) as? [String: Any])
        }
        let config = try fields(.configure(ipv4: "192.168.4.100", owner: "new-owner", sessions: 2, height: 720))
        XCTAssertNil(config["live_tv_enabled"])
        XCTAssertEqual(config["live_tv_config_generation"] as? Int, 12)
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
