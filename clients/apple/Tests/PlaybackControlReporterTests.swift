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
        }
    }

    /// Wait for `count` exchange outcomes without blocking Swift's cooperative
    /// executor. The reporter pump is an actor task; blocking every parallel
    /// XCTest worker on a semaphore can starve that task until the assertion
    /// times out, then make direct request indexing crash the whole test host.
    func awaitExchanges(_ count: Int, timeout: TimeInterval = 5) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if lock.withLock({ _exchanges.count >= count }) { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
        }
        return lock.withLock { _exchanges.count >= count }
    }

    /// Poll until the reporter has done something a test can name, rather than
    /// counting exchanges it did not ask for. Bounded, so a genuine failure
    /// fails rather than hanging.
    func waitUntil(timeout: TimeInterval = 5, _ condition: () -> Bool) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if condition() { return true }
            try? await Task.sleep(nanoseconds: 5_000_000)
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

/// XCTest's assertion parameters are autoclosures and cannot contain `await`.
/// Evaluate the asynchronous observation first, then hand the value to XCTest.
private func assertAsyncResult(
    _ value: Bool,
    _ message: String = "",
    file: StaticString = #filePath,
    line: UInt = #line
) {
    XCTAssertTrue(value, message, file: file, line: line)
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
            assertAsyncResult(await harness.awaitExchanges(1))
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
            if !permanent { assertAsyncResult(await harness.waitUntil { harness.requests.count >= 2 }, transition) }
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
            assertAsyncResult(await harness.awaitExchanges(1))
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
                assertAsyncResult(await harness.waitUntil { harness.requests.count >= 2 })
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
            assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
        await reporter.stop()

        let request = try XCTUnwrap(harness.requests.first)
        XCTAssertEqual(request.proto, PlaybackControl.protocolName)
        XCTAssertEqual(
            request.supportedActions,
            ["hold", "retry_resource", "terminal", "prepare_replacement"],
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
        assertAsyncResult(await harness.waitUntil { harness.requests.count == 1 })

        harness.setSnapshot(snapshot(position: 9_000, render: .seeking))
        let floor = await reporter.notifyUrgently()

        XCTAssertEqual(floor, 2)
        await reporter.stop()
    }

    func testUnchangedCapabilitiesAreSentOnceAndAChangeResendsThem() async throws {
        let harness = Harness()
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        assertAsyncResult(await harness.waitUntil { harness.requests.count >= 4 })
        let beforeChange = harness.requests
        harness.setSnapshot(snapshot(position: 3_000, maxHeight: 1_080))
        await reporter.notify()
        let resent = await harness.waitUntil {
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
            assertAsyncResult(await harness.awaitExchanges(1))
            harness.setSnapshot(snapshot(position: position))
            await reporter.notify()
        }
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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

    /// The fixture is deliberately a type no server will ever send.
    ///
    /// It used to be the literal `prepare_replacement`, which was a correct
    /// test right up to the moment this client learned to prepare — and then
    /// would have gone on passing while its name and its intent became false,
    /// because the wire tag for that action is `prepare`, not the name it is
    /// declared under. The property being protected is that an unrecognised
    /// action is fatal, and that property has to be tested with something
    /// genuinely unrecognised.
    func testAnUndeclaredActionIsTerminalRatherThanObeyed() async throws {
        try await assertTerminal(response: ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(type: "conjure_replacement")
        ))
    }

    /// The declared name is not the tag, and neither spelling is optional.
    func testTheDeclaredNameAndTheWireTagAreTheTwoDifferentStringsTheServerUses() {
        XCTAssertEqual(PlaybackControl.prepareReplacementAction, "prepare_replacement")
        XCTAssertEqual(PlaybackControl.prepareActionType, "prepare")
        XCTAssertTrue(
            PlaybackControl.supportedActions.contains(PlaybackControl.prepareReplacementAction),
            "the server offers only actions named in supported_actions, by their declared name"
        )
        XCTAssertFalse(
            PlaybackControl.supportedActions.contains(PlaybackControl.prepareActionType),
            "declaring the tag instead of the name is never matched, and fails silently"
        )
    }

    /// The `prepare` arm is fatal, and it is the higher-consequence half.
    ///
    /// An action *outside* the vocabulary was always fatal; this one is inside
    /// it and would be acted on, so a half-parsed payload would build a second
    /// decode pipeline against an address nothing validated. Each of these is
    /// one field the server's own `prepared_payload_is_valid` requires.
    func testAMalformedPrepareStopsTheReporterRatherThanBeingActedOn() async throws {
        let sessionId = "0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d"
        func prepare(_ mutate: (inout ControlAction) -> Void) -> ControlResponse {
            var action = ControlAction(
                type: PlaybackControl.prepareActionType,
                actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
                sessionId: sessionId,
                playlistUrl: "/api/v1/hls/\(sessionId)/index.m3u8",
                mediaOriginMs: 0,
                effectiveSelection: EffectiveSelection(
                    qualityAuto: true, height: 1_080, audioTrack: nil, subtitleBurn: nil,
                    audioOffsetMs: 0, codec: "server_selected", dynamicRange: "sdr"
                )
            )
            mutate(&action)
            return ControlResponse(
                proto: PlaybackControl.protocolName,
                generation: bootstrap().generation,
                controlEpoch: 7,
                acceptedSequence: 1,
                action: action
            )
        }
        try await assertTerminal(response: prepare { $0.actionId = nil })
        try await assertTerminal(response: prepare { $0.effectiveSelection = nil })
        try await assertTerminal(response: prepare { $0.mediaOriginMs = -1 })
        try await assertTerminal(response: prepare {
            $0.playlistUrl = "http://elsewhere.example/api/v1/hls/\(sessionId)/index.m3u8"
        })
    }

    /// A whole `prepare` is accepted and reported, and nothing about it stops
    /// the reporter — the failure mode of the arm above, in the other
    /// direction.
    func testAWholePrepareIsAcceptedAndReachesTheExchange() async throws {
        let sessionId = "0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d"
        let harness = Harness()
        harness.enqueue([.success(ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: bootstrap().generation,
            controlEpoch: 7,
            acceptedSequence: 1,
            action: ControlAction(
                type: PlaybackControl.prepareActionType,
                actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
                sessionId: sessionId,
                playlistUrl: "/api/v1/hls/\(sessionId)/master.m3u8?native=1",
                mediaOriginMs: 600_000,
                effectiveSelection: EffectiveSelection(
                    qualityAuto: false, height: 720, audioTrack: nil, subtitleBurn: nil,
                    audioOffsetMs: 0, codec: "source", dynamicRange: nil
                )
            )
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        assertAsyncResult(await harness.awaitExchanges(1))
        let stopped = await reporter.stopped
        XCTAssertFalse(stopped, "a whole prepare is not a protocol violation")
        let action = try XCTUnwrap(harness.exchanges.first?.response?.action)
        let prepared = try XCTUnwrap(PreparedReplacementAction(action))
        XCTAssertEqual(prepared.mediaOriginMs, 600_000)
        XCTAssertEqual(prepared.effectiveSelection.height, 720)
        await reporter.stop()
    }

    /// §C12.6 and the stickiness rule, on the wire rather than in the ledger.
    ///
    /// The settlement rides the snapshot, so the only proof that matters is
    /// the encoded body of the request the reporter actually sent — and that
    /// it keeps riding every request until the exchange carrying it comes
    /// back, because the reporter coalesces and a settlement it dropped would
    /// hold the server's preparation slot for 330 seconds.
    func testASettlementRidesTheEncodedRequestUntilItsExchangeReturns() async throws {
        let harness = Harness()
        let owed = ActionAcknowledgement(
            actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
            state: .committed,
            committedMediaOriginMs: 600_000,
            firstFrameUnixMs: 1_788_000_000_000
        )
        var carrying = snapshot()
        carrying.acknowledgement = owed
        harness.setSnapshot(carrying)
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        assertAsyncResult(await harness.awaitExchanges(1))
        let body = try XCTUnwrap(
            try JSONSerialization.jsonObject(
                with: PlaybackControl.encoder.encode(try XCTUnwrap(harness.requests.first))
            ) as? [String: Any]
        )
        let sent = try XCTUnwrap(body["acknowledgement"] as? [String: Any])
        XCTAssertEqual(sent["action_id"] as? String, owed.actionId)
        XCTAssertEqual(sent["state"] as? String, "committed")
        XCTAssertEqual(sent["committed_media_origin_ms"] as? Int, 600_000)
        XCTAssertEqual(sent["first_frame_unix_ms"] as? Int, 1_788_000_000_000)
        XCTAssertNil(sent["buffered_through_ms"])

        // A newer position replaces the snapshot; the settlement is still owed
        // and still rides it.
        var moved = snapshot(position: 9_000)
        moved.acknowledgement = owed
        harness.setSnapshot(moved)
        await reporter.notify()
        assertAsyncResult(await harness.waitUntil { harness.requests.contains { $0.positionMs == 9_000 } })
        await reporter.stop()
        // Every request, not just the next one: the pump's own cadence sends
        // more than the test asks for, and a settlement that survived only the
        // exchange it was attached to would still be lost by the one after it.
        XCTAssertGreaterThan(harness.requests.count, 1)
        for request in harness.requests {
            XCTAssertEqual(
                request.acknowledgement, owed,
                "a position update replaces the snapshot; it must not replace the settlement"
            )
        }
    }

    /// A commit which collides with end is sent first, then end follows.
    func testAnEndingCommitIsSettledBeforeTheEndExchange() async throws {
        let harness = Harness()
        var ending = snapshot(demand: .end)
        let committed = ActionAcknowledgement(
            actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
            state: .committed,
            committedMediaOriginMs: 0,
            firstFrameUnixMs: 1_788_000_000_000
        )
        ending.acknowledgement = committed
        // The first send disappears. The commit must retry identically before
        // the end exchange is allowed to follow.
        harness.enqueue([.failure(ControlTransportError(status: nil, code: nil))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.finishAndWait(PlaybackControlCapture(
            snapshot: ending,
            intentGeneration: 0,
            owner: PlaybackControlCaptureOwner(
                lifecycleId: clientId, attachmentGeneration: 1
            ),
            sourceRevision: 1
        ))
        let requests = harness.requests
        let finalStatus = await reporter.status()
        XCTAssertGreaterThanOrEqual(
            requests.count,
            3,
            "finalization produced \(requests.count) request(s): \(requests.map(\.demand)); "
                + "status: \(finalStatus); pacing: \(harness.pacingSleeps)"
        )
        guard requests.count >= 3 else { return }
        XCTAssertEqual(requests[0].demand, .active)
        XCTAssertGreaterThanOrEqual(
            requests[0].playbackRate,
            PlaybackControlMapping.minimumActiveRate
        )
        XCTAssertEqual(requests[0].acknowledgement, committed)
        XCTAssertEqual(requests[1], requests[0], "a dropped commit retries exactly")
        XCTAssertEqual(requests[2].demand, .end)
        XCTAssertNil(requests[2].acknowledgement)
        // An abort may end the session in the same breath.
        let harness2 = Harness()
        var aborting = snapshot(demand: .end)
        let aborted = ActionAcknowledgement(
            actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01", state: .aborted
        )
        aborting.acknowledgement = aborted
        harness2.setSnapshot(aborting)
        let reporter2 = try XCTUnwrap(makeReporter(harness2))
        await reporter2.start()
        assertAsyncResult(await harness2.awaitExchanges(1))
        XCTAssertEqual(harness2.requests.first?.acknowledgement, aborted)
        await reporter2.stop()
    }

    func testFinalizationStopsAtItsDeadlineDuringPersistentOutage() async throws {
        let harness = Harness()
        var ending = snapshot(demand: .end)
        ending.acknowledgement = ActionAcknowledgement(
            actionId: "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01",
            state: .committed,
            committedMediaOriginMs: 0,
            firstFrameUnixMs: 1_788_000_000_000
        )
        let failuresBeyondDeadline = 32
        harness.enqueue((0..<failuresBeyondDeadline).map { _ in
            .failure(ControlTransportError(status: nil, code: nil))
        })
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.finish(PlaybackControlCapture(
            snapshot: ending,
            intentGeneration: 0,
            owner: PlaybackControlCaptureOwner(
                lifecycleId: clientId, attachmentGeneration: 1
            ),
            sourceRevision: 1
        ))
        assertAsyncResult(await harness.waitUntil { !harness.requests.isEmpty })
        assertAsyncResult(await harness.waitUntil {
            harness.pacingSleeps.reduce(0, +)
                >= PlaybackControlReporter.finalizationDeadlineMs
        })
        let status = await reporter.status()
        XCTAssertEqual(status["stopped"], 1, "teardown may not orphan an indefinitely retrying actor")
        XCTAssertEqual(status["in_flight"], 0)
        XCTAssertEqual(status["pending"], 0)
        XCTAssertEqual(status["retrying"], 0)
        XCTAssertLessThan(
            harness.requests.count,
            failuresBeyondDeadline,
            "the bounded finisher stops retrying"
        )
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(1))
        try await Task.sleep(nanoseconds: 150_000_000)
        let stopped = await reporter.stopped
        XCTAssertTrue(stopped)
        XCTAssertEqual(harness.requests.count, 1)
    }

    func testPauseGraceExpiryStopsWithoutReopeningOrRetrying() async throws {
        let harness = Harness()
        harness.enqueue([.failure(ControlTransportError(
            status: 410, code: "pause_grace_expired"
        ))])
        let reporter = try XCTUnwrap(makeReporter(harness))
        await reporter.start()
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(2))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
        assertAsyncResult(await harness.awaitExchanges(1))
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
            json.contains(
                "\"supported_actions\":[\"hold\",\"retry_resource\",\"terminal\","
                    + "\"prepare_replacement\"]"
            )
        )
        XCTAssertFalse(
            json.contains("\"acknowledgement\""),
            "a request that settles nothing omits the key rather than sending null"
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
         "effective_selection":{"quality_auto":true,"height":1080,"audio_track":null,
           "subtitle_burn":null,"audio_offset_ms":0,"codec":"server_selected",
           "dynamic_range":"sdr"},
         "action":{"type":"none"}}
        """
        let decoded = try PlaybackControl.decoder.decode(ControlResponse.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.acceptedSequence, 4)
        XCTAssertEqual(decoded.delivery?.subtitleReadiness, "warming")
        XCTAssertEqual(decoded.action.type, "none")
        // The fixture used to carry `"effective_selection":{}` because nothing
        // read it. It is a whole object on the wire and this client now
        // compares it against a successor's, so an empty one is not a
        // placeholder any more — it is a body the decoder rejects.
        XCTAssertEqual(decoded.effectiveSelection?.height, 1_080)
        XCTAssertEqual(decoded.effectiveSelection?.codec, "server_selected")
        XCTAssertEqual(decoded.effectiveSelection?.qualityAuto, true)
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

final class DisplayAwareAutoEvidenceTests: XCTestCase {
    func testA05DecoderAcknowledgementRequiresExactEventAndOriginalAttachmentBudget() {
        let event = "12345678-1234-1234-1234-123456789abc"
        XCTAssertTrue(autoNegativeLinkAcknowledgement(receipt: event, values: [event], status: 204, sameEndpoint: true, remainingMs: 250))
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: event, values: [], status: 204, sameEndpoint: true, remainingMs: 250))
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: event, values: [event, event], status: 204, sameEndpoint: true, remainingMs: 250))
        XCTAssertTrue(autoDecoderAcknowledgementCurrent(accepted: true, observedAtMs: 100, nowMs: 349, remainingMs: 250, sameAttachment: true))
        for (accepted, now, remaining, same) in [(false, 101, 250, true), (true, 350, 250, true), (true, 99, 250, true), (true, 101, 0, true), (true, 101, 250, false), (true, 120, 20, true)] {
            XCTAssertFalse(autoDecoderAcknowledgementCurrent(accepted: accepted, observedAtMs: 100, nowMs: now, remainingMs: remaining, sameAttachment: same))
        }
    }
    func testA05NegativeAcknowledgementKeepsExactNonceAttachmentAndOriginalDeadline() {
        let nonce = "12345678-1234-1234-1234-123456789abc"
        XCTAssertTrue(autoNegativeLinkAcknowledgement(receipt: nonce, values: [nonce], status: 204, sameEndpoint: true, remainingMs: 1))
        for values in [[], [nonce, nonce], [nonce + "," + nonce], ["other"], [nonce.uppercased()]] {
            XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: nonce, values: values, status: 204, sameEndpoint: true, remainingMs: 1))
        }
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: "malformed", values: ["malformed"], status: 204, sameEndpoint: true, remainingMs: 1))
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: nonce, values: [nonce], status: 200, sameEndpoint: true, remainingMs: 1))
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: nonce, values: [nonce], status: 204, sameEndpoint: false, remainingMs: 1))
        XCTAssertFalse(autoNegativeLinkAcknowledgement(receipt: nonce, values: [nonce], status: 204, sameEndpoint: true, remainingMs: 0))
        let item = NSObject(), other = NSObject()
        let ticket = AutoRecoveryCauseTicket(session: "incumbent", attachment: ObjectIdentifier(item), incumbentCandidate: "a", proposedCandidate: "b", cause: .link, observedAtMs: 100, linkReceipt: nonce)
        XCTAssertEqual(ticket.receiptForRequest(previous: "incumbent", candidate: "b", cause: "link"), nonce)
        XCTAssertNil(ticket.receiptForRequest(previous: "other", candidate: "b", cause: "link"))
        XCTAssertNil(ticket.receiptForRequest(previous: "incumbent", candidate: "c", cause: "link"))
        XCTAssertNil(ticket.receiptForRequest(previous: "incumbent", candidate: "b", cause: "decode"))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(other), candidate: "a", proposed: "b", nowMs: 200))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "a", proposed: "b", nowMs: 15_101))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "a", proposed: "b", nowMs: 99))
    }
    func testA05TypedRecoveryKeepsCandidateAttachmentAndSuppliedDecodeIntervals() {
        let item = NSObject(), replacement = NSObject()
        var window = AutoDecodePressureWindow()
        func sample(_ now: Int, _ position: Int, _ drops: Int, _ eligible: Bool = true) -> Bool {
            window.observe(attachment: ObjectIdentifier(item), candidate: "full-recipe-a",
                nowMs: now, positionMs: position, cumulativeDropped: drops, eligible: eligible)
        }
        XCTAssertFalse(sample(0, 0, 0))
        XCTAssertFalse(sample(2_000, 2_000, 3))
        XCTAssertTrue(sample(4_000, 4_000, 6))
        XCTAssertEqual(window.evidence?.elapsedMs, 4_000)
        XCTAssertEqual(window.evidence?.progressMs, 4_000)
        XCTAssertEqual(window.evidence?.droppedFrames, 6)
        XCTAssertFalse(sample(6_000, 4_000, 10), "a stagnant clock is not rendered progress")
        XCTAssertFalse(sample(8_000, 6_000, 13, false), "supply/pause/seek contamination discards the interval")
        XCTAssertFalse(sample(10_000, 8_000, 16))
        XCTAssertFalse(window.observe(attachment: ObjectIdentifier(replacement), candidate: "full-recipe-a",
            nowMs: 12_000, positionMs: 10_000, cumulativeDropped: 20, eligible: true))
        XCTAssertFalse(window.observe(attachment: ObjectIdentifier(replacement), candidate: "full-recipe-a",
            nowMs: 11_000, positionMs: 11_000, cumulativeDropped: 23, eligible: true), "rollback is Unknown")
        let ticket = AutoRecoveryCauseTicket(session: "incumbent", attachment: ObjectIdentifier(item),
            incumbentCandidate: "full-recipe-a", proposedCandidate: "full-recipe-b", cause: .decode, observedAtMs: 100)
        XCTAssertTrue(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "full-recipe-a", proposed: "full-recipe-b", nowMs: 15_100))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "full-recipe-a", proposed: "full-recipe-b", nowMs: 15_101))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(replacement), candidate: "full-recipe-a", proposed: "full-recipe-b", nowMs: 200))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "other-recipe", proposed: "full-recipe-b", nowMs: 200))
        XCTAssertFalse(ticket.isCurrent(session: "incumbent", attachment: ObjectIdentifier(item), candidate: "full-recipe-a", proposed: "full-recipe-b", nowMs: 99))
        XCTAssertEqual([AutoRecoveryCause.link, .encode, .decode, .hold, .authority].map(\.rawValue), ["link", "encode", "decode", "hold", "authority"])
    }
    func testA05BoundaryEpochFencesKeepCapturedOwnershipAndIndependentScopes() throws {
        func attempt(lifecycle: Int = 1, open: Int = 2, viewer: Int = 3,
                     seek: Int = 7, decision: Int = 4) -> Attempt {
            Attempt(lifecycle: lifecycle, open: open, viewerAction: viewer,
                initialDecision: decision, createRetry: 5, preparedAlignment: 6,
                seek: seek, pgsSelection: 8, pgsItem: 9, item: nil)
        }
        let captured = attempt()
        let expected: [(AttemptFence, Set<Attempt.Scope>)] = [
            (.seekIntentAfterOptionalBoundary, [.viewerAction, .seek]),
            (.autoBoundaryOwnerCurrent, [.lifecycle, .open, .viewerAction]),
            (.autoBoundarySeekCurrent, [.seek]),
            (.autoBoundaryResumeCurrent, [.lifecycle, .open, .viewerAction]),
            (.autoBoundaryCommitViewerCurrent, [.viewerAction]),
            (.autoBoundaryCommitOwnerCurrent, [.lifecycle, .open, .viewerAction]),
            (.autoBoundaryCommitSeekCurrent, [.seek]),
            (.autoResumeFallbackCurrent, [.lifecycle, .open, .viewerAction]),
            (.autoResumeCompletedViewerCurrent, [.viewerAction])
        ]
        for (fence, scopes) in expected {
            XCTAssertEqual(fence.scopes, scopes, fence.rawValue)
            XCTAssertTrue(captured.stillCurrent(attempt(), scopes: fence.scopes))
            XCTAssertTrue(captured.stillCurrent(attempt(decision: 99), scopes: fence.scopes),
                "unrelated initial decision must not widen a boundary fence")
            for (scope, changed) in [
                (Attempt.Scope.lifecycle, attempt(lifecycle: 99)),
                (.open, attempt(open: 99)), (.viewerAction, attempt(viewer: 99)),
                (.seek, attempt(seek: 99))
            ] {
                XCTAssertEqual(captured.stillCurrent(changed, scopes: fence.scopes), !scopes.contains(scope), fence.rawValue)
            }
        }
        let sourceURL = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().appendingPathComponent("Sources/PlayerController.swift")
        let source = try String(contentsOf: sourceURL, encoding: .utf8)
        XCTAssertTrue(source.contains("AutoBoundaryAttempt(attempt: snapshotAttempt(), resumeIdentity:"))
        XCTAssertTrue(source.contains("AutoBoundaryResumeOwner(attempt: snapshotAttempt(),"))
        XCTAssertTrue(source.contains("let resumeIntentAttempt = snapshotAttempt()"))
        XCTAssertTrue(source.contains("attemptStillCurrent(boundary.attempt, fence: .autoBoundaryCommitOwnerCurrent)"))
        XCTAssertTrue(source.contains("AutoResumeIdentity(attempt) == owner.identity"))
        XCTAssertFalse(source.contains("lifecycleGeneration == boundary.lifecycle"))
        XCTAssertFalse(source.contains("owner.viewer == viewerActionEpoch"))
    }

    func testA05SeekCallsitesPreserveViewerAndAutomaticMarkerProvenance() throws {
        // Check the actual controller callers, not just the budget helper:
        // buttons and relative remote commands share skip(seconds:), whereas
        // the position-clock marker writer must not open a viewer trial.
        let sourceURL = URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Sources/PlayerController.swift")
        let source = try String(contentsOf: sourceURL, encoding: .utf8)
        func body(_ start: String, until end: String) throws -> String {
            let lower = try XCTUnwrap(source.range(of: start))
            let upper = try XCTUnwrap(source.range(of: end, range: lower.upperBound..<source.endIndex))
            return String(source[lower.upperBound..<upper.lowerBound])
        }
        let relative = try body("func skip(seconds: Double) {", until: "func skipActiveMarker()")
        XCTAssertTrue(relative.contains("seekState.relative("))
        XCTAssertTrue(relative.contains("by: Int(seconds * 1000)"))
        XCTAssertTrue(relative.contains("issueSeek(to: request.target, generation: request.generation, viewerBoundary: true)"))
        let automatic = try body("func autoSkipActiveMarkerIfNeeded() {", until: "func reportMarkerOffer")
        XCTAssertTrue(automatic.contains("beginSeek(toMs: marker.endMs, viewerOrigin: false)"))
        XCTAssertFalse(automatic.contains("seek(toMs:"))
        let absolute = try body("func seek(toMs requested: Int) {", until: "private func beginSeek")
        XCTAssertTrue(absolute.contains("beginSeek(toMs: requested, viewerOrigin: true)"))
        let forwarding = try body("private func beginSeek(toMs requested: Int, viewerOrigin: Bool) {", until: "private func issueSeek(")
        XCTAssertTrue(forwarding.contains("viewerBoundary: viewerOrigin"))
    }

    func testA05ViewerBoundaryRetainsOriginalBudgetAndRefusesRenewal() {
        let first = AutoViewerBoundaryBudget(enteredAtMs: 1_000, originalDeadlineMs: 16_000, nowMs: 1_500)!
        XCTAssertEqual(first.deadlineMs, 9_000)
        XCTAssertEqual(first.optionalDeadlineMs, 7_000)
        let coalesced = AutoViewerBoundaryBudget(enteredAtMs: 1_000, originalDeadlineMs: 16_000, nowMs: 6_999)!
        XCTAssertEqual(coalesced.optionalDeadlineMs, first.optionalDeadlineMs)
        XCTAssertNil(AutoViewerBoundaryBudget(enteredAtMs: 1_000, originalDeadlineMs: 16_000, nowMs: 7_000))
        XCTAssertNil(AutoViewerBoundaryBudget(enteredAtMs: 1_000, originalDeadlineMs: 16_000, nowMs: 999))
        XCTAssertNil(AutoViewerBoundaryBudget(enteredAtMs: Int.max, originalDeadlineMs: Int.max, nowMs: Int.max))
        let earlierExpiry = AutoViewerBoundaryBudget(enteredAtMs: 1_000, originalDeadlineMs: 5_000, nowMs: 2_000)!
        XCTAssertEqual(earlierExpiry.deadlineMs, 5_000)
        XCTAssertEqual(earlierExpiry.optionalDeadlineMs, 3_000)
        // Quiet/cliff windows are still meaningful for ordinary mid-play, but
        // observing an expired original EOF cannot renew the cliff timestamp.
        let item = NSObject()
        var window = AutoUpgradeEvidenceWindow()
        window.bind(attachment: ObjectIdentifier(item), attempt: "viewer", nowMs: 0)
        window.cliff(completedAtMs: 1_000, nowMs: 2_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 90_999))
        window.cliff(completedAtMs: 1_000, nowMs: 91_000)
        XCTAssertTrue(window.allowsUpgrade(nowMs: 91_000))
    }
    private func candidate(_ height: Int, route: String = "encode", peak: UInt64? = 3_000_000, grade: String = "sdr") -> QualityCandidate {
        QualityCandidate(id: "0a7ba9bab6fbdd31bab5e5e362a3fac7", recipeDigest: Array(repeating: 0, count: 32),
            route: route, width: height * 16 / 9, height: height, targetHeight: height,
            averageBps: 8_000_000, peakBps: peak, grade: grade, decoderCompatible: true,
            completeCache: false, sustainable: false)
    }

    func testRuntimeDecoderSnapshotUsesStrictServerVocabulary() throws {
        let video = VideoCaps(codec: "hevc", profiles: ["main10"], maxHeight: 1_440, maxWidth: 2_560,
            maxFrameRate: DecoderFrameRate(numerator: 30, denominator: 1), present: ["pq", "sdr"], dvProfiles: [8])
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let wire = try encoder.encode(DecoderCapabilitySnapshot(revision: 1, video: [video]))
        let expected = Data(#"{"revision":1,"video":[{"codec":"hevc","profiles":["main10"],"available":true,"dynamic_ranges":["hdr10","sdr","dolby_vision"],"dv_profiles":[8],"max_width":2560,"max_height":1440,"max_frame_rate":{"numerator":30,"denominator":1}}]}"#.utf8)
        XCTAssertEqual(try JSONSerialization.jsonObject(with: wire) as? NSDictionary,
                       try JSONSerialization.jsonObject(with: expected) as? NSDictionary)
        let empty = VideoCaps(codec: "h264", profiles: nil, maxHeight: nil, present: ["sdr"], dvProfiles: nil)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(DecoderCapabilitySnapshot(revision: 2, video: [empty]))) as? [String: Any])
        let entry = try XCTUnwrap((object["video"] as? [[String: Any]])?.first)
        XCTAssertEqual(entry["profiles"] as? [String], [])
        XCTAssertEqual(entry["dv_profiles"] as? [Int], [])
        XCTAssertNil(entry["present"])
        XCTAssertEqual(DecoderCapabilitySnapshot(revision: 3, video: Array(repeating: video, count: 20)).video.count, 16)
    }

    func testCurrentDisplayOptimumDoesNotUpgradeAndBothAxesLimitEnlargement() {
        let current = candidate(1_080)
        XCTAssertEqual(autoPreferredDisplayCandidate([current, candidate(1_440)], neededWidth: 2_000, neededHeight: 1_125)?.height, 1_080)
        XCTAssertEqual(autoPreferredDisplayCandidate([current, candidate(1_440)], neededWidth: 2_400, neededHeight: 1_350)?.height, 1_440)
        XCTAssertFalse(autoDisplayFits(width: 1_920, height: 1_000, neededWidth: 2_000, neededHeight: 1_125))
        XCTAssertFalse(autoDisplayFits(width: 1_800, height: 1_080, neededWidth: 2_000, neededHeight: 1_125))
    }

    func testColdOriginalCanRecoverToUnprovedCompatibleEncodeWithinLinkBudget() {
        let original = candidate(2_160, route: "remux", peak: nil)
        let lower = candidate(1_080)
        XCTAssertEqual(autoRecoveryCandidate([original, lower], current: original, rejected: [], linkCeiling: 4_000_000)?.height, 1_080)
        XCTAssertNil(autoRecoveryCandidate([lower], current: original, rejected: [], linkCeiling: 2_000_000))
        XCTAssertNil(autoRecoveryCandidate([lower], current: original, rejected: [lower.id]))
        XCTAssertFalse(lower.sustainable)
        let hdrOriginal = candidate(2_160, route: "remux", peak: nil, grade: "hdr10")
        XCTAssertNil(autoRecoveryCandidate([lower], current: hdrOriginal, rejected: []))
        XCTAssertEqual(autoRecoveryCandidate([lower], current: hdrOriginal, rejected: [], decoderRecovery: true)?.height, 1_080)
    }

    func testOriginalAverageIsDownsideOnlyAndNeedsFreshUnpacedSessionEvidence() {
        let original = candidate(2_160, route: "remux", peak: nil)
        XCTAssertEqual(autoDownsideCostBps(original, sample: sample(), sessionId: "staged", nowMs: 2_000), 8_000_000)
        XCTAssertNil(original.peakBps)
        XCTAssertNil(autoDownsideCostBps(original, sample: sample(), sessionId: "other", nowMs: 2_000))
        XCTAssertNil(autoDownsideCostBps(original, sample: sample(paced: true), sessionId: "staged", nowMs: 2_000))
        XCTAssertNil(autoDownsideCostBps(original, sample: sample(), sessionId: "staged", nowMs: 12_000))
    }

    private func sample(_ segment: String = "a", bodySeconds: Double = 0.5,
                        mediaSeconds: Double? = 1, completedAt: Int = 1_000,
                        cached: Bool = false, paced: Bool? = false) -> PlayerController.AutoCompletedTransfer {
        PlayerController.AutoCompletedTransfer(bodyBytes: 100_000, bodyDurationSeconds: bodySeconds,
            completedAtMs: completedAt, origin: "https://example.invalid", networkLoad: true,
            fromLocalCache: cached, producerPaced: paced, statusCode: 200,
            segmentId: "/api/v1/hls/staged/seg\(segment).m4s", mediaDurationSeconds: mediaSeconds)
    }

    func testFreshLinkRejectsStaleCachedPacedAndUnknownProvenance() {
        XCTAssertNotNil(autoCompletedTransferBps(sample(), nowMs: 2_000))
        XCTAssertNil(autoCompletedTransferBps(sample(completedAt: 0), nowMs: 10_001))
        XCTAssertNil(autoCompletedTransferBps(sample(cached: true), nowMs: 2_000))
        XCTAssertNil(autoCompletedTransferBps(sample(paced: true), nowMs: 2_000))
        XCTAssertNil(autoCompletedTransferBps(sample(paced: nil), nowMs: 2_000))
    }

    func testUnknownPeakOriginalNeedsDistinctSegmentsAndEmpiricalMargin() {
        XCTAssertFalse(autoOriginalTransferMarginProven([sample(), sample()], sessionId: "staged", nowMs: 2_000))
        XCTAssertTrue(autoOriginalTransferMarginProven([sample("a"), sample("b")], sessionId: "staged", nowMs: 2_000))
        XCTAssertFalse(autoOriginalTransferMarginProven([sample("a", bodySeconds: 0.6), sample("b")], sessionId: "staged", nowMs: 2_000))
        XCTAssertFalse(autoOriginalTransferMarginProven([sample("a"), sample("b", mediaSeconds: nil)], sessionId: "staged", nowMs: 2_000))
        XCTAssertFalse(autoOriginalTransferMarginProven([sample("a"), sample("b")], sessionId: "another", nowMs: 2_000))
    }

    func testA05PacingAndAttachmentUpgradeWindowsUseActualEvidence() {
        var status = PlaybackSessionStatus(id: "incumbent")
        status.producerState = "held"
        status.activeEncodeCandidateId = "candidate"
        status.activeEncodeMilliRealtime = 2_000
        status.activeEncodeAgeMs = 1_000
        status.activeEncodeActiveMs = 2_000
        status.activeEncodeSegments = 2
        func pressure(_ snapshot: PlaybackSessionStatus?, session: String? = "incumbent", at: Int = 2_000) -> Bool {
            autoActiveProductionPressure(status: snapshot, observedAtMs: 1_000, nowMs: at,
                sessionId: session, candidateId: "candidate", runwaySeconds: 3)
        }
        XCTAssertFalse(pressure(status), "paced wall delivery with actual 2x work is not pressure")
        status.activeEncodeMilliRealtime = 800
        XCTAssertTrue(pressure(status), "fresh saturation still counts while producer currently held")
        XCTAssertFalse(pressure(status, at: 16_001))
        XCTAssertFalse(pressure(status, session: "replacement"))
        XCTAssertFalse(pressure(nil))
        status.activeEncodeActiveMs = nil
        XCTAssertFalse(pressure(status), "unknown active time is not saturation proof")
        let item = NSObject(), successor = NSObject()
        var window = AutoUpgradeEvidenceWindow()
        window.bind(attachment: ObjectIdentifier(item), attempt: "attempt1", nowMs: 0)
        window.stalled(at: 1_000)
        window.cliff(completedAtMs: 2_000, nowMs: 3_000)
        window.cliff(completedAtMs: 2_000, nowMs: 10_000)
        XCTAssertEqual(window.lastCliffMs, 2_000, "same completion never renews cliff")
        XCTAssertFalse(window.allowsUpgrade(nowMs: 60_999))
        XCTAssertFalse(window.allowsUpgrade(nowMs: 91_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 92_000))
        XCTAssertFalse(window.allowsUpgrade(nowMs: 999))
        window.bind(attachment: ObjectIdentifier(successor), attempt: "attempt1", nowMs: 92_000)
        XCTAssertNil(window.lastStallMs)
        XCTAssertNil(window.lastCliffMs)
        window.stalled(at: 93_000)
        window.bind(attachment: ObjectIdentifier(successor), attempt: "attempt2", nowMs: 93_000)
        XCTAssertNil(window.lastStallMs)
        window.cliff(completedAtMs: 1, nowMs: 93_000)
        XCTAssertNil(window.lastCliffMs)
        window.bind(attachment: nil, attempt: "attempt2", nowMs: 93_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 200_000))
    }

    func testA05FreshAttachmentRequiresObservedQuietIntervalBeforeHeadroomUpgrade() {
        let item = NSObject(), replacement = NSObject()
        var window = AutoUpgradeEvidenceWindow()
        XCTAssertFalse(window.allowsUpgrade(nowMs: 100_000), "unobserved attachment is Unknown")
        window.bind(attachment: ObjectIdentifier(item), attempt: "first", nowMs: 1_000)
        let headroomStartedAt = 1_000
        XCTAssertTrue(46_000 - headroomStartedAt >= 45_000, "independent headroom is ready")
        XCTAssertFalse(window.allowsUpgrade(nowMs: 46_000), "45s headroom cannot bypass 60s observation")
        XCTAssertFalse(window.allowsProposal(nowMs: 46_000, headroomStartedAtMs: headroomStartedAt))
        window.bind(attachment: ObjectIdentifier(item), attempt: "first", nowMs: 50_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 60_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 61_000), "same binding does not renew observation")
        XCTAssertTrue(window.allowsProposal(nowMs: 61_000, headroomStartedAtMs: headroomStartedAt), "concurrent windows first allow max(45s,60s), not 105s")
        XCTAssertFalse(window.allowsProposal(nowMs: 61_000, headroomStartedAtMs: nil))
        XCTAssertFalse(window.allowsProposal(nowMs: 61_000, headroomStartedAtMs: 61_001))
        XCTAssertFalse(window.allowsUpgrade(nowMs: 999), "clock rollback is Unknown")
        window.stalled(at: 62_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 121_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 122_000))
        window.bind(attachment: ObjectIdentifier(item), attempt: "second", nowMs: 123_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 182_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 183_000))
        window.bind(attachment: ObjectIdentifier(replacement), attempt: "second", nowMs: 184_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 243_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 244_000))
        window.cliff(completedAtMs: 244_000, nowMs: 245_000)
        window.cliff(completedAtMs: 244_000, nowMs: 250_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 333_999))
        XCTAssertTrue(window.allowsUpgrade(nowMs: 334_000), "original EOF age is not renewed")
        window.bind(attachment: nil, attempt: "second", nowMs: 335_000)
        XCTAssertFalse(window.allowsUpgrade(nowMs: 500_000))
    }

    func testA05StagedAdvertisedIntervalsRequireCapturedScopeAndOriginalDeadline() {
        let manifest = "#EXTM3U\n#EXT-X-PLAYLIST-TYPE:VOD\n#EXTINF:1.000000,\nseg00000.m4s\n#EXTINF:1.000000,\nseg00001.m4s\n#EXT-X-ENDLIST\n"
        let intervals = autoVODAdvertisedIntervals(Data(manifest.utf8))!
        XCTAssertEqual(intervals["seg00001.m4s"]?.startSeconds, 1)
        XCTAssertNil(autoVODAdvertisedIntervals(Data(manifest.replacingOccurrences(of: "seg00001", with: "seg00000").utf8)))
        XCTAssertNil(autoVODAdvertisedIntervals(Data(manifest.replacingOccurrences(of: "seg00001", with: "seg1").utf8)))
        XCTAssertNil(autoVODAdvertisedIntervals(Data(manifest.replacingOccurrences(of: "#EXT-X-ENDLIST", with: "#EXT-X-DISCONTINUITY").utf8)))
        XCTAssertNil(autoVODAdvertisedIntervals(Data(repeating: 65, count: 1_048_577)))
        let scope = UUID()
        func transfer(_ index: Int) -> PlayerController.AutoCompletedTransfer {
            .init(bodyBytes: 100_000, bodyDurationSeconds: 0.1, completedAtMs: 1_000,
                origin: "https://node", networkLoad: true, fromLocalCache: false, producerPaced: false,
                statusCode: 200, segmentId: "https://node/hls/staged/seg0000\(index).m4s", mediaDurationSeconds: nil,
                receipt: "00000000-0000-0000-0000-00000000000\(index)", etag: "object\(index)",
                installedSessionId: "staged", installedCandidateId: "candidate", observedMediaDurationMs: 1_000,
                stageScope: scope)
        }
        let first = transfer(0), second = transfer(1)
        func margin(_ samples: [PlayerController.AutoCompletedTransfer], now: Int = 2_000, token: UUID? = nil) -> Bool {
            autoVODEmpiricalMargin(samples, intervals: intervals, scope: token ?? scope,
                sessionId: "staged", candidateId: "candidate", nowMs: now, deadlineMs: 15_000)
        }
        XCTAssertTrue(margin([first, second]))
        XCTAssertFalse(margin([first, first]))
        XCTAssertFalse(margin([first, second], token: UUID()))
        XCTAssertFalse(margin([first, second], now: 15_000))
        XCTAssertFalse(margin([first, second], now: 999))
        var mismatched = second
        mismatched.observedMediaDurationMs = 1_002
        XCTAssertFalse(margin([first, mismatched]))
        mismatched = second
        mismatched.installedCandidateId = "replacement"
        XCTAssertFalse(margin([first, mismatched]))
        let overlapping = ["seg00000.m4s": intervals["seg00000.m4s"]!,
            "seg00001.m4s": AutoVODAdvertisedInterval(startSeconds: 0.5, durationSeconds: 1)]
        XCTAssertFalse(autoVODEmpiricalMargin([first, second], intervals: overlapping, scope: scope,
            sessionId: "staged", candidateId: "candidate", nowMs: 2_000, deadlineMs: 15_000))
    }

    func testStagedProductionProofRequiresExactCandidateAndCombinedAge() {
        var status = PlaybackSessionStatus(id: "staged")
        status.activeEncodeCandidateId = "candidate"
        status.activeEncodeMilliRealtime = 1_150
        status.activeEncodeAgeMs = 1_000
        status.activeEncodeActiveMs = 2_000
        status.activeEncodeSegments = 2
        XCTAssertTrue(autoStagedEncodeProof(status, observedAtMs: 1_000, nowMs: 2_000, sessionId: "staged", candidateId: "candidate"))
        XCTAssertFalse(autoStagedEncodeProof(status, observedAtMs: 1_000, nowMs: 2_000, sessionId: "staged", candidateId: "other"))
        status.activeEncodeAgeMs = 14_500
        XCTAssertFalse(autoStagedEncodeProof(status, observedAtMs: 1_000, nowMs: 2_000, sessionId: "staged", candidateId: "candidate"))
        status.activeEncodeAgeMs = 0
        status.activeEncodeSegments = 1
        XCTAssertFalse(autoStagedEncodeProof(status, observedAtMs: 1_000, nowMs: 2_000, sessionId: "staged", candidateId: "candidate"))
    }

    func testSupersededAutoTrialCannotReplaceIncumbent() {
        XCTAssertTrue(autoTrialMayCommit(requestedId: "a", offeredId: "a", requestedTargetRevision: 2,
            targetRevision: 2, requestedViewerEpoch: 4, viewerEpoch: 4, automatic: true, presenting: true, seeking: false))
        XCTAssertFalse(autoTrialMayCommit(requestedId: "a", offeredId: "a", requestedTargetRevision: 2,
            targetRevision: 3, requestedViewerEpoch: 4, viewerEpoch: 4, automatic: true, presenting: true, seeking: false))
        XCTAssertFalse(autoTrialMayCommit(requestedId: "a", offeredId: "a", requestedTargetRevision: 2,
            targetRevision: 2, requestedViewerEpoch: 4, viewerEpoch: 5, automatic: true, presenting: true, seeking: false))
    }

    func testManual1440RemainsSelectableBeforeProductionProof() {
        let candidate = QualityCandidate(id: "0a7ba9bab6fbdd31bab5e5e362a3fac7", recipeDigest: Array(repeating: 0, count: 32),
            route: "encode", width: 2_560, height: 1_440, targetHeight: 1_440, grade: "sdr",
            decoderCompatible: true, completeCache: false, sustainable: false)
        XCTAssertEqual(manualCatalogHeights([candidate]), [1_440])
    }
}

final class DisplayAwareAutoIntentTests: XCTestCase {
    func testAutomaticCandidateRecoveryKeepsAutoAndOmitsOutOfFloorCopyHeight() throws {
        let id = "0a7ba9bab6fbdd31bab5e5e362a3fac7"
        let selection = MediaIntentSelection(quality: .autoCandidate(height: nil, candidateId: id),
            codec: .auto, dynamicRange: .auto, audioTrack: 2, audioOffsetMs: 0,
            subtitles: SubtitleSelection(mode: .off, track: nil))
        let envelope = MediaIntentEnvelope(lifetimeId: "same-player", recipeRevision: 2,
            destinationRevision: 3, transportRevision: 4, selection: selection)
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: encoder.encode(envelope)) as? [String: Any])
        let ask = try XCTUnwrap(object["selection"] as? [String: Any])
        let quality = try XCTUnwrap(ask["quality"] as? [String: Any])
        XCTAssertEqual(quality["mode"] as? String, "auto")
        XCTAssertEqual(quality["candidate_id"] as? String, id)
        XCTAssertNil(quality["height"])
        XCTAssertEqual(object["lifetime_id"] as? String, "same-player")
        XCTAssertEqual(object["recipe_revision"] as? Int, 2)
        XCTAssertEqual(object["destination_revision"] as? Int, 3)
        XCTAssertEqual(object["transport_revision"] as? Int, 4)
    }
}
