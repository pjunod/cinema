import Foundation

/// The playback-control exchange, over HTTP.
///
/// Separate from `PlaybackControlReporter` because the reporter's rules are
/// about ordering and the transport's are about what a server's refusal means.
/// The reporter classifies a failure by `status`, `code` and `retry_after_ms`;
/// producing those faithfully from an HTTP response is this file's whole job,
/// and getting it wrong would make a retryable refusal look terminal.
struct PlaybackControlTransport {
    /// Absolute origin of the server this session belongs to. The bootstrap's
    /// `url` is server-relative and already shape-checked, so joining them
    /// cannot reach another host.
    var origin: String
    var authorize: (inout URLRequest) -> Void
    var session: URLSession

    init(
        origin: String,
        authorize: @escaping (inout URLRequest) -> Void,
        session: URLSession = PlaybackControlTransport.defaultSession
    ) {
        self.origin = origin
        self.authorize = authorize
        self.session = session
    }

    /// The exchange is bounded by the reporter's own deadline, so this session
    /// only needs a ceiling that cannot outlive it by much. `waitsForConnectivity`
    /// is deliberately off: a control exchange that waits for the network to
    /// come back is a stale exchange by the time it lands, and the reporter
    /// would rather send the newest snapshot than an old one.
    static let defaultSession: URLSession = {
        let configuration = URLSessionConfiguration.default
        configuration.waitsForConnectivity = false
        configuration.timeoutIntervalForRequest = 10
        configuration.timeoutIntervalForResource = 10
        return URLSession(configuration: configuration)
    }()

    func send(_ path: String, _ request: ControlRequest) async throws -> ControlResponse {
        guard ControlBootstrap.isSessionControlPath(path),
              let url = URL(string: origin.hasSuffix("/")
                  ? String(origin.dropLast()) + path
                  : origin + path)
        else {
            // Not retryable and not a server refusal: the reporter must stop
            // rather than hammer an address it cannot form.
            throw ControlProtocolError(reason: "url")
        }
        var urlRequest = URLRequest(url: url)
        urlRequest.httpMethod = "POST"
        urlRequest.setValue("application/json", forHTTPHeaderField: "Content-Type")
        urlRequest.httpBody = try PlaybackControl.encoder.encode(request)
        authorize(&urlRequest)

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: urlRequest)
        } catch {
            // No status at all — the reporter treats that as retryable
            // transport, which is right: the server never saw this exchange.
            throw ControlTransportError(
                status: nil,
                code: nil,
                canceled: (error as? URLError)?.code == .cancelled
            )
        }
        guard let http = response as? HTTPURLResponse else {
            throw ControlTransportError(status: nil, code: nil)
        }
        guard (200..<300).contains(http.statusCode) else {
            throw Self.failure(status: http.statusCode, body: data)
        }
        do {
            return try PlaybackControl.decoder.decode(ControlResponse.self, from: data)
        } catch {
            // A 200 that is not a control response is not a transport problem
            // to retry; it is something other than this server's control route
            // answering.
            throw ControlProtocolError(reason: "body")
        }
    }

    /// A refusal carries the fields the reporter classifies on. Every one is
    /// optional on the wire, and a body that is missing or unparseable still
    /// yields the status, which is enough to decide retryable from terminal.
    static func failure(status: Int, body: Data) -> ControlTransportError {
        var failure = ControlTransportError(status: status, code: nil)
        guard let object = try? JSONSerialization.jsonObject(with: body),
              let fields = object as? [String: Any]
        else { return failure }
        failure.code = fields["code"] as? String
        if let generation = fields["generation"] as? String { failure.generation = generation }
        if let epoch = fields["control_epoch"] as? Int { failure.controlEpoch = epoch }
        if let retryAfter = fields["retry_after_ms"] as? Int { failure.retryAfterMs = retryAfter }
        return failure
    }
}

/// One player's control reporting, from the bootstrap the server handed back
/// with the session to the last exchange before teardown.
///
/// This is the piece `PlayerController` holds. It exists so the controller
/// deals in "the player changed" rather than in actors, transports, identities
/// and deadlines.
@MainActor
final class PlaybackControlSession {
    private var reporter: PlaybackControlReporter?
    private var observe: (() -> PlayerControlObservation?)?
    private var activeGeneration: Int?
    private let scheduleSubtitleReady: @Sendable (
        @escaping @MainActor @Sendable () -> Void
    ) -> Void

    init(
        scheduleSubtitleReady: @escaping @Sendable (
            @escaping @MainActor @Sendable () -> Void
        ) -> Void = { callback in Task { @MainActor in callback() } }
    ) {
        self.scheduleSubtitleReady = scheduleSubtitleReady
    }

    /// What the reporter reads. See `PlaybackControlLatestCapture`: the
    /// player pushes here, the reporter never pulls from the player.
    private let latest = PlaybackControlLatestCapture()
    private var captureRevision = 0

    /// What the reporter writes. The mirror of `latest`, and it exists for the
    /// same reason: the reporter is an actor and the player is `@MainActor`,
    /// so the two never call each other. A lock-guarded slot is the whole
    /// bridge.
    ///
    /// `MainActor.assumeIsolated` inside a closure an actor pulls
    /// synchronously is an assertion, not a bridge, and it killed every play
    /// on build 90. Nothing here hops.
    private let verdicts = PlaybackControlLatestVerdict()

    /// Every exchange's action, counted. The ask reads the count, waits for it
    /// to move, and takes whatever the exchange that moved it carried.
    private let answers = PlaybackControlAnswers()

    /// The last terminal verdict this session was given, if any.
    ///
    /// It deliberately outlives the reporter. A terminal verdict stops
    /// reporting — correctly, since the reporter owns no recovery — so a
    /// verdict that died with it would be discarded exactly when it mattered:
    /// at the failure it explains, minutes later.
    var terminalVerdict: ControlAction? { verdicts.load() }

    /// One identity per player instance, not per session: a reopen is the same
    /// viewer on the same device continuing, and the server reads a new
    /// `client_instance_id` as a different client.
    private let clientInstanceId = UUID().uuidString.lowercased()

    var isReporting: Bool { reporter != nil }

    /// Where this playback sits in the control ordering, for a create that
    /// wants to be ordered against the settled destination.
    ///
    /// The reporter's own counter rather than the accepted sequence: a create
    /// can reach the server before the snapshot that justifies it, and a
    /// client that reports a *higher* sequence can only look less superseded,
    /// which is the safe direction. `nil` before the first exchange.
    var controlSequence: UInt64? {
        get async {
            guard let reporter else { return nil }
            let sequence = await reporter.sequence
            return sequence > 0 ? UInt64(sequence) : nil
        }
    }

    /// Begin reporting for a session the server said is controllable. A
    /// bootstrap the client cannot address leaves this silent, which is the
    /// passive behaviour rather than a playback failure.
    func begin(
        bootstrap: ControlBootstrap,
        transport: PlaybackControlTransport,
        observe: @escaping () -> PlayerControlObservation?,
        onSubtitleReady: @escaping @MainActor @Sendable () -> Void = {}
    ) {
        end()
        // A generation, not a reset. A verdict outlives the session it was
        // given in, because the failure it explains usually arrives after a
        // reopen; `clearVerdict()` is how a new title starts clean.
        let generation = verdicts.beginGeneration()
        let owner = PlaybackControlCaptureOwner(
            lifecycleId: clientInstanceId, attachmentGeneration: generation
        )
        activeGeneration = generation
        answers.begin(generation: generation)
        let lease = TimeInterval(bootstrap.leaseTimeoutMs) / 1_000
        let subtitleReadiness = SubtitleReadinessRetryState()
        self.observe = observe
        // The reporter takes its first snapshot the moment it starts, so the
        // first one has to be there before it does.
        publish()
        reporter = PlaybackControlReporter(
            bootstrap: bootstrap,
            clientInstanceId: clientInstanceId,
            owner: owner,
            capture: { [latest] in
                guard let value = latest.load(), value.owner == owner else { return nil }
                return value
            },
            send: { path, request in try await transport.send(path, request) },
            sleep: { milliseconds, _ in
                try await Task.sleep(nanoseconds: UInt64(max(0, milliseconds)) * 1_000_000)
            },
            now: { Int(Date().timeIntervalSince1970 * 1_000) },
            // The return path. Until now this defaulted to a no-op, so the
            // server could send a verdict the player would never see.
            //
            // Only `terminal` is retained, and it is retained rather than
            // acted on: ruling D1 in the M5 handoff. `hold` and
            // `retry_resource` are exchange-level and the reporter already
            // honours them; a player that acted on them here would be
            // deciding, which is M5e.
            onExchange: { [weak self, latest, verdicts, answers, scheduleSubtitleReady] exchange in
                guard exchange.capture.owner == owner,
                      latest.load()?.owner == owner
                else { return }
                // Every exchange advances the counter, including a failed one:
                // an owner that asked must not wait out its whole bound for an
                // exchange that has already come back with nothing.
                answers.record(
                    exchange.response?.action,
                    requestSequence: exchange.request.sequence,
                    generation: generation,
                    ownerChanged: exchange.failure == "transport:409:owner_changed"
                )
                if exchange.capture.hasSameIntent(as: latest.load()), subtitleReadiness.record(
                    exchange.response?.delivery?.subtitleReadiness, commitReady: false
                ) {
                    scheduleSubtitleReady { [weak self] in
                        // A ready edge can wait for MainActor while a new
                        // session begins or playback ends. Validate when the
                        // callback executes, not when it was enqueued.
                        guard self?.activeGeneration == generation,
                              exchange.capture.hasSameIntent(as: latest.load()),
                              subtitleReadiness.record(exchange.response?.delivery?.subtitleReadiness)
                        else { return }
                        onSubtitleReady()
                    }
                }
                guard let action = exchange.response?.action,
                      action.type == "terminal",
                      action.message?.isEmpty == false
                else { return }
                verdicts.store(
                    action,
                    generation: generation,
                    intentGeneration: exchange.intentGeneration,
                    lease: lease
                )
            }
        )
        guard let reporter else {
            latest.store(nil)
            return
        }
        Task { await reporter.start() }
    }

    /// The player changed. Cheap enough to call from a time observer: the
    /// reporter coalesces, so a notification between exchanges costs nothing
    /// but replaces what the next exchange will carry.
    func playerChanged() {
        guard let capture = publish(), let reporter else { return }
        Task { await reporter.notify(capture) }
    }

    /// A recovery owner published evidence and is about to act on it.
    ///
    /// Coalescing is right for a position update and wrong for this. The pump
    /// sleeps for `next_exchange_ms` — up to a minute — and the owner's own
    /// reopen normally ends this reporter before it wakes, so the evidence
    /// would be discarded rather than sent late. The web reporter has always
    /// drained inline at exactly this call site, for exactly this reason.
    ///
    /// Restricted to callers that have evidence rather than a position, so the
    /// ordinary cadence is unchanged.
    func reportEvidence() {
        guard let capture = publish(), let reporter else { return }
        Task { await reporter.notifyUrgently(capture) }
    }

    /// Publish an interactive intent before the media mutation it authorizes.
    /// Snapshot capture happens synchronously on MainActor; awaiting only
    /// queues that immutable value and returns the request-ordering floor.
    func reportIntent() async -> UInt64? {
        guard let capture = publish(), let reporter,
              let floor = await reporter.notifyUrgently(capture)
        else { return nil }
        return UInt64(floor)
    }

    /// Publish what a recovery owner is about to act on, then wait — briefly —
    /// for the verdict that evidence earns.
    ///
    /// `reportEvidence` tells the server. This tells the server and listens.
    /// The difference is the whole milestone: an owner that only reports still
    /// decides for itself, and every one of them decides by guessing toward
    /// retry.
    ///
    /// Returns `nil` when the reporter is gone, the exchange failed, or nothing
    /// arrived in time — and every caller must fall through to its existing
    /// behaviour on it. That fallback is not a hedge: a server that has not yet
    /// decided must not strand a stalled viewer.
    ///
    /// Polled rather than continuation-based on purpose. A continuation resumed
    /// twice traps and one never resumed hangs the caller forever, and this is
    /// resumed from an actor's closure on one side and a deadline on the other.
    /// A counter and a sleep cannot do either.
    func askForAction(
        bound: TimeInterval,
        cap: TimeInterval,
        publish: () -> Void
    ) async -> ControlAction? {
        guard let reporter else { return nil }
        // A reporter that has stopped will never exchange again — a terminal
        // verdict or a non-retryable failure ends it — and the session keeps
        // holding it. Waiting out the bound for one is pure frozen picture.
        if await reporter.stopped { return nil }
        // The floor is read BEFORE the evidence is published, and the caller
        // hands the publish in for exactly that reason. Publishing first lets
        // the pump reach `nextRequest()` — and increment `sequence` — before
        // this hop lands, which makes the floor one too high and rejects the
        // very exchange that carried this stall's evidence.
        let floor = await reporter.sequence + 1
        let seenAtStart = answers.count()
        let ownerChangesAtStart = answers.ownerChangeCount()
        var seen = seenAtStart
        publish()
        // Monotonic, because this file's own stall policy is monotonic: a
        // wall-clock adjustment mid-ask would lengthen or truncate the bound.
        let startedAt = ProcessInfo.processInfo.systemUptime
        var deadline = startedAt + bound
        let hardDeadline = startedAt + cap
        var extended = false
        // Both conditions, not just the floor. `adoptNewOwner` resets the
        // reporter's sequence on a 409, so a floor read after one can sit
        // BELOW an answer already in the slot from before this ask — and an
        // answer that arrived before the ask cannot be its answer.
        func settled() -> ControlAction? {
            guard answers.count() > seenAtStart,
                  let answer = answers.answer(atOrAfter: floor)
            else { return nil }
            return answer.action
        }
        while ProcessInfo.processInfo.systemUptime < deadline {
            // The adopted owner has a new sequence space and did not answer
            // this observation. Let it continue independently, but release the
            // current recovery owner instead of extending a frozen wait.
            if answers.ownerChangeCount() > ownerChangesAtStart { return nil }
            if let answer = settled() { return answer }
            let count = answers.count()
            if count > seen {
                seen = count
                // An exchange finished and it was not ours, which means the
                // reporter could not have started ours until now: `drain`
                // returns immediately while one is in flight. One window from
                // this instant, once, or the bound would expire at the moment
                // an answer first became possible.
                if !extended {
                    extended = true
                    deadline = min(ProcessInfo.processInfo.systemUptime + bound, hardDeadline)
                }
            }
            try? await Task.sleep(nanoseconds: PlaybackControlSession.askPollNanoseconds)
            // A reporter that went away or stopped mid-ask will never exchange
            // again, so waiting out the rest of the bound would add it to a
            // stall for nothing — but read the slot one more time first.
            //
            // A terminal verdict is answered and then stops the reporter in the
            // same instant. Bailing on `stopped` without re-reading discards
            // the one verdict this ask most needed to see, and the client falls
            // through to its own guess for the exact case where the server was
            // certain. The Android mirror's terminal test caught this.
            guard let current = self.reporter else {
                return settled()
            }
            if await current.stopped {
                return settled()
            }
        }
        return settled()
    }

    /// How often the ask looks. Short enough that it costs a stalled viewer
    /// nothing measurable, long enough that it is not a spin.
    static let askPollNanoseconds: UInt64 = 25_000_000

    /// A new title. The old verdict described a source that is no longer
    /// playing, so keeping it would show a confident sentence about the wrong
    /// film.
    func clearVerdict() {
        verdicts.clearAndAdvanceIntent()
        latest.store(nil)
    }

    func end() {
        activeGeneration = nil
        // A callback may already have passed the capture-slot guard on the
        // reporter actor. Revoke publication under the verdict slot's lock;
        // preserve any verdict armed before End, but reject later writes.
        let generation = verdicts.beginGeneration()
        answers.begin(generation: generation)
        latest.store(nil)
        observe = nil
        guard let reporter else { return }
        self.reporter = nil
        Task { await reporter.stop() }
    }

    /// Read the player once, on the actor that owns it, and publish what the
    /// reporter will read. The mapping runs here rather than in the reporter's
    /// closure for the same reason: everything that touches the player belongs
    /// on the player's actor.
    @discardableResult
    private func publish() -> PlaybackControlCapture? {
        guard let generation = activeGeneration, let observation = observe?() else {
            latest.store(nil)
            return nil
        }
        captureRevision += 1
        let capture = PlaybackControlCapture(
            snapshot: PlaybackControlMapping.snapshot(from: observation),
            intentGeneration: verdicts.intentGeneration(),
            owner: PlaybackControlCaptureOwner(
                lifecycleId: clientInstanceId, attachmentGeneration: generation
            ),
            sourceRevision: captureRevision
        )
        latest.store(capture)
        return capture
    }
}


/// One slot per exchange outcome, counted so an ask can tell "the answer I was
/// waiting for" from "an answer that was already there".
///
/// The generation is checked on write for the same reason the verdict slot
/// checks it: `end()` stops the old reporter in an unstructured task, so an old
/// exchange can land after the next session has begun.
private final class PlaybackControlAnswers: @unchecked Sendable {
    struct Answer {
        /// The sequence of the REQUEST this answered, not a count of answers.
        var requestSequence: Int
        var action: ControlAction?
    }

    private let lock = NSLock()
    private var latest: Answer?
    private var answered = 0
    private var ownerChanges = 0
    private var generation = 0

    /// Adopt the verdict slot's generation rather than keeping a second one.
    /// Two counters that must agree are a bug waiting for a reason.
    func begin(generation: Int) {
        lock.lock()
        defer { lock.unlock() }
        self.generation = generation
        latest = nil
        answered = 0
        ownerChanges = 0
    }

    func record(
        _ action: ControlAction?,
        requestSequence: Int,
        generation: Int,
        ownerChanged: Bool = false
    ) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation else { return }
        answered += 1
        if ownerChanged { ownerChanges += 1 }
        latest = Answer(requestSequence: requestSequence, action: action)
    }

    /// How many exchanges have come back at all, ours or not.
    func count() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return answered
    }

    func ownerChangeCount() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return ownerChanges
    }

    func answer(atOrAfter sequence: Int) -> Answer? {
        lock.lock()
        defer { lock.unlock() }
        guard let latest, latest.requestSequence >= sequence else { return nil }
        return latest
    }
}

/// The reporter's half of the bridge: one verdict, written from an actor and
/// read from `@MainActor`, with a lock rather than an isolation assertion.
///
/// The token is not decoration. `end()` stops the old reporter with an
/// unstructured `Task`, so a reopen can begin the next session before that
/// stop lands, and an old in-flight exchange completing in that window would
/// carry a previous generation's verdict into the new one.
private final class PlaybackControlLatestVerdict: @unchecked Sendable {
    private let lock = NSLock()
    private var value: ControlAction?
    private var armedAt: TimeInterval = 0
    private var lease: TimeInterval = 0
    private var generation = 0
    private var viewerIntentGeneration = 0

    /// Claim the next generation. Deliberately does not clear the verdict: a
    /// reopen is the same viewer on the same title, and the failure a verdict
    /// explains usually arrives on the far side of one.
    func beginGeneration() -> Int {
        lock.lock()
        defer { lock.unlock() }
        generation += 1
        return generation
    }

    func store(
        _ action: ControlAction,
        generation: Int,
        intentGeneration: Int,
        lease: TimeInterval
    ) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation,
              intentGeneration == viewerIntentGeneration
        else { return }
        value = action
        armedAt = ProcessInfo.processInfo.systemUptime
        self.lease = lease
    }

    func clearAndAdvanceIntent() {
        lock.lock()
        defer { lock.unlock() }
        viewerIntentGeneration += 1
        value = nil
    }

    func intentGeneration() -> Int {
        lock.lock()
        defer { lock.unlock() }
        return viewerIntentGeneration
    }

    /// A verdict outlives its reporter and its session, but not the lease the
    /// server gave that session. Past it the session the verdict described is
    /// gone, and a sentence delivered an hour ago would caption an unrelated
    /// failure with total confidence. The bound is the server's own number.
    func load() -> ControlAction? {
        lock.lock()
        defer { lock.unlock() }
        guard value != nil else { return nil }
        if ProcessInfo.processInfo.systemUptime - armedAt > lease {
            value = nil
            return nil
        }
        return value
    }
}

/// The newest snapshot the player has produced, written by the main actor and
/// read by the reporter's.
///
/// `PlaybackControlReporter` is an actor and pulls its snapshot synchronously
/// from inside itself, so the closure it holds runs on the reporter's
/// executor — never the main actor's. A closure cannot *assume* main-actor
/// isolation there: `MainActor.assumeIsolated` traps rather than falling back,
/// and doing it here crashed the app on the first exchange of every session
/// the server considered controllable (build 90). Sending the snapshot the
/// other way removes the assumption instead of checking it.
///
/// The staleness this admits is bounded by how often the player reports that
/// it changed — once a second from the periodic time observer, plus every
/// rate change — against an exchange cadence the server never sets faster.
private final class PlaybackControlLatestCapture: @unchecked Sendable {
    private let lock = NSLock()
    private var value: PlaybackControlCapture?

    func store(_ snapshot: PlaybackControlCapture?) {
        lock.lock()
        defer { lock.unlock() }
        value = snapshot
    }

    func load() -> PlaybackControlCapture? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}
