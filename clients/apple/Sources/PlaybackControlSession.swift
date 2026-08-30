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
        self.observe = observe
        let snapshot: @Sendable () -> PlaybackControlSnapshot? = { [weak self] in
            guard let observation = self?.currentObservation() else { return nil }
            return PlaybackControlMapping.snapshot(from: observation)
        }
        reporter = PlaybackControlReporter(
            bootstrap: bootstrap,
            clientInstanceId: clientInstanceId,
            snapshot: snapshot,
            send: { path, request in try await transport.send(path, request) },
            sleep: { milliseconds, _ in
                try await Task.sleep(nanoseconds: UInt64(max(0, milliseconds)) * 1_000_000)
            },
            now: { Int(Date().timeIntervalSince1970 * 1_000) }
        )
        guard let reporter else { return }
        Task { await reporter.start() }
    }

    /// The player changed. Cheap enough to call from a time observer: the
    /// reporter coalesces, so a notification between exchanges costs nothing
    /// but replaces what the next exchange will carry.
    func playerChanged() {
        guard let reporter else { return }
        Task { await reporter.notify() }
    }

    func end() {
        guard let reporter else { return }
        self.reporter = nil
        observe = nil
        Task { await reporter.stop() }
    }

    private nonisolated func currentObservation() -> PlayerControlObservation? {
        MainActor.assumeIsolated { observe?() }
    }
}
