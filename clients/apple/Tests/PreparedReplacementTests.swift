import Foundation
import XCTest
@testable import plurx

// MARK: - Fixtures

private let stagingId = "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01"
private let successorSessionId = "0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d"

private func effectiveSelection(
    height: Int = 1_080,
    codec: String = "server_selected",
    dynamicRange: String? = "sdr"
) -> EffectiveSelection {
    EffectiveSelection(
        qualityAuto: true,
        height: height,
        audioTrack: nil,
        subtitleBurn: nil,
        audioOffsetMs: 0,
        codec: codec,
        dynamicRange: dynamicRange
    )
}

private func prepareAction(
    actionId: String = stagingId,
    sessionId: String = successorSessionId,
    playlistUrl: String? = nil,
    mediaOriginMs: Int = 0,
    selection: EffectiveSelection? = nil
) -> ControlAction {
    ControlAction(
        type: PlaybackControl.prepareActionType,
        actionId: actionId,
        sessionId: sessionId,
        playlistUrl: playlistUrl ?? "/api/v1/hls/\(sessionId)/index.m3u8",
        mediaOriginMs: mediaOriginMs,
        effectiveSelection: selection ?? effectiveSelection()
    )
}

private func preparedAction(
    actionId: String = stagingId,
    sessionId: String = successorSessionId,
    mediaOriginMs: Int = 0
) -> PreparedReplacementAction {
    PreparedReplacementAction(
        prepareAction(actionId: actionId, sessionId: sessionId, mediaOriginMs: mediaOriginMs)
    )!
}

/// A host that records what it was asked to do, so every rule the coordinator
/// applies is provable without AVFoundation, a device, or a server.
@MainActor
private final class RecordingHost: PreparedSuccessorHost {
    var started: [PreparedReplacementAction] = []
    var discards = 0
    var fallbacks: [PreparedReplacementAction] = []
    var exchanges = 0
    var interruptions: [Int] = []
    /// Pipelines alive right now, by the crudest possible count: one up per
    /// successful start, one down per discard.
    var alive = 0
    var startSucceeds = true
    var firstFrameUnixMs: Int? = 1_788_000_000_000

    func startPreparedSuccessor(
        _ action: PreparedReplacementAction,
        filmPositionMs: Int
    ) -> Bool {
        guard startSucceeds else { return false }
        started.append(action)
        alive += 1
        return true
    }

    func discardPreparedSuccessor() {
        discards += 1
        alive = 0
    }

    func commitPreparedSuccessor(_ action: PreparedReplacementAction) async -> Int? {
        alive = 0
        return firstFrameUnixMs
    }

    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction) {
        fallbacks.append(action)
    }

    func preparedSuccessorOwesAnExchange() { exchanges += 1 }

    func recordPreparedFallbackInterruption(ms: Int) { interruptions.append(ms) }
}

// MARK: - The wire

final class PreparedReplacementWireTests: XCTestCase {
    /// SSC12.1 — asserted against the serialized body, not the in-memory
    /// array. The serializer is what ships.
    func testTheDeclaredVocabularyIsExactOnTheEncodedRequest() throws {
        let request = ControlRequest(
            proto: PlaybackControl.protocolName,
            generation: "11111111-1111-4111-8111-111111111111",
            controlEpoch: 7,
            clientInstanceId: "22222222-2222-4222-8222-222222222222",
            sequence: 1,
            demand: .active,
            positionMs: 1_000,
            bufferedFromMs: 1_000,
            bufferedThroughMs: 11_000,
            playbackRate: 1,
            renderState: .rendering,
            seekTargetMs: nil,
            observedDownloadBps: 24_000_000,
            selection: ClientSelection(
                quality: .auto,
                audioTrack: 0,
                subtitle: SubtitleSelection(mode: .off, track: nil),
                audioOffsetMs: 0,
                codec: .auto,
                dynamicRange: .auto
            ),
            capabilities: nil,
            observation: nil
        )
        let body = try JSONSerialization.jsonObject(
            with: PlaybackControl.encoder.encode(request)
        ) as? [String: Any]
        XCTAssertEqual(
            body?["supported_actions"] as? [String],
            ["hold", "retry_resource", "terminal", "prepare_replacement"]
        )
        // SSC12.8 — a client that reports no throughput can never be offered a
        // preparation, whatever its capability says.
        XCTAssertEqual(body?["observed_download_bps"] as? Int, 24_000_000)
        XCTAssertNil(
            body?["acknowledgement"],
            "an absent settlement is an omitted key, not a null"
        )
    }

    /// SSC12.2 — the literal body, key by key. This is the assertion that
    /// catches the `prepare` / `prepare_replacement` trap: nothing in the
    /// server's own suite asserts the four fields a client actually consumes.
    func testTheInboundPrepareActionDecodesFieldByField() throws {
        let json = """
        {"protocol":"plurx-playback-control-v1",
         "generation":"11111111-1111-4111-8111-111111111111",
         "control_epoch":7,
         "accepted_sequence":4,
         "effective_selection":{"quality_auto":false,"height":720,
           "audio_track":1,"subtitle_burn":null,"audio_offset_ms":0,
           "codec":"source","dynamic_range":null},
         "action":{"type":"prepare",
          "action_id":"6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
          "session_id":"0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d",
          "playlist_url":"/api/v1/hls/0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d/index.m3u8",
          "media_origin_ms":0,
          "effective_selection":{"quality_auto":true,"height":1080,
            "audio_track":null,"subtitle_burn":null,"audio_offset_ms":0,
            "codec":"server_selected","dynamic_range":"sdr"}}}
        """
        let response = try PlaybackControl.decoder.decode(
            ControlResponse.self, from: Data(json.utf8)
        )
        XCTAssertEqual(response.action.type, "prepare")
        XCTAssertEqual(response.action.actionId, stagingId)
        XCTAssertEqual(response.action.sessionId, successorSessionId)
        XCTAssertEqual(
            response.action.playlistUrl,
            "/api/v1/hls/\(successorSessionId)/index.m3u8"
        )
        XCTAssertEqual(response.action.mediaOriginMs, 0)
        XCTAssertEqual(response.action.effectiveSelection, effectiveSelection())
        // The response's own selection is what the CURRENT session delivers.
        // Swift's decoder dropped it entirely until this milestone.
        XCTAssertEqual(
            response.effectiveSelection,
            EffectiveSelection(
                qualityAuto: false,
                height: 720,
                audioTrack: 1,
                subtitleBurn: nil,
                audioOffsetMs: 0,
                codec: "source",
                dynamicRange: nil
            )
        )
        XCTAssertNotNil(PreparedReplacementAction(response.action))
    }

    /// SSC12.4 — refused by the client, before any network request could be
    /// made, because validation is what turns the action into the type the
    /// player takes.
    func testAPlaylistThatIsNotNodeRelativeIsRefused() {
        let absolute = prepareAction(playlistUrl: "http://evil.example/api/v1/hls/x/index.m3u8")
        XCTAssertNil(PreparedReplacementAction(absolute))
        XCTAssertNil(PreparedReplacementAction(prepareAction(playlistUrl: "/api/v1/hls/other/index.m3u8")))
        XCTAssertNil(PreparedReplacementAction(prepareAction(playlistUrl: "/api/v1/hls/\(successorSessionId)/segment.ts")))
        XCTAssertNil(PreparedReplacementAction(prepareAction(
            playlistUrl: "//evil.example/api/v1/hls/\(successorSessionId)/index.m3u8"
        )))
        // A query or a fragment is allowed and ignored, exactly as the server
        // allows and ignores them.
        XCTAssertNotNil(PreparedReplacementAction(prepareAction(
            playlistUrl: "/api/v1/hls/\(successorSessionId)/master.m3u8?native=1"
        )))
        XCTAssertNotNil(PreparedReplacementAction(prepareAction(
            playlistUrl: "/api/v1/hls/\(successorSessionId)/index.m3u8#a"
        )))
        XCTAssertFalse(
            PreparedReplacementAction.isNodeRelativePlaylist(
                "/api/v1/hls/\(successorSessionId)/index.m3u8?"
                    + String(repeating: "a", count: 512),
                sessionId: successorSessionId
            ),
            "512 bytes is the server's own ceiling"
        )
    }

    func testAHalfPresentOrOutOfRangePreparePayloadIsRefused() {
        XCTAssertNil(PreparedReplacementAction(ControlAction(type: "prepare")))
        XCTAssertNil(PreparedReplacementAction(prepareAction(actionId: "not-a-uuid")))
        XCTAssertNil(PreparedReplacementAction(prepareAction(sessionId: "not-a-uuid")))
        XCTAssertNil(PreparedReplacementAction(prepareAction(mediaOriginMs: -1)))
        XCTAssertNil(PreparedReplacementAction(
            prepareAction(selection: effectiveSelection(codec: "hevc"))
        ))
        XCTAssertNil(PreparedReplacementAction(
            prepareAction(selection: effectiveSelection(height: 4_320))
        ))
        XCTAssertNil(PreparedReplacementAction(
            prepareAction(selection: effectiveSelection(dynamicRange: "hdr12"))
        ))
        // The declared name is not the tag: an action arriving under the name
        // this client declares is not a prepare and never becomes one.
        var misnamed = prepareAction()
        misnamed.type = PlaybackControl.prepareReplacementAction
        XCTAssertNil(PreparedReplacementAction(misnamed))
    }

    /// `codec` is the delivery method, not a codec name.
    func testTheDeliveryMethodVocabularyIsExactlyTwoValues() {
        XCTAssertTrue(effectiveSelection(codec: "source").isValid)
        XCTAssertTrue(effectiveSelection(codec: "server_selected").isValid)
        XCTAssertFalse(effectiveSelection(codec: "h264").isValid)
        XCTAssertTrue(effectiveSelection(codec: "server_selected").isServerSelected)
        XCTAssertFalse(effectiveSelection(codec: "source").isServerSelected)
    }

    /// SSC12.5 — the three terminal acknowledgements, and the pairing the
    /// server refuses outright.
    func testTerminalAcknowledgementsCarryWhatTheServerRequires() throws {
        let committed = ActionAcknowledgement(
            actionId: stagingId, state: .committed, firstFrameUnixMs: 1_788_000_000_000
        )
        XCTAssertTrue(committed.isValid)
        XCTAssertFalse(
            ActionAcknowledgement(actionId: stagingId, state: .committed).isValid,
            "committed requires first_frame_unix_ms"
        )
        XCTAssertFalse(
            ActionAcknowledgement(
                actionId: stagingId, state: .committed, firstFrameUnixMs: 0
            ).isValid
        )
        XCTAssertTrue(
            ActionAcknowledgement(
                actionId: stagingId, state: .bufferReady, bufferedThroughMs: 30_000
            ).isValid
        )
        XCTAssertFalse(
            ActionAcknowledgement(actionId: stagingId, state: .bufferReady).isValid,
            "buffer_ready requires buffered_through_ms"
        )
        XCTAssertFalse(
            ActionAcknowledgement(actionId: "nope", state: .aborted).isValid,
            "action_id must parse as a UUID"
        )
        XCTAssertTrue(ActionAcknowledgement(actionId: stagingId, state: .failed).isValid)

        // The five legal state strings, and no sixth.
        let encoded = try JSONSerialization.jsonObject(
            with: PlaybackControl.encoder.encode(committed)
        ) as? [String: Any]
        XCTAssertEqual(encoded?["state"] as? String, "committed")
        XCTAssertEqual(encoded?["action_id"] as? String, stagingId)
        XCTAssertEqual(encoded?["first_frame_unix_ms"] as? Int, 1_788_000_000_000)
        XCTAssertNil(encoded?["buffered_through_ms"])
        XCTAssertEqual(AcknowledgementState.metadataReady.rawValue, "metadata_ready")
        XCTAssertEqual(AcknowledgementState.bufferReady.rawValue, "buffer_ready")
    }

    /// The forbidden body is not merely unlikely; it cannot be constructed.
    func testACommitIsNeverBuiltOntoTheExchangeThatEndsTheSession() {
        let committed = ActionAcknowledgement(
            actionId: stagingId, state: .committed, firstFrameUnixMs: 1_788_000_000_000
        )
        XCTAssertNil(PlaybackControl.acknowledgement(committed, demand: .end))
        XCTAssertEqual(PlaybackControl.acknowledgement(committed, demand: .active), committed)
        XCTAssertEqual(PlaybackControl.acknowledgement(committed, demand: .hold), committed)
        // Every other settlement may end the session in the same breath.
        let aborted = ActionAcknowledgement(actionId: stagingId, state: .aborted)
        XCTAssertEqual(PlaybackControl.acknowledgement(aborted, demand: .end), aborted)
        // And one the server would refuse costs itself, not the exchange.
        XCTAssertNil(
            PlaybackControl.acknowledgement(
                ActionAcknowledgement(actionId: stagingId, state: .bufferReady),
                demand: .active
            )
        )
    }
}

// MARK: - The ledger

final class PreparedReplacementLedgerTests: XCTestCase {
    func testProgressIsMonotonicAndOneEntryPerStaging() {
        var ledger = PreparedReplacementLedger()
        ledger.open(preparedAction())
        ledger.noteMetadataReady()
        XCTAssertEqual(ledger.pendingAcknowledgement?.state, .metadataReady)
        ledger.noteBufferReady(bufferedThroughMs: 40_000)
        XCTAssertEqual(ledger.pendingAcknowledgement?.state, .bufferReady)
        XCTAssertEqual(ledger.pendingAcknowledgement?.bufferedThroughMs, 40_000)
        ledger.noteBufferReady(bufferedThroughMs: 55_000)
        XCTAssertEqual(ledger.acknowledgements.entries.count, 1)
        XCTAssertEqual(ledger.pendingAcknowledgement?.bufferedThroughMs, 55_000)
    }

    /// The same value rides every snapshot until the exchange that carried it
    /// comes back. Coalescing cannot drop a settlement.
    func testASettlementStaysPendingUntilItsExchangeReturns() {
        var ledger = PreparedReplacementLedger()
        ledger.open(preparedAction())
        ledger.noteCommitted(firstFrameUnixMs: 1_788_000_000_000)
        guard let owed = ledger.pendingAcknowledgement else {
            return XCTFail("a commit is owed the moment the first frame lands")
        }
        XCTAssertEqual(owed.state, .committed)
        ledger.acknowledgementDelivered(
            ActionAcknowledgement(actionId: stagingId, state: .metadataReady)
        )
        XCTAssertNotNil(ledger.pendingAcknowledgement, "a different value settles nothing")
        ledger.acknowledgementDelivered(owed)
        XCTAssertNil(ledger.pendingAcknowledgement)
    }

    /// A superseded staging is owed `aborted`, and it is owed it *ahead* of
    /// the newer staging's progress: exactly one acknowledgement rides each
    /// exchange, so a slot would lose one of the two.
    func testASupersededStagingIsSettledAheadOfItsSuccessor() {
        let first = preparedAction()
        let second = preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
        var ledger = PreparedReplacementLedger()
        ledger.open(first)
        ledger.noteSuperseded(first)
        ledger.open(second)
        ledger.noteMetadataReady()
        XCTAssertEqual(ledger.acknowledgements.entries.map(\.state), [.aborted, .metadataReady])
        XCTAssertEqual(ledger.acknowledgements.entries.first?.actionId, first.actionId)
        XCTAssertEqual(ledger.acknowledgements.entries.last?.actionId, second.actionId)
    }

    func testASettledStagingIsNeverReopenedByItsOwnReplay() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction()
        ledger.open(action)
        XCTAssertEqual(ledger.offer(action), .alreadyOpen)
        ledger.noteAbandoned(.failed)
        XCTAssertEqual(ledger.offer(action), .alreadySettled)
        XCTAssertFalse(ledger.hasActivePreparation)
    }

    func testATerminalStateIsNeverOverwrittenByProgress() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction()
        ledger.open(action)
        ledger.noteAbandoned(.aborted)
        ledger.open(action)
        ledger.noteMetadataReady()
        XCTAssertEqual(ledger.acknowledgements.entries.map(\.state), [.aborted])
    }
}

// MARK: - The coordinator

@MainActor
final class PreparedReplacementCoordinatorTests: XCTestCase {
    /// SSC12.7 — feed the same action twice, and exactly one pipeline exists.
    func testARepeatedActionIdIsOnePreparation() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        XCTAssertEqual(coordinator.offer(action, filmPositionMs: 1_000), .build(action))
        XCTAssertEqual(coordinator.offer(action, filmPositionMs: 1_000), .alreadyOpen)
        XCTAssertEqual(coordinator.offer(action, filmPositionMs: 1_000), .alreadyOpen)
        XCTAssertEqual(host.started.count, 1)
        XCTAssertEqual(host.alive, 1)
    }

    /// SS5.2 — prepare then abandon: the successor is released and the
    /// incumbent was never asked to change.
    func testAnAbandonedPreparationReleasesTheSuccessorAndLeavesTheIncumbent() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.abandonWithoutFallback(.aborted)
        XCTAssertEqual(host.alive, 0)
        XCTAssertTrue(host.fallbacks.isEmpty, "the viewer already moved on")
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.state, .aborted,
            "SSC12.6 — a preparation this client abandons is always settled"
        )
        XCTAssertFalse(coordinator.hasActivePreparation)
    }

    /// SS5.5 — a successor that cannot be made ready settles as `failed`, the
    /// in-place path takes over, and no pipeline is left alive.
    func testASuccessorThatWillNotBuildFailsAndFallsBack() {
        let host = RecordingHost()
        host.startSucceeds = false
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 1_000)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .failed)
        XCTAssertEqual(host.fallbacks, [action])
        XCTAssertEqual(host.alive, 0)
        XCTAssertFalse(coordinator.hasActivePreparation)
    }

    func testASwitchThatProducesNoFrameIsFailedRatherThanCommitted() async {
        let host = RecordingHost()
        host.firstFrameUnixMs = nil
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .failed)
        XCTAssertEqual(host.fallbacks, [action])
        XCTAssertEqual(
            host.interruptions.count, 1,
            "the fallback interruption is unmeasured on Apple; this is the instrument"
        )
    }

    func testACommitCarriesTheFirstFrameAndSettlesTheStaging() async {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .committed)
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.firstFrameUnixMs, 1_788_000_000_000
        )
        XCTAssertFalse(coordinator.hasActivePreparation)
        XCTAssertTrue(host.fallbacks.isEmpty)
        XCTAssertEqual(host.alive, 0)
    }

    /// A commit is never sent from a staging that has not been made ready: it
    /// is a claim that the viewer is already watching the successor.
    func testACommitIsRefusedBeforeTheSuccessorIsReady() async {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        await coordinator.commit()
        XCTAssertNil(coordinator.pendingAcknowledgement)
        XCTAssertEqual(coordinator.phase, .building)
    }

    func testANewStagingAbortsTheOldOneBeforeBuilding() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        let first = preparedAction()
        let second = preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
        coordinator.offer(first, filmPositionMs: 1_000)
        coordinator.offer(second, filmPositionMs: 1_000)
        XCTAssertEqual(host.started, [first, second])
        XCTAssertEqual(host.alive, 1, "one successor at a time")
        XCTAssertEqual(coordinator.pendingAcknowledgement?.actionId, first.actionId)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
    }

    func testAReadinessBoundElapsingIsAnOrdinaryFailure() {
        let host = RecordingHost()
        var clock = 1_000
        let coordinator = PreparedReplacementCoordinator(host: host, now: { clock })
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        XCTAssertFalse(coordinator.readinessBoundElapsed())
        clock += PreparedReplacementBounds.metadataMs
        XCTAssertTrue(coordinator.readinessBoundElapsed())
        coordinator.successorIsMetadataReady()
        XCTAssertFalse(
            coordinator.readinessBoundElapsed(),
            "metadata arriving buys the longer readiness bound"
        )
        clock += PreparedReplacementBounds.readinessMs
        XCTAssertTrue(coordinator.readinessBoundElapsed())
    }

    /// The whole cost of a server that stages a successor it never primes,
    /// paid once instead of on every tap.
    func testASuccessorThatNeverBecomesPlayableStopsTheAskingForThisPlayer() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        XCTAssertTrue(coordinator.shouldAskForPreparation)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.abandon(.failed)
        XCTAssertFalse(
            coordinator.shouldAskForPreparation,
            "the viewer already paid to find out that nothing can be primed here"
        )
        // A new player is a new server, a new device state and a new source.
        coordinator.playerIsEnding()
        XCTAssertTrue(coordinator.shouldAskForPreparation)
    }

    /// A successor that *did* produce media and then failed says nothing about
    /// the next one, so the asking continues.
    func testAFailureAfterReadinessDoesNotStopTheAsking() async {
        let host = RecordingHost()
        host.firstFrameUnixMs = nil
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertTrue(coordinator.shouldAskForPreparation)
    }

    /// An abort is the viewer moving on, never evidence about the server.
    func testAnAbortDoesNotStopTheAsking() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.abandonWithoutFallback(.aborted)
        XCTAssertTrue(coordinator.shouldAskForPreparation)
    }

    func testEndingThePlayerSettlesAndFreesWhateverIsLive() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.playerIsEnding()
        XCTAssertEqual(host.alive, 0)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
        XCTAssertTrue(host.fallbacks.isEmpty)
    }
}

// MARK: - Alignment

final class PreparedReplacementAlignmentTests: XCTestCase {
    /// SS5.3 — the generalised resolver over a raw origin, and the
    /// `HlsStart` path it was generalised out of, agreeing exactly.
    func testTheRawOriginResolverMatchesTheSessionPathItWasGeneralisedFrom() {
        // VOD begins at zero whatever it was asked for.
        XCTAssertEqual(
            PlayerController.sessionMediaOriginMs(
                vod: true, mediaOriginMs: 90_000, startSeconds: 9, requestedStartMs: 5_000
            ),
            0
        )
        // A live-recovery session's integer origin wins.
        XCTAssertEqual(
            PlayerController.sessionMediaOriginMs(
                vod: false, mediaOriginMs: 90_000, startSeconds: 9, requestedStartMs: 5_000
            ),
            90_000
        )
        XCTAssertEqual(
            PlayerController.sessionMediaOriginMs(
                vod: false, mediaOriginMs: -5, startSeconds: nil, requestedStartMs: 5_000
            ),
            0
        )
        // The floating-point echo remains the compatibility fallback.
        XCTAssertEqual(
            PlayerController.sessionMediaOriginMs(
                vod: false, mediaOriginMs: nil, startSeconds: 9.5, requestedStartMs: 5_000
            ),
            9_500
        )
        XCTAssertEqual(
            PlayerController.sessionMediaOriginMs(
                vod: false, mediaOriginMs: nil, startSeconds: nil, requestedStartMs: 5_000
            ),
            5_000
        )
    }

    /// The successor's own zero is not the film's, and the correction is the
    /// one a created session already gets.
    func testTheSuccessorIsPrimedToTheFilmPositionThroughItsOwnOrigin() {
        XCTAssertEqual(
            PlayerController.sessionAttachSeekMs(
                requestedStartMs: 630_000, mediaOriginMs: 600_000
            ),
            30_000
        )
        XCTAssertNil(
            PlayerController.sessionAttachSeekMs(
                requestedStartMs: 600_040, mediaOriginMs: 600_000
            ),
            "timestamp rounding is not a seek"
        )
        XCTAssertNil(
            PlayerController.sessionAttachSeekMs(requestedStartMs: 0, mediaOriginMs: 0)
        )
    }
}
