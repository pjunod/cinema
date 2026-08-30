import Foundation
import XCTest
@testable import plurx

// MARK: - Fixtures

private func bootstrap(
    generation: String = "11111111-1111-4111-8111-111111111111",
    epoch: Int = 7
) -> ControlBootstrap {
    ControlBootstrap(
        proto: PlaybackControl.protocolName,
        url: "/api/v1/hls/session-1/control",
        generation: generation,
        controlEpoch: epoch,
        nextExchangeMs: 5_000,
        leaseTimeoutMs: 300_000
    )
}

private func capabilities(maxHeight: Int = 2_160) -> DynamicCapabilities {
    DynamicCapabilities(
        platform: "apple",
        maxHeight: maxHeight,
        codecs: [.hevc, .h264],
        dynamicRanges: [.dolbyVision, .hdr10, .sdr],
        dualPlayerPreparation: false
    )
}

private func snapshot(
    position: Int = 1_000,
    render: RenderState = .rendering,
    demand: PlaybackDemand = .active,
    maxHeight: Int = 2_160,
    observation: ClientObservation? = nil
) -> PlaybackControlSnapshot {
    PlaybackControlSnapshot(
        demand: demand,
        positionMs: position,
        bufferedFromMs: position,
        bufferedThroughMs: position + 10_000,
        playbackRate: 1,
        renderState: render,
        seekTargetMs: nil,
        observedDownloadBps: nil,
        selection: ClientSelection(
            quality: .auto,
            audioTrack: 0,
            subtitle: SubtitleSelection(mode: .off, track: nil),
            audioOffsetMs: 0,
            codec: .auto,
            dynamicRange: .auto
        ),
        capabilities: capabilities(maxHeight: maxHeight),
        observation: observation
    )
}

private func accept(_ request: ControlRequest) -> ControlResponse {
    ControlResponse(
        proto: PlaybackControl.protocolName,
        generation: request.generation,
        controlEpoch: request.controlEpoch,
        acceptedSequence: request.sequence,
        action: ControlAction(type: "none")
    )
}

private let clientId = "22222222-2222-4222-8222-222222222222"

/// Every moving part the reporter depends on, held still.
///
/// The clock does not advance on its own and pacing sleeps return at once, so
/// a test drives exchanges by resuming them one at a time rather than by
/// waiting real milliseconds. Deadline sleeps hang until cancelled — that is
/// what a healthy exchange looks like from the deadline arm's side, and the
/// one test that wants a timeout says so.
private final class Harness: @unchecked Sendable {
    private let lock = NSLock()
    private var _requests: [ControlRequest] = []
    private var _clock = 0
    private var _snapshot = snapshot()
    private var _outcomes: [Result<ControlResponse, Error>] = []
    private var _exchanges: [PlaybackControlReporter.Exchange] = []
    private var _deadlineFires = false
    private var _sleeps: [(Int, PlaybackControlReporter.SleepKind)] = []
    private var _holds = false
    private let gate = DispatchSemaphore(value: 0)

    var requests: [ControlRequest] { lock.withLock { _requests } }
    var exchanges: [PlaybackControlReporter.Exchange] { lock.withLock { _exchanges } }
    var pacingSleeps: [Int] {
        lock.withLock { _sleeps.filter { $0.1 == .pacing }.map(\.0) }
    }

    func setSnapshot(_ value: PlaybackControlSnapshot) { lock.withLock { _snapshot = value } }
    func advance(_ ms: Int) { lock.withLock { _clock += ms } }
    func fireDeadlines() { lock.withLock { _deadlineFires = true } }

    /// Queue what the next exchanges answer, oldest first. Anything beyond
    /// the queue is accepted, so a test states only the outcomes it cares
    /// about and the reporter's own cadence does not run it out of answers.
    func enqueue(_ outcomes: [Result<ControlResponse, Error>]) {
        lock.withLock { _outcomes.append(contentsOf: outcomes) }
    }

    /// Keep every exchange in flight instead of answering it. This is how a
    /// test observes what the reporter does while it is already talking.
    func holdExchanges() { lock.withLock { _holds = true } }

    var send: PlaybackControlReporter.Send {
        { [self] _, request in
            var outcome: Result<ControlResponse, Error>?
            var hold = false
            lock.withLock {
                _requests.append(request)
                hold = _holds
                if !_outcomes.isEmpty { outcome = _outcomes.removeFirst() }
            }
            if hold {
                try await Task.sleep(nanoseconds: 60_000_000_000)
                throw CancellationError()
            }
            return try (outcome ?? .success(accept(request))).get()
        }
    }

    var sleep: PlaybackControlReporter.Sleep {
        { [self] milliseconds, kind in
            lock.withLock { _sleeps.append((milliseconds, kind)) }
            switch kind {
            case .pacing:
                // Virtual time: a pacing sleep costs no wall clock and moves
                // the clock the reporter reads, so backoff and cadence are
                // exercised exactly without a test waiting out a real second.
                lock.withLock { _clock += milliseconds }
                await Task.yield()
            case .deadline:
                let fires = lock.withLock { _deadlineFires }
                if fires { return }
                try await Task.sleep(nanoseconds: 60_000_000_000)
            }
        }
    }

    var now: @Sendable () -> Int { { [self] in lock.withLock { _clock } } }
    var takeSnapshot: @Sendable () -> PlaybackControlSnapshot? {
        { [self] in lock.withLock { _snapshot } }
    }
    var onExchange: @Sendable (PlaybackControlReporter.Exchange) -> Void {
        { [self] exchange in
            lock.withLock { _exchanges.append(exchange) }
            gate.signal()
        }
    }

    /// Wait for `count` exchange outcomes rather than sleeping a guessed
    /// interval, so a slow machine cannot turn a passing test into a flake.
    func awaitExchanges(_ count: Int, timeout: TimeInterval = 5) -> Bool {
        for _ in 0..<count where gate.wait(timeout: .now() + timeout) == .timedOut {
            return false
        }
        return true
    }
}

private extension NSLock {
    func withLock<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}

private func makeReporter(
    _ harness: Harness,
    bootstrap value: ControlBootstrap = bootstrap()
) -> PlaybackControlReporter? {
    PlaybackControlReporter(
        bootstrap: value,
        clientInstanceId: clientId,
        snapshot: harness.takeSnapshot,
        send: harness.send,
        sleep: harness.sleep,
        now: harness.now,
        onExchange: harness.onExchange
    )
}

// MARK: - Tests

final class PlaybackControlReporterTests: XCTestCase {
    // MARK: bootstrap acceptance

    func testAValidBootstrapIsAccepted() {
        XCTAssertTrue(bootstrap().isValid)
    }

    func testAForeignProtocolIsRefused() {
        var value = bootstrap()
        value.proto = "plurx-playback-control-v2"
        XCTAssertFalse(value.isValid)
    }

    func testOnlyASessionControlPathIsAccepted() {
        for url in [
            "/api/v1/hls/session-1/control",
            "/api/v1/hls/9f0c/control",
        ] {
            XCTAssertTrue(ControlBootstrap.isSessionControlPath(url), url)
        }
        for url in [
            "https://elsewhere.example/api/v1/hls/session-1/control",
            "//elsewhere.example/api/v1/hls/session-1/control",
            "/api/v1/hls/session-1/status",
            "/api/v1/hls/session-1/control/extra",
            "/api/v1/hls//control",
            "/api/v1/hls/../control",
            "/api/v2/hls/session-1/control",
            "control",
        ] {
            XCTAssertFalse(ControlBootstrap.isSessionControlPath(url), url)
        }
    }

    func testANonUUIDGenerationIsRefused() {
        var value = bootstrap()
        value.generation = "session-1"
        XCTAssertFalse(value.isValid)
    }

    func testAnExchangeCadenceOutsideTheProtocolBoundsIsRefused() {
        var tooFast = bootstrap()
        tooFast.nextExchangeMs = 100
        XCTAssertFalse(tooFast.isValid)
        var tooSlow = bootstrap()
        tooSlow.nextExchangeMs = 60_001
        XCTAssertFalse(tooSlow.isValid)
    }

    func testALeaseShorterThanTheExchangeCadenceIsRefused() {
        var value = bootstrap()
        value.leaseTimeoutMs = 1_000
        XCTAssertFalse(value.isValid)
    }

    func testAReporterRefusesAnInvalidBootstrapOrIdentity() {
        let harness = Harness()
        var bad = bootstrap()
        bad.controlEpoch = 0
        XCTAssertNil(makeReporter(harness, bootstrap: bad))
        XCTAssertNil(
            PlaybackControlReporter(
                bootstrap: bootstrap(),
                clientInstanceId: "not-a-uuid",
                snapshot: harness.takeSnapshot,
                send: harness.send,
                sleep: harness.sleep,
                now: harness.now
            )
        )
    }

    // MARK: the exchange itself

    func testTheFirstRequestCarriesTheWholeSnapshotAndItsCapabilities() async throws {
        let harness = Harness()
        harness.enqueue([.success(accept(ControlRequest(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            clientInstanceId: clientId,
            sequence: 1,
            demand: .active,
            positionMs: 1_000,
            bufferedFromMs: 1_000,
            bufferedThroughMs: 11_000,
            playbackRate: 1,
            renderState: .rendering,
            seekTargetMs: nil,
            observedDownloadBps: nil,
            selection: snapshot().selection,
            capabilities: capabilities(),
            observation: nil
        )))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        await reporter.stop()

        let request = try XCTUnwrap(harness.requests.first)
        XCTAssertEqual(request.proto, PlaybackControl.protocolName)
        XCTAssertEqual(request.sequence, 1)
        XCTAssertEqual(request.generation, bootstrap().generation)
        XCTAssertEqual(request.controlEpoch, 7)
        XCTAssertEqual(request.clientInstanceId, clientId)
        XCTAssertEqual(request.demand, .active)
        XCTAssertEqual(request.renderState, .rendering)
        XCTAssertEqual(request.positionMs, 1_000)
        XCTAssertEqual(request.bufferedThroughMs, 11_000)
        XCTAssertEqual(request.capabilities, capabilities())
        // The first exchange's own answer, not the reporter's running total:
        // the pump is free to have completed another exchange between the
        // gate signalling and this assertion, and on a fast machine it does.
        XCTAssertEqual(harness.exchanges.first?.response?.acceptedSequence, 1)
    }

    func testUnchangedCapabilitiesAreSentOnceAndAChangeResendsThem() async throws {
        let harness = Harness()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(4))
        let beforeChange = harness.requests
        harness.setSnapshot(snapshot(position: 3_000, maxHeight: 1_080))
        await reporter.notify()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()

        XCTAssertEqual(
            beforeChange.first?.capabilities, capabilities(),
            "the first exchange of a generation must carry them"
        )
        XCTAssertTrue(
            beforeChange.dropFirst().allSatisfy { $0.capabilities == nil },
            "unchanged capabilities are already on the server"
        )
        let resent = harness.requests.first { $0.capabilities == capabilities(maxHeight: 1_080) }
        XCTAssertNotNil(resent, "a capability change must be resent")
    }

    func testSequencesAreMonotonicAndNeverReused() async throws {
        let harness = Harness()
        harness.enqueue((1...4).map { sequence in
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: sequence,
                action: ControlAction(type: "none")
            ))
        })
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        for position in [2_000, 3_000, 4_000] {
            XCTAssertTrue(harness.awaitExchanges(1))
            harness.setSnapshot(snapshot(position: position))
            await reporter.notify()
        }
        XCTAssertTrue(harness.awaitExchanges(1))
        await reporter.stop()

        let sequences = harness.requests.map(\.sequence)
        XCTAssertEqual(sequences, Array(1...sequences.count))
    }

    func testTheNewestSnapshotWinsWhileAnExchangeIsInFlight() async throws {
        let harness = Harness()
        harness.holdExchanges()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        try await Task.sleep(nanoseconds: 100_000_000)
        harness.setSnapshot(snapshot(position: 5_000))
        await reporter.notify()
        harness.setSnapshot(snapshot(position: 9_000))
        await reporter.notify()
        let status = await reporter.status()
        XCTAssertEqual(status["in_flight"], 1)
        XCTAssertEqual(status["pending"], 1, "one pending snapshot, not two")
        XCTAssertEqual(harness.requests.count, 1, "no second exchange overlaps the first")
        await reporter.stop()
    }

    func testAnEndDemandIsTheLastExchange() async throws {
        let harness = Harness()
        harness.setSnapshot(snapshot(render: .ended, demand: .end))
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "none")
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped)
        XCTAssertEqual(harness.requests.count, 1)
    }

    // MARK: refusing what it cannot bind

    func testAResponseForAnotherGenerationIsTerminal() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: "33333333-3333-4333-8333-333333333333",
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "none")
        ))
    }

    func testAResponseAcknowledgingAnotherSequenceIsTerminal() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 99,
            action: ControlAction(type: "none")
        ))
    }

    func testAnActionOtherThanNoneIsTerminalRatherThanObeyed() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "prepare_replacement")
        ))
    }

    private func assertTerminal(response: ControlResponse) async throws {
        let harness = Harness()
        harness.enqueue([.success(response)])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped, "an unbindable response stops the reporter")
        XCTAssertEqual(harness.requests.count, 1, "and it does not keep talking")
        XCTAssertNotNil(harness.exchanges.last?.failure)
    }

    // MARK: failure classification

    func testARetryableControlFailureReplaysTheExactRequest() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(status: 429, code: "control_rate_limited")),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()

        let requests = harness.requests
        XCTAssertGreaterThanOrEqual(requests.count, 2)
        XCTAssertEqual(requests[0].sequence, 1)
        XCTAssertEqual(requests[1].sequence, 1, "a refused exchange does not consume a sequence")
        XCTAssertEqual(requests[0], requests[1], "the replay is the same request, byte for byte")
    }

    func testATransportFailureWithNoStatusIsRetried() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(status: nil, code: nil)),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped)
        await reporter.stop()
        XCTAssertEqual(
            harness.requests.map(\.sequence).prefix(2), [1, 1],
            "the replay reuses the sequence the server never accepted"
        )
    }

    func testAnUnauthorizedExchangeStopsTheReporter() async throws {
        let harness = Harness()
        harness.enqueue([.failure(ControlTransportError(status: 403, code: "forbidden"))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped)
        XCTAssertEqual(harness.requests.count, 1)
    }

    func testARetryHonoursTheServersRetryAfter() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(
                status: 503, code: "control_unavailable", retryAfterMs: 4_000
            )),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()
        XCTAssertTrue(
            harness.pacingSleeps.contains(4_000),
            "the server asked for 4s and got 4s, not the 500ms default: \(harness.pacingSleeps)"
        )
    }

    func testARetryWithoutARetryAfterUsesTheControlBackoff() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(status: 425, code: "owner_transition")),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()
        XCTAssertTrue(harness.pacingSleeps.contains(500), "\(harness.pacingSleeps)")
    }

    func testAnAbsurdRetryAfterIsIgnoredInFavourOfTheDefault() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(
                status: 429, code: "control_rate_limited", retryAfterMs: 999_999
            )),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()
        XCTAssertFalse(harness.pacingSleeps.contains(999_999))
        XCTAssertTrue(harness.pacingSleeps.contains(500), "\(harness.pacingSleeps)")
    }

    func testAnExchangeThatNeverAnswersEndsAtTheDeadline() async throws {
        let harness = Harness()
        harness.holdExchanges()
        harness.fireDeadlines()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()
        XCTAssertEqual(harness.exchanges.first?.failure, "transport:408:exchange_deadline")
        XCTAssertGreaterThanOrEqual(
            harness.requests.count, 2,
            "the deadline frees the in-flight slot rather than stranding it"
        )
        XCTAssertEqual(
            harness.requests.map(\.sequence).prefix(2), [1, 1],
            "a timed-out exchange was never accepted, so it keeps its sequence"
        )
    }

    // MARK: owner change

    func testAnOwnerChangeAdoptsTheNewGenerationAndRestartsTheSequence() async throws {
        let newGeneration = "44444444-4444-4444-8444-444444444444"
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(
                status: 409,
                code: "owner_changed",
                generation: newGeneration,
                controlEpoch: 9
            )),
            .success(ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: newGeneration,
                controlEpoch: 9,
                acceptedSequence: 1,
                action: ControlAction(type: "none")
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()

        let requests = harness.requests
        XCTAssertGreaterThanOrEqual(requests.count, 2)
        XCTAssertEqual(requests[0].generation, bootstrap().generation)
        XCTAssertEqual(requests[1].generation, newGeneration)
        XCTAssertEqual(requests[1].controlEpoch, 9)
        XCTAssertEqual(requests[1].sequence, 1, "a new owner issued no sequence yet")
        XCTAssertEqual(
            requests[1].capabilities, capabilities(),
            "the new owner has never been told this player's capabilities"
        )
    }

    func testAnOwnerChangeNamingNoNewerOwnerIsNotAdopted() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(
                status: 409,
                code: "owner_changed",
                generation: bootstrap().generation,
                controlEpoch: 7
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped, "a 409 that names the current owner is not a handoff")
        let current = await reporter.bootstrap
        XCTAssertEqual(current.generation, bootstrap().generation)
        XCTAssertEqual(current.controlEpoch, 7)
    }

    func testAnOwnerChangeWithAMalformedGenerationIsNotAdopted() async throws {
        let harness = Harness()
        harness.enqueue([
            .failure(ControlTransportError(
                status: 409, code: "owner_changed", generation: "elsewhere", controlEpoch: 9
            )),
        ])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let current = await reporter.bootstrap
        XCTAssertEqual(current.generation, bootstrap().generation)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped)
    }

    // MARK: what a snapshot is allowed to say

    func testASnapshotWhoseRunwayPrecedesItsPositionIsRefused() {
        var value = snapshot()
        value.bufferedThroughMs = value.positionMs - 1
        XCTAssertFalse(value.isValid)
    }

    func testAManualQualityOutsideTheEncoderRangeIsRefused() {
        var value = snapshot()
        value.selection.quality = .manual(height: 4_320)
        XCTAssertFalse(value.isValid)
        value.selection.quality = .manual(height: 1_080)
        XCTAssertTrue(value.isValid)
    }

    func testCapabilitiesMustNameAtLeastOneCodecAndRange() {
        var value = snapshot()
        value.capabilities.codecs = []
        XCTAssertFalse(value.isValid)
    }

    func testAnInvalidSnapshotIsNotReported() async throws {
        let harness = Harness()
        var broken = snapshot()
        broken.bufferedThroughMs = -1
        harness.setSnapshot(broken)
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        try await Task.sleep(nanoseconds: 150_000_000)
        XCTAssertEqual(harness.requests.count, 0, "a player state the server would reject is not sent")
        await reporter.stop()
    }

    // MARK: observation bounds

    func testDetailWithoutACodeIsDropped() {
        let observation = ClientObservation(
            droppedFrames: nil, decoderState: nil, errorCode: nil, errorDetail: "went wrong"
        )
        XCTAssertNil(observation.bounded)
    }

    func testDetailIsFlattenedAndBounded() throws {
        let raw = String(repeating: "e", count: 900)
        let observation = ClientObservation(
            droppedFrames: 12,
            decoderState: .starved,
            errorCode: .decoder,
            errorDetail: "bad\r\nline\u{0}break " + raw
        )
        let bounded = try XCTUnwrap(observation.bounded)
        let detail = try XCTUnwrap(bounded.errorDetail)
        XCTAssertLessThanOrEqual(detail.utf8.count, PlaybackControl.maximumErrorDetailBytes)
        XCTAssertFalse(detail.contains("\r"))
        XCTAssertFalse(detail.contains("\n"))
        XCTAssertFalse(detail.contains("\u{0}"))
        XCTAssertTrue(detail.hasPrefix("bad  line break "))
        XCTAssertEqual(bounded.droppedFrames, 12)
        XCTAssertEqual(bounded.decoderState, .starved)
    }

    func testANegativeDroppedFrameCountIsDropped() {
        let observation = ClientObservation(
            droppedFrames: -1, decoderState: .ready, errorCode: nil, errorDetail: nil
        )
        XCTAssertEqual(observation.bounded?.droppedFrames, nil)
        XCTAssertEqual(observation.bounded?.decoderState, .ready)
    }

    // MARK: the wire, exactly

    func testTheRequestEncodesTheServersFieldNames() throws {
        let request = ControlRequest(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            clientInstanceId: clientId,
            sequence: 3,
            demand: .hold,
            positionMs: 12,
            bufferedFromMs: 12,
            bufferedThroughMs: 34,
            playbackRate: 0,
            renderState: .waiting,
            seekTargetMs: 56,
            observedDownloadBps: 78,
            selection: ClientSelection(
                quality: .manual(height: 1_080),
                audioTrack: 1,
                subtitle: SubtitleSelection(mode: .overlay, track: 2),
                audioOffsetMs: -40,
                codec: .hevc,
                dynamicRange: .dolbyVision
            ),
            capabilities: capabilities(),
            observation: ClientObservation(
                droppedFrames: 9, decoderState: .ready, errorCode: nil, errorDetail: nil
            )
        )
        let encoder = PlaybackControl.encoder
        let json = try XCTUnwrap(String(data: try encoder.encode(request), encoding: .utf8))
        for key in [
            "\"protocol\":", "\"control_epoch\":", "\"client_instance_id\":", "\"position_ms\":",
            "\"buffered_from_ms\":", "\"buffered_through_ms\":", "\"playback_rate\":",
            "\"render_state\":", "\"seek_target_ms\":", "\"observed_download_bps\":",
            "\"audio_track\":", "\"audio_offset_ms\":", "\"dynamic_range\":",
            "\"max_height\":", "\"dynamic_ranges\":", "\"dual_player_preparation\":",
            "\"dropped_frames\":", "\"decoder_state\":",
        ] {
            XCTAssertTrue(json.contains(key), "missing \(key)")
        }
        XCTAssertTrue(json.contains("\"mode\":\"manual\""))
        XCTAssertTrue(json.contains("\"height\":1080"))
        XCTAssertTrue(json.contains("\"dynamic_range\":\"dolby_vision\""))
        XCTAssertTrue(json.contains("\"render_state\":\"waiting\""))
        XCTAssertTrue(json.contains("\"demand\":\"hold\""))
        XCTAssertFalse(json.contains("\"proto\""), "the wire name is protocol, not proto")
    }

    func testTheBootstrapDecodesTheServersFieldNames() throws {
        let json = """
        {"protocol":"plurx-playback-control-v1",
         "url":"/api/v1/hls/abc/control",
         "generation":"11111111-1111-4111-8111-111111111111",
         "control_epoch":7,"next_exchange_ms":5000,"lease_timeout_ms":60000}
        """
        let decoded = try PlaybackControl.decoder.decode(ControlBootstrap.self, from: Data(json.utf8))
        XCTAssertTrue(decoded.isValid)
        XCTAssertEqual(decoded.controlEpoch, 7)
        XCTAssertEqual(decoded.nextExchangeMs, 5_000)
        XCTAssertEqual(decoded.leaseTimeoutMs, 60_000)
    }

    func testTheResponseDecodesTheServersFieldNamesAndIgnoresWhatM2DoesNotConsume() throws {
        let json = """
        {"protocol":"plurx-playback-control-v1",
         "generation":"11111111-1111-4111-8111-111111111111",
         "control_epoch":7,"accepted_sequence":4,"server_time_unix_ms":1,
         "lease":{"state":"active","renew_after_ms":5000,"expires_at_unix_ms":2},
         "delivery":{},"effective_selection":{},"action":{"type":"none"}}
        """
        let decoded = try PlaybackControl.decoder.decode(ControlResponse.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.acceptedSequence, 4)
        XCTAssertEqual(decoded.action.type, "none")
    }
}
