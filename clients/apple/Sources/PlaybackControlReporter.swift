import Foundation

/// Apple's passive playback-control reporter.
///
/// M2's contract is one coalescing controller per platform that reports what
/// the player is actually doing — intent, film position, contiguous runway,
/// render state, selection, and throughput evidence — and consumes no action.
/// `crates/plurxd/src/web/playback-control.js` is the reference
/// implementation; this is a faithful port of its state machine, and the two
/// are meant to stay observably identical. Where the JS relies on a truthy
/// check this uses a typed optional, but no rule differs.
///
/// Passive means passive: this reporter declares the actions it will accept
/// and treats anything else as a protocol error that stops it, rather than as
/// an instruction to improvise. Today that vocabulary is `hold`, which is an
/// explanation and not an instruction — the server saying production is
/// deliberately not advancing. Accepting it is what keeps this client
/// reporting through a deliberate hold; acting on it is the action owner's
/// job, and recovery authority still belongs to this platform's own timers
/// until M5 moves it.
enum PlaybackControl {
    static let protocolName = "plurx-playback-control-v1"
    /// The actions this client will accept, and therefore the only ones the
    /// server will send it. An action that is never declared is never sent, so
    /// a client cannot be silenced by one it does not understand.
    static let supportedActions = ["hold", "retry_resource", "terminal"]
    static let minimumExchangeMs = 250
    static let maximumExchangeMs = 60_000
    static let exchangeDeadlineMs = 6_000
    static let maximumLeaseTimeoutMs = 600_000
    static let minimumHeight = 144
    static let maximumHeight = 2_160
    static let maximumCapabilityValues = 8
    static let maximumObservedDownloadBps: Int64 = 10_000_000_000_000
    static let maximumErrorDetailBytes = 512

    /// The server sends a UUID; a client that accepts anything else would let
    /// a redirected or spoofed bootstrap rebind the session.
    static func isUUID(_ value: String) -> Bool {
        UUID(uuidString: value) != nil
    }

    /// The control types spell their keys in camelCase and let the key
    /// strategy produce the wire's snake_case, exactly as the rest of the
    /// app's models do. These coders make that explicit for anything that
    /// touches control JSON without going through `PlurxAPI`.
    static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        return encoder
    }()

    static let decoder: JSONDecoder = {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return decoder
    }()
}

// MARK: - Wire types

struct ControlBootstrap: Codable, Equatable {
    var proto: String
    var url: String
    var generation: String
    var controlEpoch: Int
    var nextExchangeMs: Int
    var leaseTimeoutMs: Int

    enum CodingKeys: String, CodingKey {
        // `protocol` is a Swift keyword, so the property is `proto` and the
        // wire name is stated. Every other key is the camelCase spelling of
        // its snake_case wire name, which is what the app's shared
        // `.convertFromSnakeCase` / `.convertToSnakeCase` coders expect.
        case proto = "protocol"
        case url, generation, controlEpoch, nextExchangeMs, leaseTimeoutMs
    }

    /// The same acceptance the web reporter applies before it will speak at
    /// all. An older or foreign server that omits a field leaves the client
    /// silent rather than reporting into a session it cannot address.
    var isValid: Bool {
        proto == PlaybackControl.protocolName
            && Self.isSessionControlPath(url)
            && PlaybackControl.isUUID(generation)
            && controlEpoch > 0
            && nextExchangeMs >= PlaybackControl.minimumExchangeMs
            && nextExchangeMs <= PlaybackControl.maximumExchangeMs
            && leaseTimeoutMs >= nextExchangeMs
            && leaseTimeoutMs <= PlaybackControl.maximumLeaseTimeoutMs
    }

    /// `/api/v1/hls/{session}/control` and nothing else. A server-relative
    /// path with exactly one session segment cannot be steered to another
    /// origin, another route, or a traversal.
    static func isSessionControlPath(_ url: String) -> Bool {
        let parts = url.split(separator: "/", omittingEmptySubsequences: false)
        guard parts.count == 6 else { return false }
        return parts[0].isEmpty
            && parts[1] == "api"
            && parts[2] == "v1"
            && parts[3] == "hls"
            && !parts[4].isEmpty
            && !parts[4].contains(".")
            && parts[5] == "control"
    }
}

enum PlaybackDemand: String, Codable, Equatable {
    case active, hold, end
}

enum RenderState: String, Codable, Equatable {
    case starting, rendering, waiting, stalled, seeking, ended, failed
}

enum CodecPolicy: String, Codable, Equatable {
    case auto, h264, hevc, av1
}

enum DynamicRangePolicy: String, Codable, Equatable {
    case auto
    case dolbyVision = "dolby_vision"
    case hdr10, hlg, sdr
}

enum SubtitleMode: String, Codable, Equatable {
    case off, native, overlay, burn
}

enum DecoderState: String, Codable, Equatable {
    case unknown, ready, starved, failed
}

enum ClientErrorCode: String, Codable, Equatable {
    case network, manifest, media, decoder, drm, unknown
}

/// `{"mode": "auto"}`, `{"mode": "original"}`, or
/// `{"mode": "manual", "height": 1080}` — the server's
/// internally tagged `QualitySelection`.
enum QualitySelection: Codable, Equatable {
    case auto
    case original
    case manual(height: Int)

    private enum CodingKeys: String, CodingKey { case mode, height }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .mode) {
        case "auto": self = .auto
        case "original": self = .original
        case "manual": self = .manual(height: try container.decode(Int.self, forKey: .height))
        default:
            throw DecodingError.dataCorruptedError(
                forKey: .mode, in: container, debugDescription: "unknown quality mode"
            )
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .auto:
            try container.encode("auto", forKey: .mode)
        case .original:
            try container.encode("original", forKey: .mode)
        case .manual(let height):
            try container.encode("manual", forKey: .mode)
            try container.encode(height, forKey: .height)
        }
    }

    var isValid: Bool {
        guard case .manual(let height) = self else { return true }
        return (PlaybackControl.minimumHeight...PlaybackControl.maximumHeight).contains(height)
    }
}

struct SubtitleSelection: Codable, Equatable {
    var mode: SubtitleMode
    var track: Int?

    var isValid: Bool {
        if let track, !(0...1_024).contains(track) { return false }
        if mode == .off && track != nil { return false }
        return true
    }
}

struct ClientSelection: Codable, Equatable {
    var quality: QualitySelection
    var audioTrack: Int?
    var subtitle: SubtitleSelection
    var audioOffsetMs: Int
    var codec: CodecPolicy
    var dynamicRange: DynamicRangePolicy

    var isValid: Bool {
        if let audioTrack, !(0...1_024).contains(audioTrack) { return false }
        return quality.isValid && subtitle.isValid
    }
}

struct DynamicCapabilities: Codable, Equatable {
    var platform: String
    var maxHeight: Int
    var codecs: [CodecPolicy]
    var dynamicRanges: [DynamicRangePolicy]
    var dualPlayerPreparation: Bool

    var isValid: Bool {
        (PlaybackControl.minimumHeight...PlaybackControl.maximumHeight).contains(maxHeight)
            && !codecs.isEmpty
            && codecs.count <= PlaybackControl.maximumCapabilityValues
            && !dynamicRanges.isEmpty
            && dynamicRanges.count <= PlaybackControl.maximumCapabilityValues
    }
}

struct ClientObservation: Codable, Equatable {
    var droppedFrames: Int?
    var decoderState: DecoderState?
    var errorCode: ClientErrorCode?
    var errorDetail: String?

    var isEmpty: Bool {
        droppedFrames == nil && decoderState == nil && errorCode == nil && errorDetail == nil
    }

    /// Detail is free text from a player error, so it is bounded and stripped
    /// of the control characters that would let it forge a log line. Detail
    /// without a code is dropped entirely: the server rejects that pairing.
    var bounded: ClientObservation? {
        var value = ClientObservation()
        if let droppedFrames, droppedFrames >= 0 { value.droppedFrames = droppedFrames }
        value.decoderState = decoderState
        value.errorCode = errorCode
        if value.errorCode != nil, let detail = errorDetail {
            // Scalars, not Characters: "\r\n" is one grapheme cluster, so a
            // Character-wise replacement leaves a CRLF intact and a log line
            // forgeable. The web reporter works on UTF-16 code units for the
            // same reason.
            var flattened = String.UnicodeScalarView()
            var bytes = 0
            for scalar in detail.unicodeScalars {
                let replacement: Unicode.Scalar =
                    (scalar == "\r" || scalar == "\n" || scalar == "\0") ? " " : scalar
                let width = String(replacement).utf8.count
                if bytes + width > PlaybackControl.maximumErrorDetailBytes { break }
                flattened.append(replacement)
                bytes += width
            }
            let truncated = String(flattened)
            if !truncated.isEmpty { value.errorDetail = truncated }
        }
        return value.isEmpty ? nil : value
    }
}

/// What the player is doing right now. The reporter reads one of these each
/// time it is ready to speak, so a snapshot is always the newest truth rather
/// than a queued history.
struct PlaybackControlSnapshot: Equatable {
    var demand: PlaybackDemand
    var positionMs: Int
    var bufferedFromMs: Int?
    var bufferedThroughMs: Int
    var playbackRate: Double
    var renderState: RenderState
    var seekTargetMs: Int?
    var observedDownloadBps: Int64?
    var selection: ClientSelection
    var capabilities: DynamicCapabilities
    var observation: ClientObservation?

    var isValid: Bool {
        positionMs >= 0
            && bufferedThroughMs >= positionMs
            && playbackRate.isFinite
            && playbackRate >= 0
            && (bufferedFromMs.map { $0 >= 0 && $0 <= bufferedThroughMs } ?? true)
            && (seekTargetMs.map { $0 >= 0 } ?? true)
            && (observedDownloadBps.map {
                $0 >= 0 && $0 <= PlaybackControl.maximumObservedDownloadBps
            } ?? true)
            && selection.isValid
            && capabilities.isValid
    }
}

struct ControlRequest: Codable, Equatable {
    var proto: String
    var generation: String
    var controlEpoch: Int
    var clientInstanceId: String
    var sequence: Int
    var demand: PlaybackDemand
    var positionMs: Int
    var bufferedFromMs: Int?
    var bufferedThroughMs: Int
    var playbackRate: Double
    var renderState: RenderState
    var seekTargetMs: Int?
    var observedDownloadBps: Int64?
    var selection: ClientSelection
    var capabilities: DynamicCapabilities?
    var observation: ClientObservation?
    var supportedActions: [String] = PlaybackControl.supportedActions

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, clientInstanceId, sequence, demand
        case positionMs, bufferedFromMs, bufferedThroughMs, playbackRate
        case renderState, seekTargetMs, observedDownloadBps
        case selection, capabilities, observation, supportedActions
    }
}

struct ControlAction: Codable, Equatable {
    var type: String
    /// Present on `hold` and `retry_resource`, and diagnostic rather than
    /// dispositive: a reason this client has never heard of is a newer server,
    /// not a broken one.
    var reason: String? = nil
    /// `retry_resource` only: when the server wants to be asked again.
    var afterMs: Int? = nil
    /// `terminal` only: which producer decision ended this session, and a
    /// bounded sentence explaining it.
    var code: String? = nil
    var message: String? = nil
}

struct ControlDelivery: Codable, Equatable {
    /// Extensible relay value: only the exact value `ready` has client meaning.
    var subtitleReadiness: String?
}

enum SubtitleReadinessDecision {
    static func meansReady(_ value: String?) -> Bool { value == "ready" }
}

/// Turns a non-ready → ready edge into one retry and suppresses repeated
/// control cadence at `ready`. The reporter actor records into this locked
/// bridge; the player consumes the edge on MainActor.
final class SubtitleReadinessRetryState: @unchecked Sendable {
    private let lock = NSLock()
    private var lastReady: Bool?

    func record(_ value: String?) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let ready = SubtitleReadinessDecision.meansReady(value)
        defer { lastReady = ready }
        return lastReady == false && ready
    }
}

struct ControlResponse: Codable, Equatable {
    var proto: String
    var generation: String
    var controlEpoch: Int
    var acceptedSequence: Int
    var delivery: ControlDelivery? = nil
    var action: ControlAction

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, acceptedSequence, delivery, action
    }
}

/// A transport failure the reporter can classify. `status` is the HTTP status
/// where there was one and `nil` where the request never reached a server;
/// `code` is the server's typed `ControlErrorBody.code`.
struct ControlTransportError: Error, Equatable {
    var status: Int?
    var code: String?
    var generation: String?
    var controlEpoch: Int?
    var retryAfterMs: Int?
    var canceled: Bool = false

    static let deadlineExceeded = ControlTransportError(status: 408, code: "exchange_deadline")
}

/// The reporter refuses a response it cannot bind to the request it sent.
/// This is terminal on purpose: a mismatched generation or a non-`none`
/// action means something other than this session's owner is answering.
struct ControlProtocolError: Error, Equatable {
    var reason: String
}

// MARK: - Reporter

/// One in-flight exchange, newest-snapshot-wins coalescing, monotonic
/// sequences, and a bounded exchange deadline.
///
/// An actor rather than a lock because every rule here is about ordering:
/// exactly one exchange may be outstanding, a snapshot that arrives during an
/// exchange replaces any other waiting snapshot instead of queueing behind
/// it, and the sequence a request carries must never be reused or skipped.
actor PlaybackControlReporter {
    typealias Send = @Sendable (String, ControlRequest) async throws -> ControlResponse
    typealias Sleep = @Sendable (Int, SleepKind) async throws -> Void

    /// Why the reporter is waiting. Pacing waits are the exchange cadence and
    /// the retry backoff; the deadline wait races one in-flight exchange. They
    /// are named rather than merged so a test can hold one still while it
    /// drives the other, and so a caller reading a trace can tell a slow
    /// server from a quiet player.
    enum SleepKind: Sendable, Equatable {
        case pacing
        case deadline
    }

    struct Exchange: Equatable {
        var request: ControlRequest
        var response: ControlResponse?
        var failure: String?
        /// Local-only viewer intent that produced this request.
        var intentGeneration = 0
    }

    private struct PendingSnapshot {
        var snapshot: PlaybackControlSnapshot
        var intentGeneration: Int
    }

    private struct PendingRequest {
        var request: ControlRequest
        var intentGeneration: Int
    }

    private(set) var bootstrap: ControlBootstrap
    private let clientInstanceId: String
    private let snapshot: @Sendable () -> PlaybackControlSnapshot?
    private let send: Send
    private let sleep: Sleep
    private let now: @Sendable () -> Int
    private let intentGeneration: @Sendable () -> Int
    private let onExchange: @Sendable (Exchange) -> Void

    private(set) var sequence = 0
    private(set) var acceptedSequence = 0
    private(set) var stopped = false
    private var inFlight = false
    private var pending: PendingSnapshot?
    private var retryRequest: PendingRequest?
    private var acceptedCapabilities: DynamicCapabilities?
    private var lastStartedAt: Int?
    private var nextAllowedAt = 0
    private var pump: Task<Void, Never>?

    init?(
        bootstrap: ControlBootstrap,
        clientInstanceId: String,
        snapshot: @escaping @Sendable () -> PlaybackControlSnapshot?,
        send: @escaping Send,
        sleep: @escaping Sleep,
        now: @escaping @Sendable () -> Int,
        intentGeneration: @escaping @Sendable () -> Int = { 0 },
        onExchange: @escaping @Sendable (Exchange) -> Void = { _ in }
    ) {
        guard bootstrap.isValid, PlaybackControl.isUUID(clientInstanceId) else { return nil }
        self.bootstrap = bootstrap
        self.clientInstanceId = clientInstanceId
        self.snapshot = snapshot
        self.send = send
        self.sleep = sleep
        self.now = now
        self.intentGeneration = intentGeneration
        self.onExchange = onExchange
    }

    /// Report the current state now, and keep reporting until the player ends
    /// or the reporter is stopped. Idempotent: a second call while the pump is
    /// alive is a no-op rather than a second exchange loop.
    func start() {
        guard !stopped, pump == nil else { return }
        if pending == nil, let snapshot = snapshot() {
            pending = PendingSnapshot(snapshot: snapshot, intentGeneration: intentGeneration())
        }
        pump = Task { [weak self] in await self?.run() }
    }

    /// The player changed. The newest snapshot replaces any waiting one — an
    /// intermediate position between two exchanges is not worth a round trip,
    /// and reporting it late would be worse than not reporting it.
    func notify(_ value: PlaybackControlSnapshot? = nil) {
        guard !stopped else { return }
        guard let newest = value ?? snapshot(), newest.isValid else { return }
        pending = PendingSnapshot(snapshot: newest, intentGeneration: intentGeneration())
    }

    /// Report now rather than at the next cadence.
    ///
    /// `notify` leaves the pump asleep for `next_exchange_ms`, which the
    /// server may set as high as a minute. For a position update that is the
    /// point. For a recovery owner about to reopen it is fatal: the reopen
    /// ends this reporter before the pump wakes, so the evidence is never
    /// sent at all rather than sent late.
    ///
    /// Waking is a cancel-and-restart because the pump is suspended inside an
    /// injected sleep. `run()`'s loop condition reads `Task.isCancelled`, so
    /// the old pump exits at its next iteration instead of racing this one.
    /// An exchange already in flight is left alone — `run()` picks up
    /// `pending` immediately after it, without sleeping.
    @discardableResult
    func notifyUrgently(_ value: PlaybackControlSnapshot? = nil) -> Int? {
        notify(value)
        guard !stopped, pending != nil else { return nil }
        // A retry can replay the current sequence, but it cannot consume the
        // next new request. A replacement retains this floor even if ending
        // this reporter cancels the pump before it reaches the server.
        let floor = sequence + 1
        if !inFlight {
            pump?.cancel()
            pump = Task { [weak self] in await self?.run() }
        }
        return floor
    }

    func stop() {
        guard !stopped else { return }
        stopped = true
        pending = nil
        retryRequest = nil
        pump?.cancel()
        pump = nil
    }

    func status() -> [String: Int] {
        [
            "sequence": sequence,
            "accepted_sequence": acceptedSequence,
            "in_flight": inFlight ? 1 : 0,
            "pending": pending == nil ? 0 : 1,
            "retrying": retryRequest == nil ? 0 : 1,
            "stopped": stopped ? 1 : 0,
        ]
    }

    // MARK: exchange loop

    private func run() async {
        while !stopped && !Task.isCancelled {
            if pending == nil && retryRequest == nil {
                if let snapshot = snapshot() {
                    pending = PendingSnapshot(
                        snapshot: snapshot,
                        intentGeneration: intentGeneration()
                    )
                }
            }
            guard pending != nil || retryRequest != nil else {
                try? await sleep(bootstrap.nextExchangeMs, .pacing)
                continue
            }
            let wait = waitBeforeNextExchange()
            if wait > 0 {
                try? await sleep(wait, .pacing)
                continue
            }
            guard let request = nextRequest() else {
                try? await sleep(bootstrap.nextExchangeMs, .pacing)
                continue
            }
            await exchange(request)
            if stopped { return }
            if pending == nil && retryRequest == nil {
                try? await sleep(bootstrap.nextExchangeMs, .pacing)
            }
        }
    }

    private func waitBeforeNextExchange() -> Int {
        let rateAllowedAt = lastStartedAt.map { $0 + PlaybackControl.minimumExchangeMs } ?? 0
        return max(0, max(rateAllowedAt, nextAllowedAt) - now())
    }

    /// A retry replays the exact request that failed — the same sequence, the
    /// same body — because a control exchange the server never accepted must
    /// not consume a sequence number, and the server dedupes on it.
    private func nextRequest() -> PendingRequest? {
        if let retryRequest { return retryRequest }
        guard let pending, pending.snapshot.isValid else {
            pending = nil
            return nil
        }
        self.pending = nil
        sequence += 1
        let snapshot = pending.snapshot
        var request = ControlRequest(
            proto: PlaybackControl.protocolName,
            generation: bootstrap.generation,
            controlEpoch: bootstrap.controlEpoch,
            clientInstanceId: clientInstanceId,
            sequence: sequence,
            demand: snapshot.demand,
            positionMs: snapshot.positionMs,
            bufferedFromMs: snapshot.bufferedFromMs,
            bufferedThroughMs: snapshot.bufferedThroughMs,
            playbackRate: snapshot.playbackRate,
            renderState: snapshot.renderState,
            seekTargetMs: snapshot.seekTargetMs,
            observedDownloadBps: snapshot.observedDownloadBps,
            selection: snapshot.selection,
            capabilities: snapshot.capabilities,
            observation: snapshot.observation?.bounded,
            supportedActions: PlaybackControl.supportedActions
        )
        // Capabilities are static for the life of a player. Repeating them on
        // every exchange is bytes the server already has; the first request of
        // a generation must carry them, and a change must resend them.
        if request.sequence != 1 && snapshot.capabilities == acceptedCapabilities {
            request.capabilities = nil
        }
        return PendingRequest(request: request, intentGeneration: pending.intentGeneration)
    }

    private func exchange(_ pendingRequest: PendingRequest) async {
        let request = pendingRequest.request
        nextAllowedAt = 0
        lastStartedAt = now()
        inFlight = true
        defer { inFlight = false }
        do {
            let response = try await withDeadline(PlaybackControl.exchangeDeadlineMs) {
                [send, bootstrap] in
                try await send(bootstrap.url, request)
            }
            if stopped { return }
            try accept(request: request, response: response)
            retryRequest = nil
            if let capabilities = request.capabilities { acceptedCapabilities = capabilities }
            acceptedSequence = max(acceptedSequence, response.acceptedSequence)
            // `retry_resource` paces the next exchange from the server's own
            // cadence rather than this client's guess. The exchange succeeded;
            // the server only said when to ask again, so this does not touch
            // the retry path.
            if response.action.type == "retry_resource", let afterMs = response.action.afterMs {
                nextAllowedAt = now() + max(PlaybackControl.minimumExchangeMs, afterMs)
            }
            onExchange(Exchange(
                request: request,
                response: response,
                failure: nil,
                intentGeneration: pendingRequest.intentGeneration
            ))
            // A terminal verdict ends reporting. It does not tear the player
            // down: this reporter still owns no recovery, and buffer already
            // fetched is still worth playing. The milestone that moves that
            // authority is the one that acts on this.
            if request.demand == .end ||
                (response.action.type == "terminal" &&
                    pendingRequest.intentGeneration == intentGeneration()) {
                stop()
            }
        } catch {
            if stopped { return }
            handle(failure: error, for: pendingRequest)
        }
    }

    private func accept(request: ControlRequest, response: ControlResponse) throws {
        guard response.proto == PlaybackControl.protocolName else {
            throw ControlProtocolError(reason: "protocol")
        }
        guard response.generation == request.generation else {
            throw ControlProtocolError(reason: "generation")
        }
        guard response.controlEpoch == request.controlEpoch else {
            throw ControlProtocolError(reason: "control_epoch")
        }
        guard response.acceptedSequence == request.sequence else {
            throw ControlProtocolError(reason: "accepted_sequence")
        }
        // An action outside the declared vocabulary means the server and this
        // client disagree about the contract, and continuing would be
        // guessing. A `hold` is inside it: the server is explaining that
        // production is deliberately not advancing, which is the opposite of a
        // reason to stop reporting. Its reason must be present but need not be
        // one this client recognises.
        switch response.action.type {
        case "none":
            break
        case "hold":
            guard response.action.reason != nil else {
                throw ControlProtocolError(reason: "action")
            }
        case "terminal":
            // An action inside the declared vocabulary but missing the field
            // this client acts on is worse than one it has never heard of,
            // because it would be acted on.
            guard response.action.code != nil, response.action.message != nil else {
                throw ControlProtocolError(reason: "action")
            }
        case "retry_resource":
            guard response.action.reason != nil,
                let afterMs = response.action.afterMs,
                afterMs > 0,
                afterMs <= PlaybackControl.maximumExchangeMs
            else {
                throw ControlProtocolError(reason: "action")
            }
        default:
            throw ControlProtocolError(reason: "action")
        }
    }

    private func handle(failure: Error, for pendingRequest: PendingRequest) {
        let request = pendingRequest.request
        if let transport = failure as? ControlTransportError, transport.canceled { return }
        onExchange(
            Exchange(
                request: request,
                response: nil,
                failure: describe(failure),
                intentGeneration: pendingRequest.intentGeneration
            )
        )
        if failure is ControlProtocolError {
            stop()
            return
        }
        let transport = failure as? ControlTransportError
        let status = transport?.status
        let code = transport?.code
        if status == 409, code == "owner_changed", adoptNewOwner(transport) {
            nextAllowedAt = now() + retryDelay(transport, PlaybackControl.minimumExchangeMs)
            return
        }
        let retryableControl = (status == 425 && code == "owner_transition")
            || (status == 429 && code == "control_rate_limited")
            || (status == 503 && code == "control_unavailable")
        let retryableTransport = status == 408 || status == nil
        guard retryableControl || retryableTransport else {
            stop()
            return
        }
        retryRequest = pendingRequest
        let fallback = retryableControl ? 500 : bootstrap.nextExchangeMs
        nextAllowedAt = now() + retryDelay(transport, fallback)
    }

    /// A 409 names the generation and epoch that now own the session. Adopting
    /// them restarts the sequence at zero for the new owner rather than
    /// carrying a number the new owner never issued — but only when the server
    /// actually named a newer owner, so a malformed 409 cannot silently
    /// rebind this player to another session.
    private func adoptNewOwner(_ error: ControlTransportError?) -> Bool {
        guard let error,
              let generation = error.generation,
              let epoch = error.controlEpoch,
              PlaybackControl.isUUID(generation),
              epoch > 0
        else { return false }
        let generationChanged = generation != bootstrap.generation
        let epochChanged = epoch > bootstrap.controlEpoch
        guard generationChanged || epochChanged else { return false }
        guard let newest = snapshot(), newest.isValid else { return false }
        bootstrap.generation = generation
        bootstrap.controlEpoch = epoch
        sequence = 0
        acceptedSequence = 0
        retryRequest = nil
        acceptedCapabilities = nil
        lastStartedAt = nil
        pending = PendingSnapshot(
            snapshot: newest,
            intentGeneration: intentGeneration()
        )
        return true
    }

    private func retryDelay(_ error: ControlTransportError?, _ fallback: Int) -> Int {
        guard let value = error?.retryAfterMs,
              value >= 0,
              value <= PlaybackControl.maximumExchangeMs
        else { return fallback }
        return max(PlaybackControl.minimumExchangeMs, value)
    }

    private func describe(_ error: Error) -> String {
        if let error = error as? ControlProtocolError { return "protocol:\(error.reason)" }
        if let error = error as? ControlTransportError {
            return "transport:\(error.status.map(String.init) ?? "none"):\(error.code ?? "-")"
        }
        return "transport:none:-"
    }

    /// The exchange itself is bounded, not just the socket: a server that
    /// accepts the request and then never answers must not hold the only
    /// in-flight slot open forever.
    private func withDeadline<T: Sendable>(
        _ milliseconds: Int,
        _ work: @escaping @Sendable () async throws -> T
    ) async throws -> T {
        let sleep = self.sleep
        return try await withThrowingTaskGroup(of: T.self) { group in
            group.addTask { try await work() }
            group.addTask {
                try await sleep(milliseconds, .deadline)
                throw ControlTransportError.deadlineExceeded
            }
            defer { group.cancelAll() }
            guard let first = try await group.next() else {
                throw ControlTransportError.deadlineExceeded
            }
            return first
        }
    }
}
