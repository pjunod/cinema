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
    static let supportedActions = [
        "hold", "retry_resource", "terminal", prepareReplacementAction,
    ]
    /// The name a client **declares**, and the name the action **arrives**
    /// under, are not the same string, and this is the only action in the
    /// protocol where that is true.
    ///
    /// `accepts()` on the server is a literal comparison against the declared
    /// name (`playback_control.rs`, `PREPARE_REPLACEMENT_ACTION`), while the
    /// tag comes from `#[serde(tag = "type", rename_all = "snake_case")]` on
    /// the `ControlAction` enum, whose variant is `Prepare`. The transaction
    /// is named for the whole exchange; the tag is named for the moment.
    ///
    /// Both failures from getting this wrong are silent. Declaring `"prepare"`
    /// is never matched, so the server simply never offers a preparation.
    /// Switching on `"prepare_replacement"` never fires, so a staged successor
    /// is paid for and thrown away. These two constants exist so neither
    /// spelling is ever typed by hand again.
    static let prepareReplacementAction = "prepare_replacement"
    static let prepareActionType = "prepare"
    /// Mirrors of the server's own bounds, so a payload this client would have
    /// refused is refused before it reaches AVFoundation rather than after.
    /// `playback_control.rs`: `MAX_MEDIA_MILLIS`, `MAX_PLAYLIST_URL_LEN`,
    /// `MAX_ACTION_ID_LEN`, and `EffectiveSelection::is_valid`.
    static let maximumMediaMs = 366 * 24 * 60 * 60 * 1_000
    static let maximumPlaylistUrlBytes = 512
    static let maximumActionIdBytes = 64
    static let maximumAudioOffsetMs = 15_000
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

    /// The acknowledgement a request may actually carry.
    ///
    /// Two refusals, and both of them cost the acknowledgement rather than the
    /// exchange. An acknowledgement this client could not form is dropped
    /// because the server answers a malformed one with `400 invalid_control`,
    /// which fails the whole request and takes the position report with it.
    ///
    /// And `committed` may not share an exchange with `demand: "end"` — the
    /// server rejects that pairing outright. A viewer who switches players and
    /// then immediately closes owes two exchanges, in that order, and the
    /// caller is what sequences them; this is the guard that makes the
    /// forbidden body impossible to construct rather than merely unlikely.
    static func acknowledgement(
        _ value: ActionAcknowledgement?,
        demand: PlaybackDemand
    ) -> ActionAcknowledgement? {
        guard let value, value.isValid else { return nil }
        guard !(demand == .end && value.state == .committed) else { return nil }
        return value
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
    /// The settlement this player owes a staged successor, if it owes one.
    ///
    /// It rides the snapshot rather than a queue of its own because the
    /// reporter coalesces: the newest capture replaces any waiting one, so an
    /// acknowledgement parked anywhere else would be the one thing a position
    /// update could silently drop. The player keeps the same value on every
    /// snapshot until it sees the exchange that carried it come back, which
    /// makes a repeat — which the server tolerates on every state — the price
    /// of never losing a terminal settlement.
    ///
    /// Deliberately outside `isValid`: an acknowledgement this client could
    /// not form must cost the acknowledgement, never the whole exchange. The
    /// request builder drops it instead.
    var acknowledgement: ActionAcknowledgement? = nil

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
    /// Encoded only when present. The server has `deny_unknown_fields` and a
    /// `null` here is legal, but an omitted key is what every other optional
    /// on this request does, and Swift's synthesized encoding omits it for
    /// free.
    var acknowledgement: ActionAcknowledgement? = nil
    var supportedActions: [String] = PlaybackControl.supportedActions

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, clientInstanceId, sequence, demand
        case positionMs, bufferedFromMs, bufferedThroughMs, playbackRate
        case renderState, seekTargetMs, observedDownloadBps
        case selection, capabilities, observation, acknowledgement
        case supportedActions
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
    /// `prepare` only, all five. Optionals on a flat struct rather than an
    /// enum with associated values: web and Android both keep a flat shape,
    /// and the three implementations are meant to stay observably identical.
    /// `PreparedReplacementAction` is where the flat shape is turned into
    /// something that cannot be half-present.
    ///
    /// The UUID that names this staging, replayed byte-identically on every
    /// exchange until it settles. Echo it in `acknowledgement.action_id`.
    var actionId: String? = nil
    /// The successor's own session UUID.
    var sessionId: String? = nil
    /// Node-relative, and only ever this session's own index or master
    /// playlist. Never followed as an absolute URL.
    var playlistUrl: String? = nil
    /// The source position the successor's session-relative zero maps to.
    /// Without it the second timeline cannot be aligned with the first.
    var mediaOriginMs: Int? = nil
    /// What the successor will deliver — the server's answer, not the ask.
    var effectiveSelection: EffectiveSelection? = nil
}

/// What a session delivers, in the server's own vocabulary.
///
/// It appears twice on the wire: on the response, describing the session that
/// is playing now, and inside a `prepare`, describing the one that would
/// replace it. Comparing the two is how a client knows what is about to
/// change.
struct EffectiveSelection: Codable, Equatable {
    var qualityAuto: Bool
    var height: Int
    var audioTrack: Int?
    var subtitleBurn: Int?
    var audioOffsetMs: Int
    /// **Not a codec name.** Exactly `source` or `server_selected`, which is
    /// the direct-play/remux versus transcode distinction — the server's
    /// `PreparationAxis::DeliveryMethod`. Rendering this string to a viewer as
    /// a codec is wrong.
    var codec: String
    /// `dolby_vision`, `hdr10`, `hlg`, `sdr`, or absent.
    var dynamicRange: String?

    static let deliveryMethods = ["source", "server_selected"]
    static let dynamicRanges = ["dolby_vision", "hdr10", "hlg", "sdr"]

    /// The mirror of `EffectiveSelection::is_valid` in
    /// `crates/plurxd/src/playback_control.rs`. Height starts at zero because
    /// zero is what a source delivery reports.
    var isValid: Bool {
        let offset = PlaybackControl.maximumAudioOffsetMs
        return (0...PlaybackControl.maximumHeight).contains(height)
            && (audioTrack.map { (0...1_024).contains($0) } ?? true)
            && (subtitleBurn.map { (0...1_024).contains($0) } ?? true)
            && (-offset...offset).contains(audioOffsetMs)
            && Self.deliveryMethods.contains(codec)
            && (dynamicRange.map { Self.dynamicRanges.contains($0) } ?? true)
    }

    /// Whether the successor is a transcode rather than the source itself.
    var isServerSelected: Bool { codec == "server_selected" }
}

/// How far a prepared successor got, in the five words the server accepts.
///
/// There is no sixth. A value outside this set is a serde error on the server,
/// which answers `400 invalid_control` with no `invalid_field` at all — the
/// body never reaches the semantic validator, so nothing says which key was
/// wrong.
enum AcknowledgementState: String, Codable, Equatable {
    case metadataReady = "metadata_ready"
    case bufferReady = "buffer_ready"
    case committed
    case failed
    case aborted

    /// Progress states are optional and the server behaves correctly without
    /// them; a terminal one is owed. A preparation left to the 330-second
    /// deadline holds the session's only preparation slot for the rest of its
    /// life, so that session gets exactly one preparation, ever.
    var isTerminal: Bool {
        switch self {
        case .committed, .failed, .aborted: return true
        case .metadataReady, .bufferReady: return false
        }
    }
}

/// The client's half of the preparation transaction.
struct ActionAcknowledgement: Codable, Equatable {
    /// The `action_id` of the staging being settled, verbatim. An
    /// acknowledgement whose id does not match the currently bound `prepare`
    /// is silently ignored by the server — no error comes back — so a `200`
    /// is never proof a commit was accepted.
    var actionId: String
    var state: AcknowledgementState
    /// Required by `buffer_ready`, and meaningless elsewhere.
    var bufferedThroughMs: Int? = nil
    /// Required by `committed`: the source position the successor's timeline
    /// actually starts at, echoed from the offer being committed to.
    ///
    /// This is what a commit is proof *of*. `action_id` says which offer is
    /// being answered; this says the client built the thing that offer
    /// described. Between the `prepare` and the commit the server's own intent
    /// can move — a seek, a quality change, a new recipe — and an
    /// acknowledgement carrying only an id cannot distinguish a client that
    /// committed to the current offer from one that committed to a stale one
    /// and is about to present media nobody asked for. The server compares it
    /// against the staged successor's own `media_origin_ms` and refuses a
    /// mismatch, which is the only reason it is worth carrying.
    var committedMediaOriginMs: Int? = nil
    /// Required by `committed`: the wall clock at the successor's first
    /// qualifying frame, in milliseconds, and greater than zero.
    var firstFrameUnixMs: Int? = nil

    /// Every rule in the server's acknowledgement validator, applied before
    /// the request is built. Each of these is a `400 invalid_control` that
    /// would cost the whole exchange, not just the acknowledgement.
    var isValid: Bool {
        guard PlaybackControl.isUUID(actionId),
              actionId.utf8.count <= PlaybackControl.maximumActionIdBytes
        else { return false }
        if let bufferedThroughMs,
           !(0...PlaybackControl.maximumMediaMs).contains(bufferedThroughMs) {
            return false
        }
        if let firstFrameUnixMs, firstFrameUnixMs <= 0 { return false }
        if let committedMediaOriginMs,
           !(0...PlaybackControl.maximumMediaMs).contains(committedMediaOriginMs) {
            return false
        }
        if state == .bufferReady && bufferedThroughMs == nil { return false }
        if state == .committed && firstFrameUnixMs == nil { return false }
        if state == .committed && committedMediaOriginMs == nil { return false }
        return true
    }
}

/// A `prepare` action that has already been proven whole.
///
/// The reporter refuses a malformed one as a protocol violation, so nothing
/// downstream has to ask whether a field is present: reaching this type at all
/// means every rule below held.
struct PreparedReplacementAction: Equatable {
    let actionId: String
    let sessionId: String
    let playlistUrl: String
    let mediaOriginMs: Int
    let effectiveSelection: EffectiveSelection

    /// `/api/v1/hls/{sessionId}/index.m3u8` or `.../master.m3u8`, and nothing
    /// else — the exact rule `is_node_relative_playlist` applies, including
    /// that a query or fragment is allowed and ignored.
    ///
    /// Node-relative is the whole security property: resolved against this
    /// session's own origin, a prepared handoff cannot point a client
    /// anywhere. An absolute URL is refused here rather than followed, even
    /// if some later server sends one.
    static func isNodeRelativePlaylist(_ value: String, sessionId: String) -> Bool {
        guard value.utf8.count <= PlaybackControl.maximumPlaylistUrlBytes else { return false }
        let path = String(
            value.split(
                separator: "?", maxSplits: 1, omittingEmptySubsequences: false
            )[0].split(
                separator: "#", maxSplits: 1, omittingEmptySubsequences: false
            )[0]
        )
        return path == "/api/v1/hls/\(sessionId)/index.m3u8"
            || path == "/api/v1/hls/\(sessionId)/master.m3u8"
    }

    /// The mirror of `prepared_payload_is_valid`. Nil means the reporter must
    /// treat this action as a protocol violation.
    init?(_ action: ControlAction) {
        guard action.type == PlaybackControl.prepareActionType,
              let actionId = action.actionId,
              let sessionId = action.sessionId,
              let playlistUrl = action.playlistUrl,
              let mediaOriginMs = action.mediaOriginMs,
              let effectiveSelection = action.effectiveSelection,
              PlaybackControl.isUUID(actionId),
              actionId.utf8.count <= PlaybackControl.maximumActionIdBytes,
              PlaybackControl.isUUID(sessionId),
              Self.isNodeRelativePlaylist(playlistUrl, sessionId: sessionId),
              (0...PlaybackControl.maximumMediaMs).contains(mediaOriginMs),
              effectiveSelection.isValid
        else { return nil }
        self.actionId = actionId
        self.sessionId = sessionId
        self.playlistUrl = playlistUrl
        self.mediaOriginMs = mediaOriginMs
        self.effectiveSelection = effectiveSelection
    }
}

struct ControlDelivery: Codable, Equatable {
    /// Extensible relay value: only the exact value `ready` has client meaning.
    var subtitleReadiness: String?
    /// How far the server has got with a preparation for the newest ask:
    /// `staging`, `offered`, or `none`.
    ///
    /// Optional because it is optional on the wire. An older server, or a
    /// relay that predates the field, sends nothing — and **absence must never
    /// be read as a decline**. `PreparedOfferWait` is where that rule is
    /// written down and tested; this is only the field it reads.
    var preparation: String?
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
    /// What the session being reported on is actually delivering. The server
    /// has always sent this; until now Swift's decoder dropped it on the
    /// floor, so a client had no way to compare what it has against what a
    /// prepared successor would give it.
    var effectiveSelection: EffectiveSelection? = nil
    var action: ControlAction

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, acceptedSequence, delivery
        case effectiveSelection, action
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

    /// Teardown is best-effort after this bound; the server owns durable reap.
    static let finalizationDeadlineMs = 3_000
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

    private(set) var bootstrap: ControlBootstrap
    private let clientInstanceId: String
    private let owner: PlaybackControlCaptureOwner
    private let capture: @Sendable () -> PlaybackControlCapture?
    private let send: Send
    private let sleep: Sleep
    private let now: @Sendable () -> Int
    private let onExchange: @Sendable (Exchange) -> Void

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
    /// A source-independent final capture owned by teardown. When it contains
    /// committed+end, the reporter must finish the commit (including retries)
    /// and only then send the end exchange.
    private var finishing = false
    private var finishingDeadlineAt: Int?

    init?(
        bootstrap: ControlBootstrap,
        clientInstanceId: String,
        owner: PlaybackControlCaptureOwner,
        capture: @escaping @Sendable () -> PlaybackControlCapture?,
        send: @escaping Send,
        sleep: @escaping Sleep,
        now: @escaping @Sendable () -> Int,
        onExchange: @escaping @Sendable (Exchange) -> Void = { _ in }
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
        finishing = false
        finishingDeadlineAt = nil
        guard !stopped else { return }
        stopped = true
        pending = nil
        retryRequest = nil
        pump?.cancel()
        pump = nil
    }

    /// Finish reporting from an immutable teardown capture.
    ///
    /// The source slot is revoked immediately after this call is scheduled, so
    /// this path deliberately does not reconcile through `capture`. A commit
    /// colliding with end is retried byte-for-byte until accepted, then the
    /// reporter derives the acknowledgement-free end from the same capture.
    func finish(_ final: PlaybackControlCapture?) {
        guard let final, final.owner == owner, final.snapshot.isValid else {
            stop()
            return
        }
        terminalStop = nil
        stopped = false
        finishing = true
        finishingDeadlineAt = now() + Self.finalizationDeadlineMs
        pending = final
        if !inFlight {
            pump?.cancel()
            pump = Task { [weak self] in await self?.run() }
        }
    }

    /// Finish reporting and do not return until the reporter can no longer
    /// start a transport request.
    ///
    /// Production teardown is intentionally fire-and-forget so closing the
    /// player never waits on the control plane. Tests and transport owners
    /// that are about to invalidate an injected URLSession need the stronger
    /// boundary: invalidating it while this actor can still begin the final
    /// exchange is an Objective-C exception, not a catchable transport error.
    func finishAndWait(_ final: PlaybackControlCapture?) async {
        finish(final)
        let activePump = pump
        await activePump?.value
    }

    /// A terminal stops only the captured intent. MainActor may publish B
    /// immediately after this check, so B's notification can resume this
    /// latch. Explicit End/stop and protocol failure remain permanent.
    private func stopForTerminal(_ captured: PlaybackControlCapture) {
        guard !stopped, captured.hasSameIntent(as: newestCapture(pending)) else { return }
        finishing = false
        finishingDeadlineAt = nil
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
            "stopped": stopped ? 1 : 0,
        ]
    }

    // MARK: exchange loop

    private func run() async {
        while !stopped && !Task.isCancelled {
            if finishingExpired() {
                stop()
                return
            }
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
        let ordinary = max(0, max(rateAllowedAt, nextAllowedAt) - now())
        guard finishing, let finishingDeadlineAt else { return ordinary }
        return min(ordinary, max(0, finishingDeadlineAt - now()))
    }

    private func finishingExpired() -> Bool {
        finishing && finishingDeadlineAt.map { now() >= $0 } == true
    }

    /// A retry replays the exact request that failed — the same sequence, the
    /// same body — because a control exchange the server never accepted must
    /// not consume a sequence number, and the server dedupes on it.
    private func nextRequest() -> PendingRequest? {
        if let retryRequest { return retryRequest }
        let selected = finishing ? pending : newestCapture(pending)
        guard let pending = selected, pending.snapshot.isValid else {
            pending = nil
            return nil
        }
        self.pending = nil
        sequence += 1
        var snapshot = pending.snapshot
        // A first-frame commit and a terminal player event can land in the
        // same capture. The server deliberately rejects committed+end, but
        // dropping the acknowledgement loses the durable pointer move. Send
        // the commit as an active settlement first; once that accepted
        // exchange clears the acknowledgement, the next capture carries end.
        if snapshot.demand == .end, snapshot.acknowledgement?.state == .committed {
            snapshot.demand = .active
            snapshot.playbackRate = max(
                PlaybackControlMapping.minimumActiveRate,
                snapshot.playbackRate
            )
        }
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
            acknowledgement: PlaybackControl.acknowledgement(
                snapshot.acknowledgement, demand: snapshot.demand
            ),
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
            let exchangeDeadline = finishingDeadlineAt.map {
                max(1, min(PlaybackControl.exchangeDeadlineMs, $0 - now()))
            } ?? PlaybackControl.exchangeDeadlineMs
            let response = try await withDeadline(exchangeDeadline) {
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
                capture: pendingRequest.capture
            ))
            if finishing,
               pendingRequest.capture.snapshot.demand == .end,
               pendingRequest.capture.snapshot.acknowledgement?.state == .committed,
               request.demand == .active {
                var ending = pendingRequest.capture.snapshot
                ending.acknowledgement = nil
                pending = PlaybackControlCapture(
                    snapshot: ending,
                    intentGeneration: pendingRequest.capture.intentGeneration,
                    owner: pendingRequest.capture.owner,
                    sourceRevision: pendingRequest.capture.sourceRevision + 1
                )
            }
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
        case PlaybackControl.prepareActionType:
            // Strict, and fatal when it fails, for the same reason the
            // `default:` arm is fatal: an action inside the declared
            // vocabulary whose payload this client cannot trust is worse than
            // one it has never heard of, because it would be acted on. A
            // prepared handoff that half-parses would build a second decode
            // pipeline against an address nobody validated.
            guard PreparedReplacementAction(response.action) != nil else {
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
                capture: pendingRequest.capture
            )
        )
        if failure is ControlProtocolError {
            stop()
            return
        }
        if finishingExpired() {
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
        // Ordinary transport loss follows the advertised cadence. Teardown is
        // different: its immutable commit must retry before the bounded
        // finalization deadline, which may be shorter than that cadence.
        let fallback = retryableControl
            ? 500
            : (finishing ? PlaybackControl.minimumExchangeMs : bootstrap.nextExchangeMs)
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
        return true
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
