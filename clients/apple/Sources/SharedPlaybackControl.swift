import Foundation

/// A viewer action on a shared HLS session's current rendition. B accepts it
/// first; the renderer changes only after that acceptance.
enum SharedControlIntent: Equatable {
    case play
    case pause
    case seek(targetMs: Int)
}

/// The renderer facts one exchange reports, read from AVPlayer at send time.
struct SharedRendererSample: Equatable {
    var positionMs: Int
    var bufferedFromMs: Int?
    var bufferedThroughMs: Int
    var playing: Bool
}

/// The exact control body. `selection` is the frozen raw ask the session was
/// started with (or, for a directed change, the new raw ask) — never the
/// delivered encoder dimensions.
struct SharedControlRequest: Encodable, Equatable {
    let proto: String
    let generation: String
    let controlEpoch: Int
    let clientInstanceId: String
    let sequence: Int
    let demand: PlaybackDemand
    let positionMs: Int
    let bufferedFromMs: Int?
    let bufferedThroughMs: Int
    let playbackRate: Double
    let renderState: RenderState
    let seekTargetMs: Int?
    let selection: SharedPlaybackJSON
    let capabilities: DynamicCapabilities?
    let supportedActions: [String]

    enum CodingKeys: String, CodingKey {
        case proto = "protocol"
        case generation, controlEpoch, clientInstanceId, sequence, demand, positionMs
        case bufferedFromMs, bufferedThroughMs, playbackRate, renderState, seekTargetMs
        case selection, capabilities, supportedActions
    }

    /// Sorted keys keep a replay of the same sequence byte-identical.
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.keyEncodingStrategy = .convertToSnakeCase
        encoder.outputFormatting = [.sortedKeys]
        let data = try encoder.encode(self)
        guard data.count <= 16_384 else { throw APIError.badURL }
        return data
    }
}

/// What one exchange means for the player.
enum SharedControlOutcome: Equatable {
    /// B accepted exactly this sequence. `preparation` is relayed verbatim;
    /// only a directed change reads it, and absence is never a decline.
    case accepted(preparation: String?)
    /// The B session is gone (410, or a terminal answer).
    case ended
    /// A transition or rate refusal the server asked to be retried.
    case retry(afterMs: Int)
    /// Anything else: the change does not happen.
    case refused
}

/// One started shared HLS playback's control lane: B's exact tuple, one stable
/// client identity and one strictly ordered sequence. B's tuple never moves
/// for a shared session, so a refusal naming another owner is not adopted.
struct SharedControlChannel {
    static let supportedActions = ["terminal"]
    let sessionId: String
    let path: String
    let generation: String
    let controlEpoch: Int
    let clientInstanceId: String
    let capabilities: DynamicCapabilities
    let durationMs: Int?
    private(set) var sequence = 0

    init(playback: SharedStartedPlayback, clientInstanceId: String, capabilities: DynamicCapabilities) throws {
        let start = playback.start.response
        _ = try playback.start.validated(playback.context)
        guard let control = start.control, control.isValid,
              control.url == "/api/v1/hls/\(start.sessionId)/control",
              playback.context.sessionId == start.sessionId,
              PlaybackFileContext.canonicalV4(clientInstanceId),
              capabilities.isValid, !capabilities.dualPlayerPreparation
        else { throw APIError.badURL }
        sessionId = start.sessionId; path = control.url
        generation = control.generation; controlEpoch = control.controlEpoch
        self.clientInstanceId = clientInstanceId; self.capabilities = capabilities
        durationMs = start.durationMs
    }

    /// The next ordered request. `intent` nil is a directed change that keeps
    /// the current demand. Capabilities ride the first sequence only.
    mutating func request(_ intent: SharedControlIntent?, sample: SharedRendererSample,
                          selection: SharedPlaybackJSON) throws -> SharedControlRequest {
        try SharedPlaybackPlan.validateControlSelection(selection)
        guard sequence < 9_007_199_254_740_991 else { throw APIError.badURL }
        let ceiling = durationMs.map { max(0, min($0, PlaybackControl.maximumMediaMs)) } ?? PlaybackControl.maximumMediaMs
        let position = max(0, min(sample.positionMs, ceiling))
        let through = max(position, min(sample.bufferedThroughMs, ceiling))
        let from = sample.bufferedFromMs.flatMap { value in value >= 0 && value <= position ? value : nil }
        var demand: PlaybackDemand = sample.playing ? .active : .hold
        var render: RenderState = .rendering
        var target: Int?
        switch intent {
        case .play?: demand = .active
        case .pause?: demand = .hold
        case .seek(let ms)?: render = .seeking; target = max(0, min(ms, ceiling))
        case nil: break
        }
        sequence += 1
        return SharedControlRequest(
            proto: PlaybackControl.protocolName, generation: generation, controlEpoch: controlEpoch,
            clientInstanceId: clientInstanceId, sequence: sequence, demand: demand,
            positionMs: position, bufferedFromMs: from, bufferedThroughMs: through,
            playbackRate: demand == .active ? 1 : 0, renderState: render, seekTargetMs: target,
            selection: selection, capabilities: sequence == 1 ? capabilities : nil,
            supportedActions: Self.supportedActions)
    }

    /// An answer binds only to the request that was sent on this tuple.
    func accept(_ response: ControlResponse, for request: SharedControlRequest) throws -> SharedControlOutcome {
        guard request.generation == generation, request.controlEpoch == controlEpoch,
              response.proto == PlaybackControl.protocolName,
              response.generation == generation,
              response.controlEpoch == controlEpoch,
              response.acceptedSequence == request.sequence
        else { throw ControlProtocolError(reason: "tuple") }
        switch response.action.type {
        case "none", "hold", "retry_resource": return .accepted(preparation: response.delivery?.preparation)
        case "terminal": return .ended
        default: throw ControlProtocolError(reason: "action")
        }
    }

    /// B's closed refusal vocabulary. A transport loss replays the same bytes,
    /// which B answers exactly once accepted.
    static func classify(_ failure: ControlTransportError) -> SharedControlOutcome {
        let delay = max(250, min(failure.retryAfterMs ?? 500, 5_000))
        switch (failure.status, failure.code) {
        case (410?, _): return .ended
        case (425?, "owner_transition"?), (429?, "control_rate_limited"?), (503?, "control_unavailable"?): return .retry(afterMs: delay)
        case (nil, _): return failure.canceled ? .refused : .retry(afterMs: 500)
        default: return .refused
        }
    }
}

/// B's complete direct Start reply: exactly five fields, no control route, no
/// playlist, and a byte URL that is this context's own file alias bound to the
/// returned B session.
struct SharedDirectStart: Equatable {
    static let mimes: Set<String> = ["video/mp4", "video/webm", "video/x-matroska", "video/mp2t",
        "video/x-msvideo", "audio/mp4", "audio/aac", "audio/mpeg", "audio/flac", "audio/ogg", "audio/wav",
        "audio/x-ms-wma", "application/octet-stream"]
    let sessionId: String
    let url: String
    let length: Int64
    let mime: String

    static func decode(_ data: Data, context: PlaybackFileContext) throws -> (start: Self, context: PlaybackFileContext) {
        guard data.count <= 16_384, context.reference != nil, context.sessionId == nil,
              let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == ["presentation", "session_id", "url", "length", "mime"],
              object["presentation"] as? String == "direct",
              let session = object["session_id"] as? String, PlaybackFileContext.canonicalV4(session),
              let length = object["length"] as? NSNumber,
              ["s", "i", "l", "q", "Q"].contains(String(cString: length.objCType)),
              (0...9_007_199_254_740_991).contains(length.int64Value),
              let mime = object["mime"] as? String, mimes.contains(mime),
              let url = object["url"] as? String
        else { throw APIError.badURL }
        let bound = try context.withSession(session)
        guard url == (try bound.path("direct")) else { throw APIError.badURL }
        return (Self(sessionId: session, url: url, length: length.int64Value, mime: mime), bound)
    }
}

/// A started shared direct play: the B session owns the byte route; there is
/// no status or control for it, Local or shared.
struct SharedStartedDirect {
    let start: SharedDirectStart
    let context: PlaybackFileContext
    let request: CreateSessionRequest
    func validated() throws -> Self {
        guard context.reference != nil, context.sessionId == start.sessionId,
              start.url == (try context.path("direct")) else { throw APIError.badURL }
        return self
    }
}

/// Either started shared media. Both are B sessions that DELETE ends; only HLS
/// has a playlist, status and control.
enum SharedStartedMedia {
    case hls(SharedStartedPlayback)
    case direct(SharedStartedDirect)
    var sessionId: String {
        switch self { case .hls(let p): return p.start.response.sessionId; case .direct(let d): return d.start.sessionId }
    }
    var context: PlaybackFileContext {
        switch self { case .hls(let p): return p.context; case .direct(let d): return d.context }
    }
    var request: CreateSessionRequest {
        switch self { case .hls(let p): return p.request; case .direct(let d): return d.request }
    }
    var durationMs: Int64? {
        if case .hls(let p) = self { return p.start.response.durationMs.map(Int64.init) }
        return nil
    }
    var isDirect: Bool { if case .direct = self { return true }; return false }
    func validated() throws {
        switch self {
        case .hls(let p):
            _ = try p.start.validated(p.context)
            guard p.context.sessionId == p.start.response.sessionId else { throw APIError.badURL }
        case .direct(let d): _ = try d.validated()
        }
    }
}

/// The viewer's raw ask for a directed change. Nil fields keep the current ask.
struct SharedDirectedChange: Equatable {
    var quality: PlaybackQuality?
    var audioIndex: Int??
    var subtitleIndex: Int??
}

extension SharedPlaybackPlan {
    /// The raw ask this plan's Start carried, in the vocabulary a reopen uses.
    var rawQuality: PlaybackQuality {
        // The same reading `frozenControlSelection` gives the Start's raw ask.
        if request.presentation == "direct" { return .original }
        if request.qualityAuto ?? (request.height == nil) { return .auto }
        if request.copy == true { return .original }
        if let height = request.height, let rung = PlaybackQuality(rawValue: String(height)) { return rung }
        return .auto
    }
    var rawAudioIndex: Int? { request.audio }
    var rawSubtitleIndex: Int? { request.nativeSubtitles == true ? request.subtitle : nil }

    /// The selection the server is asked for on a directed change: the frozen
    /// raw selection with only the changed axes replaced, so an unchanged axis
    /// (an Auto raw height, say) keeps exactly the ask the Start carried.
    func directedSelection(_ change: SharedDirectedChange) throws -> SharedPlaybackJSON {
        guard var object = try frozenControlSelection().object else { throw APIError.badURL }
        if let quality = change.quality, quality != rawQuality {
            switch quality {
            case .auto: object["quality"] = .object(["mode": .string("auto")])
            case .original: object["quality"] = .object(["mode": .string("original")])
            case let rung:
                guard let height = rung.rungHeight, (PlaybackControl.minimumHeight...PlaybackControl.maximumHeight).contains(height)
                else { throw APIError.transport("This Shared quality is outside the control grammar.") }
                object["quality"] = .object(["mode": .string("manual"), "height": .integer(Int64(height))])
            }
        }
        if let audio = change.audioIndex {
            if let audio { guard (0...1024).contains(audio) else { throw APIError.badURL } }
            object["audio_track"] = audio.map { .integer(Int64($0)) } ?? .null
        }
        if let subtitle = change.subtitleIndex {
            if let subtitle, subtitle >= 0 {
                guard (0...1024).contains(subtitle) else { throw APIError.badURL }
                object["subtitle"] = .object(["mode": .string("native"), "track": .integer(Int64(subtitle))])
            } else {
                object["subtitle"] = .object(["mode": .string("off")])
            }
        }
        let selection = SharedPlaybackJSON.object(object)
        try Self.validateControlSelection(selection)
        return selection
    }

    /// The control grammar's selection: closed keys, bounded values.
    static func validateControlSelection(_ selection: SharedPlaybackJSON) throws {
        guard let object = selection.object,
              Set(object.keys) == ["quality", "audio_track", "audio_offset_ms", "subtitle", "codec", "dynamic_range"],
              let quality = object["quality"]?.object, let mode = quality["mode"]?.string,
              object["audio_offset_ms"] == .integer(0),
              object["codec"] == .string("auto"), object["dynamic_range"] == .string("auto"),
              let subtitle = object["subtitle"]?.object, let subtitleMode = subtitle["mode"]?.string
        else { throw APIError.badURL }
        func bounded(_ value: SharedPlaybackJSON?, _ range: ClosedRange<Int64>) -> Bool {
            if case .integer(let n)? = value { return range.contains(n) }; return false
        }
        let height = Int64(PlaybackControl.minimumHeight)...Int64(PlaybackControl.maximumHeight)
        switch mode {
        case "auto": guard Set(quality.keys).isSubset(of: ["mode", "height"]), quality["height"].map({ bounded($0, height) }) ?? true else { throw APIError.badURL }
        case "original": guard quality.keys.count == 1 else { throw APIError.badURL }
        case "manual": guard quality.keys.count == 2, bounded(quality["height"], height) else { throw APIError.badURL }
        default: throw APIError.badURL
        }
        switch object["audio_track"] { case .null?: break; case let value? where bounded(value, 0...1024): break; default: throw APIError.badURL }
        switch subtitleMode {
        case "off": guard subtitle.keys.count == 1 else { throw APIError.badURL }
        case "native": guard subtitle.keys.count == 2, bounded(subtitle["track"], 0...1024) else { throw APIError.badURL }
        default: throw APIError.badURL
        }
    }
}

/// What the player does after one exchange. Pure, so the ordering rule —
/// the renderer moves only after B accepted — is testable without AVPlayer.
enum SharedControlStep: Equatable {
    /// Apply the viewer's action to the renderer now.
    case apply
    /// B lost the session during a pause: pause locally; the next play or
    /// seek starts fresh at its position.
    case pauseEnded
    /// Start a fresh shared session at the action's position.
    case reopen(play: Bool)
    /// B lost the session and this attachment already used its fresh Start.
    case ended
    /// Nothing changes; tell the viewer.
    case refused

    static func after(_ intent: SharedControlIntent, outcome: SharedControlOutcome,
                      playing: Bool, restartAllowed: Bool) -> Self {
        switch outcome {
        case .accepted: return .apply
        case .ended:
            if intent == .pause { return .pauseEnded }
            return restartAllowed ? .reopen(play: intent == .play || playing) : .ended
        case .retry, .refused: return .refused
        }
    }

    /// A directed change never moves the renderer in place: an evaluated
    /// `none` (or a lost session) reopens; absence or a refusal changes nothing.
    static func afterChange(_ outcome: SharedControlOutcome, playing: Bool) -> Self {
        switch outcome {
        case .accepted(let preparation): return preparation == "none" ? .reopen(play: playing) : .refused
        case .ended: return .reopen(play: playing)
        case .retry, .refused: return .refused
        }
    }
}
