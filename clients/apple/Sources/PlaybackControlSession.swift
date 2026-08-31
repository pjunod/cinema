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

    /// What the reporter reads. See `PlaybackControlLatestSnapshot`: the
    /// player pushes here, the reporter never pulls from the player.
    private let latest = PlaybackControlLatestSnapshot()

    /// What the reporter writes. The mirror of `latest`, and it exists for the
    /// same reason: the reporter is an actor and the player is `@MainActor`,
    /// so the two never call each other. A lock-guarded slot is the whole
    /// bridge.
    ///
    /// `MainActor.assumeIsolated` inside a closure an actor pulls
    /// synchronously is an assertion, not a bridge, and it killed every play
    /// on build 90. Nothing here hops.
    private let verdicts = PlaybackControlLatestVerdict()

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

    /// Begin reporting for a session the server said is controllable. A
    /// bootstrap the client cannot address leaves this silent, which is the
    /// passive behaviour rather than a playback failure.
    func begin(
        bootstrap: ControlBootstrap,
        transport: PlaybackControlTransport,
        observe: @escaping () -> PlayerControlObservation?
    ) {
        end()
        // A generation, not a reset. A verdict outlives the session it was
        // given in, because the failure it explains usually arrives after a
        // reopen; `clearVerdict()` is how a new title starts clean.
        let generation = verdicts.beginGeneration()
        let lease = TimeInterval(bootstrap.leaseTimeoutMs) / 1_000
        self.observe = observe
        // The reporter takes its first snapshot the moment it starts, so the
        // first one has to be there before it does.
        publish()
        reporter = PlaybackControlReporter(
            bootstrap: bootstrap,
            clientInstanceId: clientInstanceId,
            snapshot: { [latest] in latest.load() },
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
            onExchange: { [verdicts] exchange in
                guard let action = exchange.response?.action,
                      action.type == "terminal",
                      action.message?.isEmpty == false
                else { return }
                verdicts.store(action, generation: generation, lease: lease)
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
        publish()
        guard let reporter else { return }
        Task { await reporter.notify() }
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
        publish()
        guard let reporter else { return }
        Task { await reporter.notifyUrgently() }
    }

    /// A new title. The old verdict described a source that is no longer
    /// playing, so keeping it would show a confident sentence about the wrong
    /// film.
    func clearVerdict() { verdicts.clear() }

    func end() {
        latest.store(nil)
        guard let reporter else { return }
        self.reporter = nil
        observe = nil
        Task { await reporter.stop() }
    }

    /// Read the player once, on the actor that owns it, and publish what the
    /// reporter will read. The mapping runs here rather than in the reporter's
    /// closure for the same reason: everything that touches the player belongs
    /// on the player's actor.
    private func publish() {
        guard let observation = observe?() else {
            latest.store(nil)
            return
        }
        latest.store(PlaybackControlMapping.snapshot(from: observation))
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
    private var armedAt = Date.distantPast
    private var lease: TimeInterval = 0
    private var generation = 0

    /// Claim the next generation. Deliberately does not clear the verdict: a
    /// reopen is the same viewer on the same title, and the failure a verdict
    /// explains usually arrives on the far side of one.
    func beginGeneration() -> Int {
        lock.lock()
        defer { lock.unlock() }
        generation += 1
        return generation
    }

    func store(_ action: ControlAction, generation: Int, lease: TimeInterval) {
        lock.lock()
        defer { lock.unlock() }
        guard generation == self.generation else { return }
        value = action
        armedAt = Date()
        self.lease = lease
    }

    func clear() {
        lock.lock()
        defer { lock.unlock() }
        value = nil
    }

    /// A verdict outlives its reporter and its session, but not the lease the
    /// server gave that session. Past it the session the verdict described is
    /// gone, and a sentence delivered an hour ago would caption an unrelated
    /// failure with total confidence. The bound is the server's own number.
    func load() -> ControlAction? {
        lock.lock()
        defer { lock.unlock() }
        guard value != nil else { return nil }
        if Date().timeIntervalSince(armedAt) > lease {
            value = nil
            return nil
        }
        return value
    }
}

/// The newest snapshot the player has produced, written by the main actor and
/// read by the reporter's.
private final class PlaybackControlLatestSnapshot: @unchecked Sendable {
    private let lock = NSLock()
    private var value: PlaybackControlSnapshot?

    func store(_ snapshot: PlaybackControlSnapshot?) {
        lock.lock()
        defer { lock.unlock() }
        value = snapshot
    }

    func load() -> PlaybackControlSnapshot? {
        lock.lock()
        defer { lock.unlock() }
        return value
    }
}
