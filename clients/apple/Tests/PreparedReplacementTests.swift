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
/// Holds a commit open so a test can observe the switching window — the one
/// window every re-entrancy bug in this file lives in.
@MainActor
private final class CommitGate {
    private var resume: CheckedContinuation<Void, Never>?
    private var opened = false

    func wait() async {
        guard !opened else { return }
        await withCheckedContinuation { continuation in
            if opened { continuation.resume() } else { resume = continuation }
        }
    }

    func open() {
        opened = true
        resume?.resume()
        resume = nil
    }
}

@MainActor
private final class RecordingHost: PreparedSuccessorHost {
    var commitGate: CommitGate?
    var started: [PreparedReplacementAction] = []
    var discards = 0
    var fallbacks: [PreparedReplacementAction] = []
    var exchanges = 0
    var interruptions: [Int] = []
    /// Pipelines alive right now. One up per successful start, one *down* per
    /// discard rather than a reset to zero: a discard that frees nothing, or
    /// two starts against one discard, is exactly the leak this counts, and a
    /// counter that clamps to zero could not see either.
    var alive = 0
    var startSucceeds = true
    var outcome: PreparedCommitOutcome = .committed(firstFrameUnixMs: 1_788_000_000_000)
    var commits: [PreparedReplacementAction] = []

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
        if alive > 0 { alive -= 1 }
    }

    func commitPreparedSuccessor(
        _ action: PreparedReplacementAction
    ) async -> PreparedCommitOutcome {
        commits.append(action)
        await commitGate?.wait()
        if case .refused = outcome { return outcome }
        // A switch consumes the pipeline whether or not a frame proves it.
        if alive > 0 { alive -= 1 }
        return outcome
    }

    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction) {
        fallbacks.append(action)
    }

    func preparedSuccessorOwesAnExchange() { exchanges += 1 }

    /// Contract §3.3 row 18, recorded so the wiring is provable rather than
    /// assumed: every abandonment of a live staging owes exactly one of these.
    var abandonments: [PreparedReplacementAbandonment] = []

    func notePreparedSuccessorAbandoned(_ reason: PreparedReplacementAbandonment) {
        abandonments.append(reason)
    }

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
            actionId: stagingId,
            state: .committed,
            committedMediaOriginMs: 600_000,
            firstFrameUnixMs: 1_788_000_000_000
        )
        XCTAssertTrue(committed.isValid)
        XCTAssertFalse(
            ActionAcknowledgement(actionId: stagingId, state: .committed).isValid,
            "committed requires first_frame_unix_ms"
        )
        XCTAssertFalse(
            ActionAcknowledgement(
                actionId: stagingId,
                state: .committed,
                committedMediaOriginMs: 600_000,
                firstFrameUnixMs: 0
            ).isValid
        )
        XCTAssertFalse(
            ActionAcknowledgement(
                actionId: stagingId, state: .committed, firstFrameUnixMs: 1_788_000_000_000
            ).isValid,
            "committed also requires committed_media_origin_ms — the server compares it"
        )
        XCTAssertFalse(
            ActionAcknowledgement(
                actionId: stagingId,
                state: .committed,
                committedMediaOriginMs: -1,
                firstFrameUnixMs: 1_788_000_000_000
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
        XCTAssertEqual(encoded?["committed_media_origin_ms"] as? Int, 600_000)
        XCTAssertNil(encoded?["buffered_through_ms"])
        XCTAssertEqual(AcknowledgementState.metadataReady.rawValue, "metadata_ready")
        XCTAssertEqual(AcknowledgementState.bufferReady.rawValue, "buffer_ready")
    }

    /// The forbidden body is not merely unlikely; it cannot be constructed.
    func testACommitIsNeverBuiltOntoTheExchangeThatEndsTheSession() {
        let committed = ActionAcknowledgement(
            actionId: stagingId,
            state: .committed,
            committedMediaOriginMs: 0,
            firstFrameUnixMs: 1_788_000_000_000
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

    /// The commit echoes the offer's own origin, whatever the player thinks.
    func testACommitEchoesTheOffersOriginRatherThanRecomputingIt() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction(mediaOriginMs: 600_000)
        ledger.open(action)
        ledger.noteCommitted(action, firstFrameUnixMs: 1_788_000_000_000)
        XCTAssertEqual(ledger.pendingAcknowledgement?.committedMediaOriginMs, 600_000)
        XCTAssertTrue(ledger.pendingAcknowledgement?.isValid == true)
    }

    /// The same value rides every snapshot until the exchange that carried it
    /// comes back. Coalescing cannot drop a settlement.
    func testASettlementStaysPendingUntilItsExchangeReturns() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction()
        ledger.open(action)
        ledger.noteCommitted(action, firstFrameUnixMs: 1_788_000_000_000)
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
        ledger.noteAbandoned(first, .aborted)
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

    /// A settled staging never reopens through the only door there is.
    func testATerminalStateIsNeverOverwrittenByProgress() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction()
        ledger.open(action)
        ledger.noteAbandoned(.aborted)
        // Through `offer`, which is how a replay actually arrives — a direct
        // `open` of a settled staging is a state the production path cannot
        // reach, and testing through it would prove nothing about the code
        // that runs.
        XCTAssertEqual(ledger.offer(action), .alreadySettled)
        ledger.noteMetadataReady()
        XCTAssertEqual(ledger.acknowledgements.entries.map(\.state), [.aborted])
    }

    /// At the ceiling the value being recorded is the one worth keeping: if it
    /// is a settlement, it is the one holding a server slot open right now.
    func testTheQueueCeilingDropsAProgressReportRatherThanASettlement() {
        var ledger = PreparedAcknowledgementLedger()
        let ids = (0..<PreparedAcknowledgementLedger.capacity).map {
            "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e\(String(format: "%02d", $0))"
        }
        ledger.record(ActionAcknowledgement(actionId: ids[0], state: .metadataReady))
        for id in ids.dropFirst() {
            ledger.record(ActionAcknowledgement(actionId: id, state: .aborted))
        }
        XCTAssertEqual(ledger.entries.count, PreparedAcknowledgementLedger.capacity)
        let late = ActionAcknowledgement(
            actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12", state: .failed
        )
        ledger.record(late)
        XCTAssertEqual(ledger.entries.count, PreparedAcknowledgementLedger.capacity)
        XCTAssertTrue(ledger.entries.contains(late), "the incoming settlement is never the victim")
        XCTAssertFalse(
            ledger.entries.contains { $0.state == .metadataReady },
            "progress is what goes: it reports nothing the server is waiting on"
        )
        // And with nothing but settlements queued, the oldest goes rather than
        // the newest being thrown away.
        var terminalOnly = PreparedAcknowledgementLedger()
        for id in ids { terminalOnly.record(ActionAcknowledgement(actionId: id, state: .aborted)) }
        terminalOnly.record(late)
        XCTAssertTrue(terminalOnly.entries.contains(late))
        XCTAssertEqual(terminalOnly.entries.first?.actionId, ids[1])
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

    /// Contract §3.3 row 18 — prepared-successor abandonment is `log_only`:
    /// nothing was ever switched to, so the incumbent is untouched and the
    /// event is the only trace there was ever a second pipeline.
    ///
    /// Every route out of a live staging that is NOT a committed switch is one
    /// abandonment, and exactly one: the viewer moving on, the successor's own
    /// item failing, a readiness bound elapsing, a refused commit, and a
    /// switch that produced no frame.
    func testEveryAbandonmentOfALiveStagingIsReportedExactlyOnce() async {
        let viewerMovedOn = RecordingHost()
        let moved = PreparedReplacementCoordinator(host: viewerMovedOn)
        moved.offer(preparedAction(), filmPositionMs: 1_000)
        moved.abandonWithoutFallback(.aborted)
        moved.abandonWithoutFallback(.aborted)
        XCTAssertEqual(
            viewerMovedOn.abandonments, [.aborted],
            "abandoned once, reported once — a settled staging cannot be abandoned again"
        )

        let successorFailed = RecordingHost()
        let failed = PreparedReplacementCoordinator(host: successorFailed)
        failed.offer(preparedAction(), filmPositionMs: 1_000)
        failed.abandon(.failed)
        XCTAssertEqual(successorFailed.abandonments, [.failed])
        XCTAssertEqual(successorFailed.fallbacks.count, 1, "and the in-place path still runs")

        let refused = RecordingHost()
        refused.outcome = .refused
        let refusedCoordinator = PreparedReplacementCoordinator(host: refused)
        refusedCoordinator.offer(preparedAction(), filmPositionMs: 1_000)
        refusedCoordinator.successorIsMetadataReady()
        refusedCoordinator.successorIsBuffered(throughMs: 40_000)
        await refusedCoordinator.commit()
        XCTAssertEqual(refused.abandonments, [.aborted], "nothing was switched")

        let frameless = RecordingHost()
        frameless.outcome = .switchedWithoutAFrame
        let framelessCoordinator = PreparedReplacementCoordinator(host: frameless)
        framelessCoordinator.offer(preparedAction(), filmPositionMs: 1_000)
        framelessCoordinator.successorIsMetadataReady()
        framelessCoordinator.successorIsBuffered(throughMs: 40_000)
        await framelessCoordinator.commit()
        XCTAssertEqual(frameless.abandonments, [.failed])

        let committed = RecordingHost()
        let committedCoordinator = PreparedReplacementCoordinator(host: committed)
        committedCoordinator.offer(preparedAction(), filmPositionMs: 1_000)
        committedCoordinator.successorIsMetadataReady()
        committedCoordinator.successorIsBuffered(throughMs: 40_000)
        await committedCoordinator.commit()
        XCTAssertEqual(
            committed.abandonments, [],
            "a switch the viewer is watching is not an abandonment"
        )
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

    /// The switch happened and no frame proved it. There is no incumbent left
    /// to keep, so this is a settlement *and* a reopen — and the interruption
    /// is the number Apple has never had.
    func testASwitchThatProducesNoFrameIsFailedRatherThanCommitted() async {
        let host = RecordingHost()
        host.outcome = .switchedWithoutAFrame
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .failed)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.actionId, action.actionId)
        XCTAssertEqual(host.fallbacks, [action])
        XCTAssertEqual(host.alive, 0)
        XCTAssertEqual(
            host.interruptions.count, 1,
            "the fallback interruption is unmeasured on Apple; this is the instrument"
        )
    }

    /// A commit the host would not even attempt is not evidence about
    /// anything: the incumbent is untouched, so it is an abort plus the
    /// ordinary in-place change, and this playback keeps asking.
    func testARefusedSwitchAbortsRatherThanFailsAndLeavesTheAskingAlone() async {
        let host = RecordingHost()
        host.outcome = .refused
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
        XCTAssertEqual(host.fallbacks, [action])
        XCTAssertTrue(host.interruptions.isEmpty, "nothing was interrupted")
        XCTAssertTrue(coordinator.shouldAskForPreparation)
        XCTAssertEqual(host.alive, 0)
    }

    /// A staging offered while a switch is in flight builds nothing, disturbs
    /// nothing, and — crucially — does not claim the viewer's change.
    func testAStagingOfferedDuringASwitchIsRefusedWithoutTouchingTheSwitch() async {
        let host = RecordingHost()
        let gate = CommitGate()
        host.commitGate = gate
        let coordinator = PreparedReplacementCoordinator(host: host)
        let first = preparedAction()
        let second = preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
        coordinator.offer(first, filmPositionMs: 1_000)
        coordinator.successorIsMetadataReady()
        // Held inside the host, which is where a real commit spends its time:
        // up to six seconds of waiting for the successor's first frame, with
        // MainActor free the whole way.
        let switching = Task { await coordinator.commit() }
        while host.commits.isEmpty { await Task.yield() }
        XCTAssertEqual(coordinator.phase, .switching)
        XCTAssertEqual(coordinator.offer(second, filmPositionMs: 1_000), .busy)
        XCTAssertFalse(PreparedReplacementOffer.busy.ownsTheChange)
        XCTAssertEqual(host.started, [first], "nothing is built on top of a switch")
        // Nor may a viewer command settle the staging the switch is committing.
        coordinator.abandonWithoutFallback(.aborted)
        coordinator.abandon(.failed)
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.state, .metadataReady,
            "the progress already reported is all that is owed mid-switch"
        )
        XCTAssertEqual(coordinator.phase, .switching)
        gate.open()
        await switching.value
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .committed)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.actionId, first.actionId)
    }

    /// The commit settles the staging it was called for, by name.
    ///
    /// The coordinator's switching critical section is what makes this state
    /// unreachable in production, and this is the ledger's own half of that
    /// guarantee: even handed a stale staging while a different one is live,
    /// it stamps the one it was given. A commit carrying another staging's
    /// identity would pass the server's `bound_preparation_acknowledgement`
    /// and move the playback pointer to a session nothing ever displayed.
    func testACommitSettlesTheStagingItWasCalledForRatherThanWhateverIsCurrent() {
        var ledger = PreparedReplacementLedger()
        let first = preparedAction(mediaOriginMs: 600_000)
        let second = preparedAction(
            actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12", mediaOriginMs: 900_000
        )
        ledger.open(first)
        ledger.noteMetadataReady()
        XCTAssertTrue(ledger.noteSwitching(first))
        ledger.open(second)
        ledger.noteCommitted(first, firstFrameUnixMs: 1_788_000_000_000)
        let committed = ledger.acknowledgements.entries.first { $0.state == .committed }
        XCTAssertEqual(committed?.actionId, first.actionId)
        XCTAssertEqual(
            committed?.committedMediaOriginMs, 600_000,
            "not 900000 — the origin the server compares is the offer's own"
        )
        XCTAssertEqual(ledger.active?.actionId, second.actionId, "the newer staging is untouched")
    }

    /// Nothing may switch from a staging that has not been made ready.
    func testNoteSwitchingRefusesAnUnreadyOrForeignStaging() {
        var ledger = PreparedReplacementLedger()
        let action = preparedAction()
        ledger.open(action)
        XCTAssertFalse(ledger.noteSwitching(action), "building is not ready")
        ledger.noteMetadataReady()
        XCTAssertFalse(
            ledger.noteSwitching(
                preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
            ),
            "a staging that is not the live one cannot start a switch"
        )
        XCTAssertTrue(ledger.noteSwitching(action))
    }

    /// A failed attempt settles that action, but a later explicit change may
    /// try again. Runtime failure is not a session-wide feature gate.
    func testAFailedStagingDoesNotSuppressTheNextExplicitChange() {
        let host = RecordingHost()
        host.startSucceeds = false
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        XCTAssertTrue(coordinator.shouldAskForPreparation)
        host.startSucceeds = true
        let pushed = preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
        XCTAssertEqual(coordinator.offer(pushed, filmPositionMs: 1_000), .build(pushed))
        XCTAssertEqual(host.started.count, 1)
    }

    /// A staging this client already settled changes nothing, so the caller
    /// still owes the viewer the change they asked for.
    func testASettledReplayDoesNotClaimTheViewersChange() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 1_000)
        coordinator.abandonWithoutFallback(.aborted)
        XCTAssertEqual(coordinator.offer(action, filmPositionMs: 1_000), .alreadySettled)
        XCTAssertFalse(PreparedReplacementOffer.alreadySettled.ownsTheChange)
        XCTAssertTrue(PreparedReplacementOffer.build(action).ownsTheChange)
        XCTAssertTrue(PreparedReplacementOffer.alreadyOpen.ownsTheChange)
        XCTAssertEqual(host.started.count, 1)
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
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.committedMediaOriginMs, 0,
            "echoed from the offer, never recomputed — the server refuses a mismatch"
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

    /// A failed transaction is evidence about that attempt, never a hidden
    /// feature gate for the rest of the playback.
    func testASuccessorThatNeverBecomesPlayableAllowsAnotherOfferForThisPlayer() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        XCTAssertTrue(coordinator.shouldAskForPreparation)
        coordinator.offer(preparedAction(), filmPositionMs: 1_000)
        coordinator.abandon(.failed)
        XCTAssertTrue(coordinator.shouldAskForPreparation)
        let next = preparedAction(actionId: "7c2e3b55-3c8f-4b2d-8a4f-3d6b8c9e0f12")
        XCTAssertEqual(
            coordinator.offer(next, filmPositionMs: 1_000),
            .build(next),
            "one failed action cannot suppress the viewer's next explicit change"
        )
    }

    /// A successor that *did* produce media and then failed says nothing about
    /// the next one, so the asking continues.
    func testAFailureAfterReadinessDoesNotStopTheAsking() async {
        let host = RecordingHost()
        host.outcome = .switchedWithoutAFrame
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

// MARK: - Ownership after the offer (M2)

/// Once a staging has been offered, **something** must reach the viewer: the
/// successor commits, or the ordinary in-place change runs. These are the three
/// ways the offered path can end, and the count of reopens each one owes.
///
/// M2 makes them load-bearing for the first time. Until now no viewer action
/// ever reached the prepared path at all — the ask waited on the exchange that
/// carried it, which cannot carry a `prepare` — so every one of these branches
/// was reachable only from the server's own push.
@MainActor
final class PreparedOfferOwnershipTests: XCTestCase {
    func testAnOfferWhoseSuccessorWillNotStartOwesExactlyOneReopen() {
        let host = RecordingHost()
        host.startSucceeds = false
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 30_000)
        XCTAssertEqual(host.fallbacks, [action], "exactly one, and it names this staging")
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .failed)
        XCTAssertEqual(host.alive, 0)
    }

    func testAnOfferThatRunsOutOfReadinessOwesExactlyOneReopen() {
        let host = RecordingHost()
        var clock = 1_000
        let coordinator = PreparedReplacementCoordinator(host: host, now: { clock })
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 30_000)
        coordinator.successorIsMetadataReady()
        clock += PreparedReplacementBounds.readinessMs
        XCTAssertTrue(coordinator.readinessBoundElapsed())
        coordinator.abandon(.failed)
        coordinator.abandon(.failed)
        XCTAssertEqual(
            host.fallbacks, [action],
            "a bound that elapses twice while the monitor polls is still one reopen"
        )
    }

    func testAnOfferSupersededByTheViewerOwesNoReopenFromThisPath() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        coordinator.offer(preparedAction(), filmPositionMs: 30_000)
        // What `beginViewerAction` does at the tap of the next command.
        coordinator.abandonWithoutFallback(.aborted)
        XCTAssertTrue(
            host.fallbacks.isEmpty,
            "the command that superseded this staging is itself the change being made"
        )
        XCTAssertEqual(host.alive, 0)
    }

    /// The abandoned ask's settlement is the thing that frees the server's one
    /// preparation slot for the rest of the session, so it is queued behind
    /// whatever the newer staging owes rather than dropped for it.
    func testTheAbortedAcknowledgementOfASupersededStagingIsQueuedNotDropped() {
        let host = RecordingHost()
        let coordinator = PreparedReplacementCoordinator(host: host)
        let first = preparedAction()
        let second = preparedAction(actionId: "2d4f6a80-1b3c-4d5e-8f90-a1b2c3d4e5f6")
        coordinator.offer(first, filmPositionMs: 30_000)
        coordinator.offer(second, filmPositionMs: 30_000)
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.actionId, first.actionId,
            "the settlement that frees the slot rides first"
        )
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
        // The newer staging makes progress; the older settlement still has not
        // been delivered, and must not be displaced by it.
        coordinator.successorIsMetadataReady()
        XCTAssertEqual(coordinator.pendingAcknowledgement?.actionId, first.actionId)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
        coordinator.acknowledgementDelivered(
            ActionAcknowledgement(actionId: first.actionId, state: .aborted)
        )
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.actionId, second.actionId,
            "and the newer staging's progress was waiting behind it, not lost"
        )
    }

    /// There is no incumbent left to put back: its item is gone and its session
    /// released. Recording the interruption without reopening would leave the
    /// viewer looking at a frozen frame with the instrument that measured it.
    func testASwitchWithoutAFrameReopensRatherThanOnlyRecordingIt() async {
        let host = RecordingHost()
        host.outcome = .switchedWithoutAFrame
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 30_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        await coordinator.commit()
        XCTAssertEqual(host.interruptions.count, 1)
        XCTAssertEqual(
            host.fallbacks, [action],
            "measuring the freeze is not a substitute for ending it"
        )
    }
}

/// §5.3 — the two rules that were already here, held still by a test rather
/// than by reading the source.
///
/// Both are about a failure that resolves *after* the viewer has moved on. The
/// prepared path's failures take a MainActor hop before they reopen, and a
/// seek can land inside that hop; a fallback that reopened anyway would move
/// the film out from under the command that superseded it.
@MainActor
final class PreparedFallbackOwnershipTests: XCTestCase {
    private func settle() async {
        for _ in 0..<10 {
            try? await Task.sleep(nanoseconds: 20_000_000)
            await Task.yield()
        }
    }

    func testAnInPlaceFallbackRunsWhenItStillOwnsThePlayer() async {
        Caps.PreparedHandoffTelemetry.shared.reset()
        let controller = PlayerController()
        let model = AppModel()
        controller.start(
            model: model, itemId: 1, fileId: 1,
            startMs: 0, durationMs: 600_000, title: "Ownership"
        )
        controller.fallBackToInPlaceReplacement(preparedAction())
        await settle()
        XCTAssertEqual(
            Caps.PreparedHandoffTelemetry.shared.lastOutcome, "fell back",
            "nothing superseded it, so the viewer still gets their change"
        )
        controller.stop()
        Caps.PreparedHandoffTelemetry.shared.reset()
    }

    func testAnInPlaceFallbackStandsDownWhenANewerViewerActionOwnsThePlayer() async {
        Caps.PreparedHandoffTelemetry.shared.reset()
        let controller = PlayerController()
        let model = AppModel()
        controller.start(
            model: model, itemId: 1, fileId: 1,
            startMs: 0, durationMs: 600_000, title: "Ownership"
        )
        // The failure, and the viewer's seek landing inside its hop.
        controller.fallBackToInPlaceReplacement(preparedAction())
        controller.seek(toMs: 90_000)
        await settle()
        XCTAssertNil(
            Caps.PreparedHandoffTelemetry.shared.lastOutcome,
            "the seek owns the player; this fallback is about a destination nobody wants"
        )
        controller.stop()
        Caps.PreparedHandoffTelemetry.shared.reset()
    }
}

// MARK: - A refused commit frees everything it was holding (A2)

/// The commit is the one place the coordinator cannot be got out of from
/// outside: `.switching` makes `abandon`, `abandonWithoutFallback` and `offer`
/// all bail, and `shouldAskForPreparation` false. A commit that never returns
/// therefore costs the session every later quality change, not just this one —
/// which is why the alignment inside it is bounded, and why the bound's answer
/// is `refused`.
@MainActor
final class PreparedRefusedCommitTests: XCTestCase {
    func testARefusedCommitLeavesTheCoordinatorFreeForTheNextChange() async {
        let host = RecordingHost()
        host.outcome = .refused
        let coordinator = PreparedReplacementCoordinator(host: host)
        let action = preparedAction()
        coordinator.offer(action, filmPositionMs: 30_000)
        coordinator.successorIsMetadataReady()
        coordinator.successorIsBuffered(throughMs: 40_000)
        XCTAssertFalse(coordinator.shouldAskForPreparation, "one at a time, while it is live")
        await coordinator.commit()

        XCTAssertFalse(coordinator.hasActivePreparation)
        XCTAssertFalse(
            coordinator.ledger.isSwitching,
            "a commit that returns is what takes the coordinator out of .switching"
        )
        XCTAssertTrue(
            coordinator.shouldAskForPreparation,
            "every later quality change in this session would otherwise skip the prepared path"
        )
        XCTAssertEqual(
            host.fallbacks, [action],
            "the viewer's tap still gets its change, in place"
        )
        XCTAssertEqual(
            coordinator.pendingAcknowledgement?.state, .aborted,
            "and the server's one preparation slot is freed rather than held to its deadline"
        )
        XCTAssertEqual(host.alive, 0)

        let next = preparedAction(actionId: "4e5f6a7b-8c9d-4e0f-8a1b-2c3d4e5f6a7b")
        XCTAssertEqual(
            coordinator.offer(next, filmPositionMs: 45_000), .build(next),
            "and the next change can be prepared"
        )
    }
}
