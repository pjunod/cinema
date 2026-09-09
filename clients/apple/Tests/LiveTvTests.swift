import AVFoundation
import Foundation
import XCTest
@testable import plurx

@MainActor
final class LiveTvTests: XCTestCase {
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
        XCTAssertEqual(formatted.sourceFormatDescription, "HD source · HEVC video · AC4 audio")
        XCTAssertTrue(channel.formatBadges.isEmpty)

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
        // Both PlayerSurface sites hand the element to AVKit.
        XCTAssertEqual(source.components(separatedBy: "allowsPictureInPicture: true").count - 1, 2,
                       "both live surfaces must allow picture-in-picture")
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
        XCTAssertTrue(source.contains("if !visible { focusedControl = .reveal }"))
        // And the television is fullscreen without anyone pressing a button.
        XCTAssertTrue(source.contains(".onChange(of: live.playing) { _, playing in if playing { fullscreen = true } }"))
        // Search must be explicit on a television. Applying `.searchable` to
        // its List focused the field at entry and covered half the page with a
        // keyboard before the viewer asked for one.
        XCTAssertTrue(source.contains(".sheet(isPresented: $showingSearch)"))
        // tvOS 26's default tint can make a label and its capsule the same
        // accent colour. Page actions and rows own their focus contrast.
        XCTAssertTrue(source.contains(".buttonStyle(TVReadableButtonStyle(prominent: favoritesOnly))"))
        XCTAssertTrue(source.contains(".buttonStyle(LiveTvChannelButtonStyle())"))
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
