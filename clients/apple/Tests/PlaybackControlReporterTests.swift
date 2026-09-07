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
    private var _intentGeneration = 0
    private var _sourceRevision = 0
    private var _available = true
    private var _owner = PlaybackControlCaptureOwner(lifecycleId: clientId, attachmentGeneration: 1)
    private var _afterExchange: (@Sendable () -> Void)?
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

    func setSnapshot(_ value: PlaybackControlSnapshot) { lock.withLock { _snapshot = value; _sourceRevision += 1 } }
    func setCaptureSource(_ value: PlaybackControlSnapshot, intent: Int, attachment: Int) {
        lock.withLock {
            _snapshot = value
            _sourceRevision += 1
            _intentGeneration = intent
            _owner = PlaybackControlCaptureOwner(lifecycleId: clientId, attachmentGeneration: attachment)
        }
    }
    func advance(_ ms: Int) { lock.withLock { _clock += ms } }
    func setAvailable(_ value: Bool) { lock.withLock { _available = value } }
    func afterNextExchange(_ body: @escaping @Sendable () -> Void) { lock.withLock { _afterExchange = body } }
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
    var takeCapture: @Sendable () -> PlaybackControlCapture? {
        { [self] in lock.withLock {
            guard _available else { return nil }
            return PlaybackControlCapture(snapshot: _snapshot, intentGeneration: _intentGeneration,
                                   owner: _owner, sourceRevision: _sourceRevision)
        } }
    }
    var onExchange: @Sendable (PlaybackControlReporter.Exchange) -> Void {
        { [self] exchange in
            let after = lock.withLock {
                _exchanges.append(exchange)
                let after = _afterExchange; _afterExchange = nil
                return after
            }
            after?()
            gate.signal()
        }
    }

    /// Wait for `count` exchange outcomes rather than sleeping a guessed
    /// interval, so a slow machine cannot turn a passing test into a flake.
    ///
    /// The gate counts every exchange the reporter has ever made, so this is
    /// only sound for "at least this many have happened by now". A test that
    /// needs "something happened *after* this moment" must poll a predicate
    /// instead — see `waitUntil`.
    func awaitExchanges(_ count: Int, timeout: TimeInterval = 5) -> Bool {
        for _ in 0..<count where gate.wait(timeout: .now() + timeout) == .timedOut {
            return false
        }
        return true
    }

    /// Poll until the reporter has done something a test can name, rather than
    /// counting exchanges it did not ask for. Bounded, so a genuine failure
    /// fails rather than hanging.
    func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            usleep(5_000)
        }
        return condition()
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
        owner: PlaybackControlCaptureOwner(lifecycleId: clientId, attachmentGeneration: 1),
        capture: harness.takeCapture,
        send: harness.send,
        sleep: harness.sleep,
        now: harness.now,
        onExchange: harness.onExchange
    )
}

// MARK: - Tests

final class PlaybackControlReporterTests: XCTestCase {

    func testNewIntentResumesOnlyAnIntentOwnedTerminalStop() async throws {
        for transition in ["callback", "ordinary", "urgent", "explicit", "end", "protocol"] {
            let harness = Harness()
            var newer = snapshot()
            newer.positionMs = 9_000; newer.bufferedThroughMs = 19_000
            let next = newer
            if transition == "callback" {
                harness.afterNextExchange { harness.setCaptureSource(next, intent: 1, attachment: 1) }
            }
            if transition == "end" {
                var ended = snapshot(); ended.demand = .end; harness.setSnapshot(ended)
            }
            if transition == "protocol" { harness.enqueue([.failure(ControlProtocolError(reason: "body"))]) }
            else {
                harness.enqueue([.success(ControlResponse(proto: PlaybackControl.protocolName,
                    generation: bootstrap().generation, controlEpoch: 7, acceptedSequence: 1,
                    action: ControlAction(type: "terminal", code: "unsupported", message: "A")))])
            }
            let reporter = try XCTUnwrap(makeReporter(harness))
            await reporter.start()
            XCTAssertTrue(harness.awaitExchanges(1))
            if transition != "callback" {
                var stopped = await reporter.stopped
                XCTAssertTrue(stopped)
                await reporter.notify()
                stopped = await reporter.stopped
                XCTAssertTrue(stopped, "same intent cannot resume its own terminal stop")
                if transition == "explicit" { await reporter.stop() }
                harness.setCaptureSource(next, intent: 1, attachment: 1)
                if transition == "ordinary" { await reporter.notify() }
                else { await reporter.notifyUrgently() }
            }
            let permanent = ["explicit", "end", "protocol"].contains(transition)
            if !permanent { XCTAssertTrue(harness.waitUntil { harness.requests.count >= 2 }, transition) }
            let stopped = await reporter.stopped
            XCTAssertEqual(stopped, permanent, transition)
            await reporter.stop()
            if permanent { XCTAssertEqual(harness.requests.count, 1, transition) }
            else { XCTAssertEqual(harness.requests[1].positionMs, 9_000, transition) }
        }
    }

    func testOwnerResetDuringClearedSourceWaitsForItsOwnAttachmentCapture() async throws {
        for foreignOwner in [false, true] {
            let harness = Harness()
            let reporter = try XCTUnwrap(makeReporter(harness))
            let nextGeneration = "44444444-4444-4444-8444-444444444444"
            harness.afterNextExchange { harness.setAvailable(false) }
            harness.enqueue([.failure(ControlTransportError(status: 409, code: "owner_changed",
                generation: nextGeneration, controlEpoch: 8))])
            await reporter.start()
            XCTAssertTrue(harness.awaitExchanges(1))
            let stopped = await reporter.stopped
            XCTAssertFalse(stopped, "a source publication gap is not a terminal protocol error")
            XCTAssertEqual(harness.requests.count, 1)
            var newer = snapshot()
            newer.positionMs = 9_000; newer.bufferedThroughMs = 19_000
            harness.setCaptureSource(newer, intent: 1, attachment: foreignOwner ? 2 : 1)
            harness.setAvailable(true)
            let floor = await reporter.notifyUrgently()
            if foreignOwner {
                XCTAssertNil(floor)
                XCTAssertEqual(harness.requests.count, 1, "old reporter cannot borrow B's body")
            } else {
                XCTAssertNotNil(floor)
                XCTAssertTrue(harness.waitUntil { harness.requests.count >= 2 })
                XCTAssertEqual(harness.requests[1].generation, nextGeneration)
                XCTAssertEqual(harness.requests[1].controlEpoch, 8)
                XCTAssertEqual(harness.requests[1].sequence, 1)
                XCTAssertEqual(harness.requests[1].positionMs, 9_000)
            }
            await reporter.stop()
        }
    }

    func testReversedActorEnqueueUsesNewestSourceEvenWhenIntentIsUnchanged() async throws {
        for sameIntent in [false, true] {
            let harness = Harness()
            let reporter = try XCTUnwrap(makeReporter(harness))
            let older = try XCTUnwrap(harness.takeCapture())
            var newer = snapshot()
            newer.positionMs = 9_000; newer.bufferedThroughMs = 19_000
            harness.setCaptureSource(newer, intent: sameIntent ? 0 : 1, attachment: 1)
            let captured = try XCTUnwrap(harness.takeCapture())
            await reporter.notify(captured)
            await reporter.notify(older) // delayed actor hop from the earlier source turn
            await reporter.start()
            XCTAssertTrue(harness.awaitExchanges(1))
            await reporter.stop()
            XCTAssertEqual(harness.requests.first?.positionMs, 9_000)
            XCTAssertEqual(harness.exchanges.first?.capture, captured)
        }
    }

    func testClearedSourceAndNewAttachmentCannotAdmitAnOldQueuedCapture() async throws {
        for replaceOwner in [false, true] {
            let harness = Harness()
            let reporter = try XCTUnwrap(makeReporter(harness))
            let captured = try XCTUnwrap(harness.takeCapture())
            await reporter.notify(captured)
            if replaceOwner { harness.setCaptureSource(snapshot(), intent: 0, attachment: 2) }
            else { harness.setAvailable(false) }
            await reporter.start()
            for _ in 0..<10 { await Task.yield() }
            let floor = await reporter.notifyUrgently(captured)
            await reporter.stop()
            XCTAssertNil(floor)
            XCTAssertTrue(harness.requests.isEmpty, "neither queued work nor cadence may cross the source owner")
        }
    }

    func testStartedRetryKeepsTheCapturedPayloadIntentAndAttachment() async throws {
        let harness = Harness()
        let reporter = try XCTUnwrap(makeReporter(harness))
        let captured = try XCTUnwrap(harness.takeCapture())
        var replacement = snapshot()
        replacement.positionMs = 9_000
        replacement.bufferedThroughMs = 19_000
        let newer = replacement
        harness.afterNextExchange { harness.setCaptureSource(newer, intent: 7, attachment: 1) }
        harness.enqueue([.failure(ControlTransportError(status: 429, code: "control_rate_limited"))])
        // Source changes after first failure, before the exact retry.
        await reporter.notify(captured)
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(2))
        await reporter.stop()
        XCTAssertEqual(harness.requests[0], harness.requests[1])
        XCTAssertEqual(harness.exchanges[0].capture, captured)
        XCTAssertEqual(harness.exchanges[1].capture, captured)
        XCTAssertNotEqual(captured.intentGeneration, harness.takeCapture()?.intentGeneration)
    }

    func testTerminalForOldAttachmentDoesNotStopTheReusedIntentNumber() async throws {
        let harness = Harness()
        let reporter = try XCTUnwrap(makeReporter(harness))
        let captured = try XCTUnwrap(harness.takeCapture())
        harness.afterNextExchange {
            harness.setCaptureSource(snapshot(), intent: captured.intentGeneration, attachment: 2)
        }
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName, generation: bootstrap().generation,
            controlEpoch: 7, acceptedSequence: 1,
            action: ControlAction(type: "terminal", code: "unsupported", message: "old attachment")
        ))])
        await reporter.notify(captured)
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped)
        await reporter.stop()
        XCTAssertEqual(harness.exchanges[0].capture, captured)
        XCTAssertEqual(harness.requests.count, 1, "an old reporter cannot send the replacement's capture")
    }
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
                owner: PlaybackControlCaptureOwner(lifecycleId: clientId, attachmentGeneration: 1),
                capture: harness.takeCapture,
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
            observation: nil,
            supportedActions: PlaybackControl.supportedActions
        )))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        await reporter.stop()

        let request = try XCTUnwrap(harness.requests.first)
        XCTAssertEqual(request.proto, PlaybackControl.protocolName)
        XCTAssertEqual(
            request.supportedActions,
            ["hold", "retry_resource", "terminal"],
            "the server sends only actions this client has declared"
        )
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

    func testUrgentIntentReturnsTheNextRequestOrderingFloor() async throws {
        let harness = Harness()
        harness.holdExchanges()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.waitUntil { harness.requests.count == 1 })

        harness.setSnapshot(snapshot(position: 9_000, render: .seeking))
        let floor = await reporter.notifyUrgently()

        XCTAssertEqual(floor, 2)
        await reporter.stop()
    }

    func testUnchangedCapabilitiesAreSentOnceAndAChangeResendsThem() async throws {
        let harness = Harness()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.waitUntil { harness.requests.count >= 4 })
        let beforeChange = harness.requests
        harness.setSnapshot(snapshot(position: 3_000, maxHeight: 1_080))
        await reporter.notify()
        let resent = harness.waitUntil {
            harness.requests.contains { $0.capabilities == capabilities(maxHeight: 1_080) }
        }
        await reporter.stop()

        XCTAssertEqual(
            beforeChange.first?.capabilities, capabilities(),
            "the first exchange of a generation must carry them"
        )
        XCTAssertTrue(
            beforeChange.dropFirst().allSatisfy { $0.capabilities == nil },
            "unchanged capabilities are already on the server"
        )
        XCTAssertTrue(resent, "a capability change must be resent")
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

    func testAnUndeclaredActionIsTerminalRatherThanObeyed() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "prepare_replacement")
        ))
    }

    func testATerminalVerdictEndsReportingWithoutAProtocolError() async throws {
        let harness = Harness()
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "terminal", code: "unsupported", message: "no")
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped, "a terminal verdict ends reporting")
        XCTAssertNil(
            harness.exchanges.first?.failure,
            "a terminal verdict is an answer, not a protocol error"
        )
    }

    func testAVerdictMissingTheFieldItWouldBeActedOnIsTerminal() async throws {
        // Inside the declared vocabulary but unusable. Worse than an unknown
        // action, because this one would be acted on.
        for action in [
            ControlAction(type: "terminal", message: "no code"),
            ControlAction(type: "retry_resource", reason: "reader_failed"),
            ControlAction(type: "retry_resource", reason: "reader_failed", afterMs: 0),
            ControlAction(type: "retry_resource", reason: "reader_failed", afterMs: 60_001),
        ] {
            try await assertTerminal(response: ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: action
            ))
        }
    }

    func testARetryIsAnAnswerAndKeepsTheReporterRunning() async throws {
        let harness = Harness()
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "retry_resource", reason: "reader_failed", afterMs: 9_000)
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped, "a retry is not a reason to stop reporting")
        XCTAssertNil(harness.exchanges.first?.failure)
        await reporter.stop()
    }

    func testAHoldWithoutItsReasonIsTerminal() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "hold")
        ))
    }

    func testAHoldIsAnExplanationAndKeepsTheReporterRunning() async throws {
        // The point of declaring the action. The server is saying production
        // is deliberately not advancing; a client that stopped reporting there
        // would go silent for the rest of the film exactly when the server had
        // just explained itself.
        let harness = Harness()
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "hold", reason: "working_set")
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped, "a hold is not a reason to stop reporting")
        await reporter.stop()
    }

    func testAnUnrecognisedHoldReasonIsStillAHold() async throws {
        // A reason this client has never heard of is a newer server, not a
        // broken one. Refusing the exchange over one unknown word would be the
        // same silence by a different route.
        let harness = Harness()
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "hold", reason: "a_reason_from_next_year")
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        XCTAssertTrue(harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped)
        await reporter.stop()
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

    func testOriginalQualityHasAnExplicitWireRepresentation() throws {
        var value = snapshot()
        value.selection.quality = .original
        XCTAssertTrue(value.isValid)
        let json = try XCTUnwrap(
            String(data: try PlaybackControl.encoder.encode(value.selection), encoding: .utf8)
        )
        XCTAssertTrue(json.contains("\"mode\":\"original\""))
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
            "\"dropped_frames\":", "\"decoder_state\":", "\"supported_actions\":",
        ] {
            XCTAssertTrue(json.contains(key), "missing \(key)")
        }
        XCTAssertTrue(json.contains("\"mode\":\"manual\""))
        XCTAssertTrue(json.contains("\"height\":1080"))
        XCTAssertTrue(json.contains("\"dynamic_range\":\"dolby_vision\""))
        XCTAssertTrue(json.contains("\"render_state\":\"waiting\""))
        XCTAssertTrue(json.contains("\"demand\":\"hold\""))
        XCTAssertTrue(
            json.contains("\"supported_actions\":[\"hold\",\"retry_resource\",\"terminal\"]")
        )
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

    func testTheResponseDecodesTheServersFieldNamesAndConsumesSubtitleReadiness() throws {
        let json = """
        {"protocol":"plurx-playback-control-v1",
         "generation":"11111111-1111-4111-8111-111111111111",
         "control_epoch":7,"accepted_sequence":4,"server_time_unix_ms":1,
         "lease":{"state":"active","renew_after_ms":5000,"expires_at_unix_ms":2},
         "delivery":{"subtitle_readiness":"warming"},
         "effective_selection":{},"action":{"type":"none"}}
        """
        let decoded = try PlaybackControl.decoder.decode(ControlResponse.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.acceptedSequence, 4)
        XCTAssertEqual(decoded.delivery?.subtitleReadiness, "warming")
        XCTAssertEqual(decoded.action.type, "none")
    }

    func testSubtitleReadinessDecisionAndTransitionAreClosedAndSingleShot() {
        for (value, expected) in [
            ("ready", true),
            ("warming", false),
            ("unavailable", false),
            ("a_value_from_next_year", false),
            ("", false),
        ] {
            XCTAssertEqual(SubtitleReadinessDecision.meansReady(value), expected, value)
        }
        XCTAssertFalse(SubtitleReadinessDecision.meansReady(nil))

        let transition = SubtitleReadinessRetryState()
        XCTAssertFalse(transition.record(nil))
        XCTAssertFalse(transition.record("warming"))
        XCTAssertTrue(transition.record("ready"), "warming → ready retries once")
        XCTAssertFalse(transition.record("ready"), "control cadence cannot retry again")
    }
}
