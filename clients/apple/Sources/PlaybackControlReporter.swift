import Foundation

/// Apple's playback-control reporter.
///
/// M2's contract is one coalescing controller per platform that reports what
/// the player is actually doing — intent, film position, contiguous runway,
/// render state, selection, and throughput evidence. Its sequencing and
/// coalescing remain the port of the web state machine; platform-specific
/// prepared-successor lifecycle lives beside that common exchange loop.
///
/// The prepared-switch vocabulary is active only at the message boundary in
/// this slice. It can receive an offer and carry progress through commit, but
/// deliberately creates no second player: the server does not yet start a
/// producer behind the offered route. `onPreparation` is the seam the player
/// will consume when that producer exists.
enum PlaybackControl {
    static let protocolName = "plurx-playback-control-v1"
    /// The actions this client will accept, and therefore the only ones the
    /// server will send it. An action that is never declared is never sent, so
    /// a client cannot be silenced by one it does not understand.
    static let supportedActions = ["hold", "retry_resource", "terminal", "prepare_replacement"]
    static let minimumExchangeMs = 250
    static let maximumExchangeMs = 60_000
    static let exchangeDeadlineMs = 6_000
    static let maximumLeaseTimeoutMs = 600_000
    static let minimumHeight = 144
    static let maximumHeight = 2_160
    static let maximumCapabilityValues = 8
    static let maximumObservedDownloadBps: Int64 = 10_000_000_000_000
    static let maximumMediaMs = 366 * 24 * 60 * 60 * 1_000
    static let maximumPlaylistURLBytes = 512
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

enum EffectiveCodec: String, Codable, Equatable, Sendable {
    case source
    case serverSelected = "server_selected"
}

enum EffectiveDynamicRange: String, Codable, Equatable, Sendable {
    case dolbyVision = "dolby_vision"
    case hdr10, hlg, sdr
}

/// The complete selection attached to a prepared successor. These are not the
/// viewer's policies: they are the concrete answers the server staged.
struct EffectiveSelection: Codable, Equatable, Sendable {
    var qualityAuto: Bool
    var height: Int
    var audioTrack: Int?
    var subtitleBurn: Int?
    var audioOffsetMs: Int
    var codec: EffectiveCodec
    var dynamicRange: EffectiveDynamicRange?

    var isValid: Bool {
        (0...PlaybackControl.maximumHeight).contains(height)
            && (audioTrack.map { (0...1_024).contains($0) } ?? true)
            && (subtitleBurn.map { (0...1_024).contains($0) } ?? true)
            && (-15_000...15_000).contains(audioOffsetMs)
    }
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
    var acknowledgement: ActionAcknowledgement? = nil
    var supportedActions: [String] = PlaybackControl.supportedActions

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, clientInstanceId, sequence, demand
        case positionMs, bufferedFromMs, bufferedThroughMs, playbackRate
        case renderState, seekTargetMs, observedDownloadBps
        case selection, capabilities, observation, acknowledgement, supportedActions
    }
}

enum AcknowledgementState: String, Codable, Equatable, Sendable {
    case metadataReady = "metadata_ready"
    case bufferReady = "buffer_ready"
    case committed
    case failed
    case aborted
}

struct ActionAcknowledgement: Codable, Equatable, Sendable {
    var actionId: String
    var state: AcknowledgementState
    var bufferedThroughMs: Int? = nil
    var committedMediaOriginMs: Int? = nil
    var firstFrameUnixMs: Int? = nil

    var isValid: Bool {
        guard PlaybackControl.isUUID(actionId),
              bufferedThroughMs.map({ (0...PlaybackControl.maximumMediaMs).contains($0) }) ?? true,
              committedMediaOriginMs.map({
                  (0...PlaybackControl.maximumMediaMs).contains($0)
              }) ?? true,
              firstFrameUnixMs.map({ $0 > 0 }) ?? true
        else { return false }
        switch state {
        case .bufferReady:
            return bufferedThroughMs != nil
        case .committed:
            return firstFrameUnixMs != nil && committedMediaOriginMs != nil
        case .metadataReady, .failed, .aborted:
            return true
        }
    }
}

struct PreparedSwitchOffer: Equatable, Sendable {
    var actionId: String
    var sessionId: String
    var playlistURL: String
    var mediaOriginMs: Int
    var effectiveSelection: EffectiveSelection

    fileprivate var isValid: Bool {
        guard PlaybackControl.isUUID(actionId), PlaybackControl.isUUID(sessionId),
              playlistURL.utf8.count <= PlaybackControl.maximumPlaylistURLBytes,
              (0...PlaybackControl.maximumMediaMs).contains(mediaOriginMs),
              effectiveSelection.isValid
        else { return false }
        let path = playlistURL.split(whereSeparator: { $0 == "?" || $0 == "#" }).first
            .map(String.init) ?? playlistURL
        return path == "/api/v1/hls/\(sessionId)/index.m3u8"
            || path == "/api/v1/hls/\(sessionId)/master.m3u8"
    }
}

enum PreparedSwitchReleaseReason: Equatable, Sendable {
    case withdrawn
    case replaced
    case committed
    case acknowledgementDiscarded
    case staleControl
    case reporterStopped
}

enum PreparedSwitchEvent: Equatable, Sendable {
    case offered(PreparedSwitchOffer)
    case released(actionId: String, reason: PreparedSwitchReleaseReason)
}

struct ControlAction: Codable, Equatable {
    var type: String
    /// Present on `hold` and `retry_resource`, and diagnostic rather than
    /// dispositive: a reason this client has never heard of is a newer server,
    /// not a broken one.
    var reason: String? = nil
    /// `retry_resource` only: when the server wants to be asked again.
    var afterMs: Int? = nil
    /// `hold` only: when the server expects to be worth asking again.
    ///
    /// A revisit contract, not an expiry — nothing fails when it passes. It
    /// exists because `no_room` is cleared by something outside this session,
    /// so a client told to hold for that reason has no way of its own to know
    /// when asking again is worth the exchange.
    var revisitAfterMs: Int? = nil
    /// `terminal` only: which producer decision ended this session, and a
    /// bounded sentence explaining it.
    var code: String? = nil
    var message: String? = nil
    /// `prepare` only. Optional here so malformed shapes decode and are then
    /// rejected as a protocol error by `accept`, rather than being mistaken
    /// for a transport failure.
    var actionId: String? = nil
    var sessionId: String? = nil
    var playlistUrl: String? = nil
    var mediaOriginMs: Int? = nil
    var effectiveSelection: EffectiveSelection? = nil

    var preparedOffer: PreparedSwitchOffer? {
        guard type == "prepare", let actionId, let sessionId, let playlistUrl,
              let mediaOriginMs, let effectiveSelection
        else { return nil }
        return PreparedSwitchOffer(
            actionId: actionId,
            sessionId: sessionId,
            playlistURL: playlistUrl,
            mediaOriginMs: mediaOriginMs,
            effectiveSelection: effectiveSelection
        )
    }
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

    func record(_ value: String?, commitReady: Bool = true) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        let ready = SubtitleReadinessDecision.meansReady(value)
        defer { if !ready || commitReady { lastReady = ready } }
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
    var invalidField: String?
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

/// Local ownership is deliberately separate from the server's 409 generation.
struct PlaybackControlCaptureOwner: Equatable, Sendable {
    let lifecycleId: String
    let attachmentGeneration: Int
}

/// A source-actor value, never a snapshot labelled after an actor hop. Swift
/// value semantics keep nested capabilities and selections immutable too.
struct PlaybackControlCapture: Equatable, Sendable {
    let snapshot: PlaybackControlSnapshot
    let intentGeneration: Int
    let owner: PlaybackControlCaptureOwner
    let sourceRevision: Int

    func hasSameIntent(as other: Self?) -> Bool {
        guard let other else { return false }
        return owner == other.owner && intentGeneration == other.intentGeneration
    }
}

/// One in-flight exchange, newest-capture-wins coalescing, monotonic
/// sequences, and a bounded exchange deadline. The actor owns ordering, not
/// source observation: all player reads happen before the capture reaches it.
actor PlaybackControlReporter {
    typealias Send = @Sendable (String, ControlRequest) async throws -> ControlResponse
    typealias Sleep = @Sendable (Int, SleepKind) async throws -> Void
    typealias OnPreparation = @Sendable (PreparedSwitchEvent) -> Void

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
        let capture: PlaybackControlCapture
        var intentGeneration: Int { capture.intentGeneration }
    }

    private struct PendingRequest {
        var request: ControlRequest
        let capture: PlaybackControlCapture
    }

    private struct ActivePreparation {
        var offer: PreparedSwitchOffer
        /// The exact request selection against which the offer was staged.
        var selection: ClientSelection
        var acceptedState: AcknowledgementState?
    }

    private(set) var bootstrap: ControlBootstrap
    private let clientInstanceId: String
    private let owner: PlaybackControlCaptureOwner
    private let capture: @Sendable () -> PlaybackControlCapture?
    private let send: Send
    private let sleep: Sleep
    private let now: @Sendable () -> Int
    private let onExchange: @Sendable (Exchange) -> Void
    private let onPreparation: OnPreparation

    private(set) var sequence = 0
    private(set) var acceptedSequence = 0
    private(set) var stopped = false
    private var terminalStop: PlaybackControlCapture?
    private var inFlight = false
    private var pending: PlaybackControlCapture?
    private var retryRequest: PendingRequest?
    private var acceptedCapabilities: DynamicCapabilities?
    private var lastStartedAt: Int?
    private var nextAllowedAt = 0
    private var pump: Task<Void, Never>?
    private var activePreparation: ActivePreparation?
    private var pendingAcknowledgement: ActionAcknowledgement?
    /// A terminal acknowledgement that comes back with the same offer was
    /// silently discarded. Keep its id tombstoned until the server withdraws
    /// it so cadence cannot build the same successor again.
    private var ignoredPreparationActionId: String?

    init?(
        bootstrap: ControlBootstrap,
        clientInstanceId: String,
        owner: PlaybackControlCaptureOwner,
        capture: @escaping @Sendable () -> PlaybackControlCapture?,
        send: @escaping Send,
        sleep: @escaping Sleep,
        now: @escaping @Sendable () -> Int,
        onExchange: @escaping @Sendable (Exchange) -> Void = { _ in },
        onPreparation: @escaping OnPreparation = { _ in }
    ) {
        guard bootstrap.isValid, PlaybackControl.isUUID(clientInstanceId) else { return nil }
        self.bootstrap = bootstrap
        self.clientInstanceId = clientInstanceId
        self.owner = owner
        self.capture = capture
        self.send = send
        self.sleep = sleep
        self.now = now
        self.onExchange = onExchange
        self.onPreparation = onPreparation
    }

    /// Report the current state now, and keep reporting until the player ends
    /// or the reporter is stopped. Idempotent: a second call while the pump is
    /// alive is a no-op rather than a second exchange loop.
    func start() {
        guard !stopped, pump == nil else { return }
        if pending == nil { pending = newestCapture(nil) }
        pump = Task { [weak self] in await self?.run() }
    }

    /// The player changed. The newest snapshot replaces any waiting one — an
    /// intermediate position between two exchanges is not worth a round trip,
    /// and reporting it late would be worse than not reporting it.
    func notify(_ value: PlaybackControlCapture? = nil) {
        guard let newest = newestCapture(value), newest.snapshot.isValid else { return }
        let resume = stopped
        guard admitAfterTerminal(newest) else { return }
        pending = newest
        if resume { pump = Task { [weak self] in await self?.run() } }
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
    func notifyUrgently(_ value: PlaybackControlCapture? = nil) -> Int? {
        guard let newest = newestCapture(value), newest.snapshot.isValid,
              admitAfterTerminal(newest) else { return nil }
        pending = newest
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
        terminalStop = nil
        guard !stopped else { return }
        releasePreparation(.reporterStopped)
        stopped = true
        pending = nil
        retryRequest = nil
        pump?.cancel()
        pump = nil
    }

    /// A terminal stops only the captured intent. MainActor may publish B
    /// immediately after this check, so B's notification can resume this
    /// latch. Explicit End/stop and protocol failure remain permanent.
    private func stopForTerminal(_ captured: PlaybackControlCapture) {
        guard !stopped, captured.hasSameIntent(as: newestCapture(pending)) else { return }
        releasePreparation(.reporterStopped)
        stopped = true
        terminalStop = captured
        pending = nil
        retryRequest = nil
        pump?.cancel()
        pump = nil
    }

    private func admitAfterTerminal(_ newest: PlaybackControlCapture) -> Bool {
        guard stopped else { return true }
        guard let terminalStop, !terminalStop.hasSameIntent(as: newest) else { return false }
        self.terminalStop = nil
        stopped = false
        return true
    }

    func status() -> [String: Int] {
        [
            "sequence": sequence,
            "accepted_sequence": acceptedSequence,
            "in_flight": inFlight ? 1 : 0,
            "pending": pending == nil ? 0 : 1,
            "retrying": retryRequest == nil ? 0 : 1,
            "preparing": activePreparation == nil ? 0 : 1,
            "stopped": stopped ? 1 : 0,
        ]
    }

    var preparedOffer: PreparedSwitchOffer? { activePreparation?.offer }

    /// Future player seam. These methods only queue wire progress; this slice
    /// never creates, buffers, or presents a second AVPlayer.
    @discardableResult
    func preparationMetadataReady(actionId: String) -> Bool {
        queuePreparationAcknowledgement(ActionAcknowledgement(
            actionId: actionId, state: .metadataReady
        ))
    }

    @discardableResult
    func preparationBufferReady(actionId: String, bufferedThroughMs: Int) -> Bool {
        queuePreparationAcknowledgement(ActionAcknowledgement(
            actionId: actionId, state: .bufferReady, bufferedThroughMs: bufferedThroughMs
        ))
    }

    @discardableResult
    func preparationCommitted(actionId: String, firstFrameUnixMs: Int) -> Bool {
        guard let activePreparation, activePreparation.offer.actionId == actionId else { return false }
        return queuePreparationAcknowledgement(ActionAcknowledgement(
            actionId: actionId,
            state: .committed,
            committedMediaOriginMs: activePreparation.offer.mediaOriginMs,
            firstFrameUnixMs: firstFrameUnixMs
        ))
    }

    @discardableResult
    func preparationFailed(actionId: String) -> Bool {
        queuePreparationAcknowledgement(ActionAcknowledgement(
            actionId: actionId, state: .failed
        ))
    }

    @discardableResult
    func preparationAborted(actionId: String) -> Bool {
        queuePreparationAcknowledgement(ActionAcknowledgement(
            actionId: actionId, state: .aborted
        ))
    }

    /// Internal protocol entry point kept visible to tests so the silent
    /// wrong-origin discard can be proved. Production callers should use the
    /// typed methods above; `preparationCommitted` always echoes the offer.
    @discardableResult
    func queuePreparationAcknowledgement(_ acknowledgement: ActionAcknowledgement) -> Bool {
        guard !stopped, acknowledgement.isValid,
              pendingAcknowledgement == nil,
              let activePreparation,
              activePreparation.offer.actionId == acknowledgement.actionId
        else { return false }
        switch acknowledgement.state {
        case .metadataReady:
            guard activePreparation.acceptedState == nil else { return false }
        case .bufferReady:
            guard activePreparation.acceptedState == .metadataReady else { return false }
        case .committed:
            guard activePreparation.acceptedState == .bufferReady else { return false }
            guard currentCapture()?.snapshot.selection == activePreparation.selection else {
                return queuePreparationAcknowledgement(ActionAcknowledgement(
                    actionId: acknowledgement.actionId, state: .aborted
                ))
            }
        case .failed, .aborted:
            break
        }
        pendingAcknowledgement = acknowledgement
        pending = newestCapture(pending)
        if !inFlight {
            pump?.cancel()
            pump = Task { [weak self] in await self?.run() }
        }
        return true
    }

    // MARK: exchange loop

    private func run() async {
        while !stopped && !Task.isCancelled {
            if pending == nil && retryRequest == nil {
                pending = newestCapture(nil)
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
        guard let pending = newestCapture(pending), pending.snapshot.isValid else {
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
            acknowledgement: acknowledgement(for: snapshot),
            supportedActions: PlaybackControl.supportedActions
        )
        // Capabilities are static for the life of a player. Repeating them on
        // every exchange is bytes the server already has; the first request of
        // a generation must carry them, and a change must resend them.
        if request.sequence != 1 && snapshot.capabilities == acceptedCapabilities {
            request.capabilities = nil
        }
        return PendingRequest(request: request, capture: pending)
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
            reconcilePreparation(
                request: request,
                response: response,
                requestSelection: pendingRequest.capture.snapshot.selection
            )
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
                capture: pendingRequest.capture
            ))
            // A terminal verdict ends reporting. It does not tear the player
            // down: this reporter still owns no recovery, and buffer already
            // fetched is still worth playing. The milestone that moves that
            // authority is the one that acts on this.
            if request.demand == .end { stop() }
            else if response.action.type == "terminal" { stopForTerminal(pendingRequest.capture) }
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
        case "prepare":
            guard let offer = response.action.preparedOffer, offer.isValid else {
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
                capture: pendingRequest.capture
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
            releasePreparation(.staleControl)
            nextAllowedAt = now() + retryDelay(transport, PlaybackControl.minimumExchangeMs)
            return
        }
        if status == 409, code == "stale_control" {
            if transportNamesDifferentGeneration(transport), adoptNewOwner(transport) {
                releasePreparation(.staleControl)
                nextAllowedAt = now() + PlaybackControl.minimumExchangeMs
                return
            }
            if request.acknowledgement != nil {
                ignoredPreparationActionId = request.acknowledgement?.actionId
                pendingAcknowledgement = nil
                releasePreparation(.staleControl)
                pending = newestCapture(pending)
                return
            }
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
        // Source may be intentionally empty between intent invalidation and
        // publication. Adopt the validated wire owner without inventing data;
        // fresh admission will wait for this local attachment's next capture.
        let newest = currentCapture().flatMap { $0.snapshot.isValid ? $0 : nil }
        bootstrap.generation = generation
        bootstrap.controlEpoch = epoch
        sequence = 0
        acceptedSequence = 0
        retryRequest = nil
        acceptedCapabilities = nil
        lastStartedAt = nil
        pending = newest
        pendingAcknowledgement = nil
        ignoredPreparationActionId = nil
        return true
    }

    private func transportNamesDifferentGeneration(_ error: ControlTransportError?) -> Bool {
        guard let generation = error?.generation else { return false }
        return generation != bootstrap.generation
    }

    private func acknowledgement(for snapshot: PlaybackControlSnapshot) -> ActionAcknowledgement? {
        guard let acknowledgement = pendingAcknowledgement else { return nil }
        // End owns teardown. A commit on the same request is both invalid and
        // ambiguous, so release the unpresented successor locally and let End
        // be the sole server-side path.
        if snapshot.demand == .end, acknowledgement.state == .committed {
            pendingAcknowledgement = nil
            releasePreparation(.withdrawn)
            return nil
        }
        if acknowledgement.state == .committed,
           snapshot.selection != activePreparation?.selection
        {
            let aborted = ActionAcknowledgement(
                actionId: acknowledgement.actionId, state: .aborted
            )
            pendingAcknowledgement = aborted
            return aborted
        }
        return acknowledgement
    }

    private func reconcilePreparation(
        request: ControlRequest,
        response: ControlResponse,
        requestSelection: ClientSelection
    ) {
        let acknowledged = request.acknowledgement
        if acknowledged == pendingAcknowledgement { pendingAcknowledgement = nil }
        let offered = response.action.preparedOffer

        if let acknowledged,
           acknowledged.actionId == activePreparation?.offer.actionId,
           [.committed, .failed, .aborted].contains(acknowledged.state)
        {
            if offered?.actionId == acknowledged.actionId {
                ignoredPreparationActionId = acknowledged.actionId
                releasePreparation(.acknowledgementDiscarded)
            } else {
                releasePreparation(acknowledged.state == .committed ? .committed : .withdrawn)
            }
        }

        guard let offered else {
            ignoredPreparationActionId = nil
            if activePreparation != nil { releasePreparation(.withdrawn) }
            return
        }
        if ignoredPreparationActionId == offered.actionId { return }
        if let current = activePreparation {
            if current.offer.actionId == offered.actionId {
                if let acknowledged,
                   acknowledged.actionId == offered.actionId,
                   acknowledged.state == .metadataReady || acknowledged.state == .bufferReady
                {
                    activePreparation?.acceptedState = acknowledged.state
                }
                return
            }
            releasePreparation(.replaced)
        }
        ignoredPreparationActionId = nil
        activePreparation = ActivePreparation(
            offer: offered, selection: requestSelection, acceptedState: nil
        )
        onPreparation(.offered(offered))
    }

    private func releasePreparation(_ reason: PreparedSwitchReleaseReason) {
        guard let actionId = activePreparation?.offer.actionId else {
            pendingAcknowledgement = nil
            return
        }
        activePreparation = nil
        pendingAcknowledgement = nil
        onPreparation(.released(actionId: actionId, reason: reason))
    }

    private func currentCapture() -> PlaybackControlCapture? {
        guard let current = capture(), current.owner == owner else { return nil }
        return current
    }

    /// Actor delivery order is not source observation order. Adopt the newer
    /// source envelope whole; never relabel an older queued payload.
    private func newestCapture(_ value: PlaybackControlCapture?) -> PlaybackControlCapture? {
        guard let current = currentCapture() else { return nil }
        if let value, value.owner != owner { return nil }
        guard let value, value.sourceRevision >= current.sourceRevision else { return current }
        return value
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
