import Foundation

/// Lossless wire values retain fields the Local presentation DTO does not model.
indirect enum SharedPlaybackJSON: Codable, Equatable {
    case object([String: Self]), array([Self]), string(String), integer(Int64), unsigned(UInt64), number(Double), bool(Bool), null
    init(from decoder: Decoder) throws {
        let c = try decoder.singleValueContainer()
        if c.decodeNil() { self = .null }
        else if let value = try? c.decode(Bool.self) { self = .bool(value) }
        else if let value = try? c.decode(Int64.self) { self = .integer(value) }
        else if let value = try? c.decode(UInt64.self) { self = .unsigned(value) }
        else if let value = try? c.decode(Double.self) { self = .number(value) }
        else if let value = try? c.decode(String.self) { self = .string(value) }
        else if let value = try? c.decode([String: Self].self) { self = .object(value) }
        else { self = .array(try c.decode([Self].self)) }
    }
    func encode(to encoder: Encoder) throws {
        var c = encoder.singleValueContainer()
        switch self {
        case .object(let v): try c.encode(v)
        case .array(let v): try c.encode(v)
        case .string(let v): try c.encode(v)
        case .integer(let v): try c.encode(v)
        case .unsigned(let v): try c.encode(v)
        case .number(let v): try c.encode(v)
        case .bool(let v): try c.encode(v)
        case .null: try c.encodeNil()
        }
    }
    var string: String? { if case .string(let v) = self { return v }; return nil }
    var object: [String: Self]? { if case .object(let v) = self { return v }; return nil }
}

struct SharedPlaybackFileReference: Codable, Hashable {
    let item: SharedPlaybackReference
    let fileId: String
    let revision: String
    let lifecycleGeneration: Int64?
    func validate(_ context: PlaybackFileContext) throws {
        try item.validate()
        guard let lifecycleGeneration, lifecycleGeneration > 0, lifecycleGeneration == context.lifecycleGeneration else { throw APIError.badURL }
        try context.validateSharedReference(item, file: fileId, revision: revision)
    }
}

struct SharedDecisionPresentation: Decodable {
    var delivery: Delivery?
    var reasons: [String]?
    var transcodeAudio: Bool?
    var preserveDolbyVision: Bool?
    var source: SourceSummary?
    var audio: [AudioTrack]?
    var subtitles: [SubtitleTrack]?
    var selection: DecisionSelection?
    var markers: [Marker]?
    var audioOffsetMs: Int?
    var declaredOffsetMs: Int?
    var ladder: [QualityRung]?
    var qualityCandidates: [QualityCandidate]?
    var qualityCandidateId: String?
    var displayAwareAutoProtocol: String?
    var deliveredDynamicRange: String?
    var deliveredDolbyVisionProfile: Int?
}

/// No conversion to Decision: Source file IDs remain strings, including IDs above 2^53.
struct SharedDecision {
    let fileId: String
    let reference: SharedPlaybackFileReference
    let method: String
    let playUrl: String
    let wire: [String: SharedPlaybackJSON]
    let presentation: SharedDecisionPresentation
    static func decode(_ data: Data) throws -> Self {
        guard data.count <= 4_194_304,
              let object = try JSONDecoder().decode(SharedPlaybackJSON.self, from: data).object,
              let file = object["file_id"]?.string, PlaybackFileContext.canonicalID(file),
              let method = object["method"]?.string, ["direct_play", "remux", "transcode"].contains(method),
              let play = object["play_url"]?.string,
              let binding = object["reference"], object["delivery"]?.object != nil
        else { throw APIError.badURL }
        let raw = try JSONSerialization.jsonObject(with: data) as? [String: Any]
        guard let rawReference = raw?["reference"] as? [String: Any], PlaybackFileContext.canonicalLifecycle(rawReference["lifecycle_generation"]) != nil else { throw APIError.badURL }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let reference = try decoder.decode(SharedPlaybackFileReference.self, from: JSONEncoder().encode(binding))
        guard file == reference.fileId else { throw APIError.badURL }
        return Self(fileId: file, reference: reference, method: method, playUrl: play, wire: object, presentation: try decoder.decode(SharedDecisionPresentation.self, from: data))
    }
    func validated(_ context: PlaybackFileContext) throws -> Self {
        try reference.validate(context)
        try context.validateDescriptiveURL(playUrl)
        guard let delivery = wire["delivery"]?.object,
              let mode = delivery["mode"]?.string, mode == (method == "direct_play" ? "direct" : method)
        else { throw APIError.badURL }
        for key in ["url", "sessions_url"] {
            if let value = delivery[key], value != .null {
                guard let url = value.string else { throw APIError.badURL }
                try context.validateDescriptiveURL(url)
                if key == "sessions_url", url != context.fileBase + "/hls/sessions" { throw APIError.badURL }
            }
        }
        return self
    }
}

enum PlaybackSubject {
    case local(itemId: String, context: PlaybackFileContext)
    case shared(context: PlaybackFileContext)
    func validated() throws -> Self {
        switch self {
        case .local(let item, let context):
            guard PlaybackFileContext.canonicalID(item) else { throw APIError.badURL }
            _ = try context.localID()
        case .shared(let context):
            guard let item = context.reference, let revision = context.revision else { throw APIError.badURL }
            try context.validateSharedReference(item, file: context.sourceFileId, revision: revision)
        }
        return self
    }
}

/// Retains additive server start fields alongside the existing session projection.
struct SharedStart {
    let response: HlsStart
    let wire: [String: SharedPlaybackJSON]
    static func decode(_ data: Data) throws -> Self {
        guard data.count <= 1_048_576,
              let wire = try JSONDecoder().decode(SharedPlaybackJSON.self, from: data).object
        else { throw APIError.badURL }
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return Self(response: try decoder.decode(HlsStart.self, from: data), wire: wire)
    }
    func validated(_ context: PlaybackFileContext) throws -> Self {
        _ = try SharedStartValidation.validated(response, context: context)
        return self
    }
}

/// Ordinary complete B Start retained with its authenticated signed-file context.
/// This is routing metadata; it never asserts Source physical production.
struct SharedStartedPlayback {
    let start: SharedStart
    let context: PlaybackFileContext
    let request: CreateSessionRequest
}

extension SharedStart {
    func bindInitial(_ context: PlaybackFileContext, request: CreateSessionRequest) throws -> SharedStartedPlayback {
        guard context.reference != nil, context.sessionId == nil,
              response.vod == true, let position = response.startSeconds,
              position.isFinite, position >= 0, position <= 9_007_199_254_740,
              response.durationMs.map({ (0...9_007_199_254_740_991).contains($0) }) ?? true,
              let control = response.control,
              PlaybackFileContext.matches(control.generation, "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"),
              control.nextExchangeMs == 5_000, control.leaseTimeoutMs == 300_000
        else { throw APIError.badURL }
        let bound = try context.withSession(response.sessionId)
        _ = try validated(bound)
        return SharedStartedPlayback(start: self, context: bound, request: request)
    }
}

/// B session authority must already be admitted; a metadata response cannot bind it.
enum SharedStartValidation {
    static func validated(_ start: HlsStart, context: PlaybackFileContext) throws -> HlsStart {
        try context.validateSessionPlaylist(start.playlistUrl, session: start.sessionId)
        guard let control = start.control, control.isValid,
              control.url == "/api/v1/hls/\(start.sessionId)/control" else { throw APIError.badURL }
        return start
    }
}

struct SharedPGSManifest: Codable {
    let schema: Int
    let generation: String
    let fileId: String
    let reference: SharedPlaybackFileReference
    let trackIndex: Int
    let kind: String
    let timebase: String
    let durationMs: Int
    let cues: [PGSOverlayCue]

    func validated(context: PlaybackFileContext, trackIndex expectedTrackIndex: Int) throws -> Self {
        try reference.validate(context)
        guard schema == 1,
              kind == "pgs",
              timebase == "source_ms",
              fileId == reference.fileId,
              trackIndex == expectedTrackIndex,
              durationMs > 0,
              Self.isSHA256(generation),
              cues.count <= PGSOverlayPolicy.maximumManifestCues
        else { throw PGSOverlayError.invalidManifest }

        var previousEnd = 0
        var imageDimensions: [String: (width: Int, height: Int)] = [:]
        for cue in cues {
            guard !cue.id.isEmpty,
                  cue.startMs >= previousEnd,
                  cue.endMs > cue.startMs,
                  cue.endMs <= durationMs,
                  (1...PGSOverlayPolicy.maximumCanvasWidth).contains(cue.canvasWidth),
                  (1...PGSOverlayPolicy.maximumCanvasHeight).contains(cue.canvasHeight),
                  cue.objects.count <= PGSOverlayPolicy.maximumObjectsPerCue
            else { throw PGSOverlayError.invalidManifest }
            previousEnd = cue.endMs

            for object in cue.objects {
                guard object.x >= 0,
                      object.y >= 0,
                      object.width > 0,
                      object.height > 0,
                      object.x <= cue.canvasWidth,
                      object.y <= cue.canvasHeight,
                      object.width <= cue.canvasWidth - object.x,
                      object.height <= cue.canvasHeight - object.y,
                      Self.objectHash(
                        from: object.image,
                        generation: generation
                      ) != nil
                else { throw PGSOverlayError.invalidManifest }
                if let existing = imageDimensions[object.image] {
                    guard existing.width == object.width,
                          existing.height == object.height
                    else { throw PGSOverlayError.invalidManifest }
                } else {
                    imageDimensions[object.image] = (object.width, object.height)
                }
            }
        }
        return self
    }

    static func objectHash(from path: String, generation: String) -> String? {
        let prefix = "overlay/\(generation)/objects/"
        guard path.hasPrefix(prefix), path.hasSuffix(".png") else { return nil }
        let hash = String(path.dropFirst(prefix.count).dropLast(4))
        return isSHA256(hash) ? hash : nil
    }

    static func isSHA256(_ value: String) -> Bool {
        value.count == 64 && value.utf8.allSatisfy {
            (48...57).contains($0) || (97...102).contains($0)
        }
    }
}


/// A beat is retained verbatim across uncertain sends. A conflict discards it;
/// only a fresh authorized watch read can permit the next new beat.
struct SharedProgressBeat: Encodable, Equatable {
    let sessionId: String
    let sequence: Int64
    let positionMs: Int64
    let durationMs: Int64?
    let watched: Bool
    func validate() throws {
        guard sessionId.count == 36,
              PlaybackFileContext.matches(sessionId, "^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$"),
              (0...9_007_199_254_740_991).contains(sequence),
              (0...9_007_199_254_740_991).contains(positionMs),
              durationMs.map({ (0...9_007_199_254_740_991).contains($0) }) ?? true
        else { throw APIError.badURL }
    }
}
enum SharedProgressResult: Equatable {
    case acknowledged
    case previousBeatAcknowledged
    case resyncRequired(currentSequence: Int64?)
}
struct SharedProgressOrder {
    private(set) var sequence: Int64
    private(set) var pending: SharedProgressBeat?
    private(set) var needsResync = false
    init(sequence: Int64) throws {
        guard (0...9_007_199_254_740_991).contains(sequence) else { throw APIError.badURL }
        self.sequence = sequence
    }
    mutating func beat(sessionId: String, positionMs: Int64, durationMs: Int64?, watched: Bool) throws -> SharedProgressBeat {
        guard !needsResync else { throw APIError.badURL }
        if let pending { guard pending.sessionId == sessionId else { throw APIError.badURL }; return pending }
        guard sequence < 9_007_199_254_740_991 else { throw APIError.badURL }
        let beat = SharedProgressBeat(sessionId: sessionId, sequence: sequence + 1, positionMs: positionMs, durationMs: durationMs, watched: watched)
        try beat.validate(); sequence = beat.sequence; pending = beat; return beat
    }
    mutating func complete(_ beat: SharedProgressBeat, result: SharedProgressResult) throws {
        guard pending == beat else { throw APIError.badURL }
        if case let .resyncRequired(current) = result {
            if let current { guard (0...9_007_199_254_740_991).contains(current) else { throw APIError.badURL }; sequence = max(sequence, current) }
            needsResync = true
        }
        pending = nil
    }
    mutating func resync(freshAuthorizedSequence: Int64) throws {
        guard needsResync, (0...9_007_199_254_740_991).contains(freshAuthorizedSequence) else { throw APIError.badURL }
        sequence = max(sequence, freshAuthorizedSequence); needsResync = false
    }
}

/// An initial Shared subject has no numeric Local item or file identity.
struct SharedPlaybackSubject {
    let context: PlaybackFileContext
    let title: String
    let resumeMs: Int64
    let watchSequence: Int64
    func validate() throws {
        guard context.sessionId == nil, let reference = context.reference,
              let revision = context.revision, title.utf8.count <= 4096,
              (0...9_007_199_254_740_991).contains(resumeMs),
              (0...9_007_199_254_740_991).contains(watchSequence) else { throw APIError.badURL }
        try context.validateSharedReference(reference, file: context.sourceFileId, revision: revision)
    }
}
/// The fixed initial HLS plan retains the raw original ask for later directed
/// control equality. Delivered encoder dimensions never replace that ask.
struct SharedPlaybackPlan {
    let subject: SharedPlaybackSubject
    let decision: SharedDecision
    let caps: DeviceCaps
    let request: CreateSessionRequest
    init(subject: SharedPlaybackSubject, decision: SharedDecision, caps: DeviceCaps, request: CreateSessionRequest) throws {
        try subject.validate(); _ = try decision.validated(subject.context)
        let direct = request.presentation == "direct"
        guard caps.v == 2, caps.transports.contains("hls"), request.caps == caps,
              request.presentation == "vod" || direct, request.intent == nil,
              request.previousSessionId == nil, request.controlSequence == nil, request.reopenReason == nil,
              request.preserveDolbyVision != true,
              (request.start ?? 0) == Double(subject.resumeMs) / 1000,
              request.height.map({ $0 > 0 && $0 <= 8192 }) ?? true,
              direct ? Self.directEligible(decision) && request.copy == nil && request.height == nil && request.aac == nil
                    && request.audio == nil && request.nativeSubtitles == nil && request.subtitle == nil
                : (decision.method == "transcode" || request.subtitleBurn != nil ? request.copy != true : request.copy == true)
        else { throw APIError.transport("This Shared HLS plan is not available yet.") }
        self.subject = subject; self.decision = decision; self.caps = caps; self.request = request
    }

    /// Shared direct play: the Source decided direct play for these caps and
    /// AVPlayer can take the bytes as they are. A Dolby Vision source stays on
    /// copy HLS for the reason Local gives (`PlayerController.playbackMode`:
    /// AVPlayer renders a black plane for progressive DV). A planned
    /// non-default audio track or an A/V offset needs a session, not raw bytes.
    static func directEligible(_ decision: SharedDecision) -> Bool {
        let presentation = decision.presentation
        let plannedAudio = presentation.delivery?.audio
        let defaultAudio = plannedAudio.map { index in presentation.audio?.first(where: { $0.index == index })?.default == true } ?? true
        return decision.method == "direct_play" && presentation.delivery?.mode == "direct"
            && presentation.source?.hdr?.lowercased() != "dolby_vision"
            && presentation.deliveredDynamicRange != "dolby_vision"
            && presentation.preserveDolbyVision != true && presentation.delivery?.preserveDolbyVision != true
            && (presentation.audioOffsetMs ?? 0) == 0 && defaultAudio
    }

    /// The one place a shared Start request is composed from a decision: the
    /// initial play and every reopen. A reopen keeps the player's playback id,
    /// so B supersedes the predecessor once the new session publishes.
    static func make(subject: SharedPlaybackSubject, decision: SharedDecision, caps: DeviceCaps,
                     quality: PlaybackQuality, audioIndex: Int? = nil, subtitleIndex: Int? = nil,
                     playbackId: String = UUID().uuidString.lowercased()) throws -> Self {
        let start = Double(subject.resumeMs) / 1000
        let requestId = UUID().uuidString.lowercased()
        let subtitle = subtitleIndex.flatMap { index in
            index >= 0 && decision.presentation.subtitles?.contains(where: { $0.index == index && $0.isNativeHLS }) == true ? index : nil
        }
        let burn = subtitleIndex.flatMap { index in
            index >= 0 && decision.presentation.subtitles?.contains(where: { $0.index == index && !$0.isNativeHLS }) == true ? index : nil
        }
        if let index = subtitleIndex, index >= 0, subtitle == nil, burn == nil {
            throw APIError.transport("This Shared subtitle is unavailable.")
        }
        var request: CreateSessionRequest
        if audioIndex == nil, subtitle == nil, burn == nil, quality == .auto || quality == .original, directEligible(decision) {
            request = CreateSessionRequest(playbackId: playbackId, requestId: requestId, start: start, caps: caps)
            request.presentation = "direct"
        } else {
            request = CreateSessionRequest(playbackId: playbackId, requestId: requestId,
                height: quality.rungHeight, qualityAuto: quality == .auto, start: start, audio: audioIndex,
                subtitleBurn: burn, nativeSubtitles: subtitle == nil ? nil : true, subtitle: subtitle,
                copy: burn == nil && decision.method != "transcode", aac: decision.presentation.transcodeAudio,
                hdr10: decision.method == "transcode" && decision.presentation.deliveredDynamicRange == "hdr10" ? true : nil, caps: caps)
        }
        return try Self(subject: subject, decision: decision, caps: caps, request: request)
    }
}

/// Current-rendition control selection comes from the original ask, never the
/// delivered encoder height. Kept separate from Local auto-candidate enums.
extension SharedPlaybackPlan {
    func frozenControlSelection() throws -> SharedPlaybackJSON {
        guard request.height.map({ (PlaybackControl.minimumHeight...PlaybackControl.maximumHeight).contains($0) }) ?? true,
              request.audio.map({ (0...1024).contains($0) }) ?? true
        else { throw APIError.transport("The original Shared selection is outside the control grammar.") }
        var quality: [String: SharedPlaybackJSON]
        if request.qualityAuto ?? (request.height == nil) {
            quality = ["mode": .string("auto")]
            if let height = request.height { quality["height"] = .integer(Int64(height)) }
        } else if request.copy == true || request.height == nil { quality = ["mode": .string("original")] }
        else if let height = request.height, height > 0 { quality = ["mode": .string("manual"), "height": .integer(Int64(height))] }
        else { throw APIError.transport("The original Shared selection is unavailable.") }
        var subtitle: [String: SharedPlaybackJSON] = ["mode": .string("off")]
        if let track = request.subtitleBurn {
            guard (0...1024).contains(track) else { throw APIError.badURL }
            subtitle = ["mode": .string("burn"), "track": .integer(Int64(track))]
        } else if request.nativeSubtitles == true, let track = request.subtitle {
            guard (0...1024).contains(track) else { throw APIError.transport("The original Shared subtitle selection is unavailable.") }
            subtitle = ["mode": .string("native"), "track": .integer(Int64(track))]
        }
        return .object(["quality": .object(quality), "audio_track": request.audio.map { .integer(Int64($0)) } ?? .null,
            "audio_offset_ms": .integer(0), "subtitle": .object(subtitle), "codec": .string("auto"), "dynamic_range": .string("auto")])
    }
}

/// Shared telemetry is presentation metadata, never a Local owner/status DTO.
struct SharedPlaybackStatus {
    let wire: [String: SharedPlaybackJSON]
    let targetHeight: Int64
    let encoder: String
    let producerState: String
    let serverReadyState: String
    let aheadSeconds: Int64?
    /// Rendered only from validated machine tokens and bounded integers; no
    /// Source prose, path or identity can reach this line.
    var summary: String {
        var parts = ["Shared HLS", "\(targetHeight)p", encoder, producerState]
        if let aheadSeconds { parts.append("\(aheadSeconds) s ahead") }
        return parts.joined(separator: " · ")
    }
    /// The server's `status_token` grammar: short ASCII machine vocabulary.
    static func isToken(_ text: String) -> Bool {
        !text.isEmpty && text.utf8.count <= 32 && text.utf8.allSatisfy {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || $0 == 95 || $0 == 45 || $0 == 46
        }
    }
    static let requiredCounters = Set("target_height fetched_end_ms materialized_segments planned_segments materialized_bytes planned_bytes working_set_bytes working_set_budget_bytes completed_cache_bytes delivered_bytes delivered_idle_ms http_wait_count status_generated_unix_ms".split(separator: " ").map(String.init))
    static let optionalCounters = Set("active_encode_milli_realtime active_encode_age_ms active_encode_active_ms active_encode_segments tone_map_peak_nits reported_position_ms client_runway_ms server_ready_anchor_ms server_ready_end_ms server_next_ready_start_ms server_next_ready_end_ms published_end_ms ready_ahead_end_ms fetched_segment ahead_seconds delivered_bps http_wait_oldest_ms http_wait_segment".split(separator: " ").map(String.init))
    static let requiredWords: Set<String> = ["encoder", "playlist_shape", "producer_state", "server_ready_state"]
    static let optionalWords: Set<String> = ["tone_map_peak_source", "producer_hold", "producer_decision", "control_demand", "render_state"]
    static func decode(_ data: Data, playback: SharedStartedPlayback) throws -> Self {
        guard data.count <= 65_536,
              let outer = try JSONDecoder().decode(SharedPlaybackJSON.self, from: data).object,
              Set(outer.keys) == Set(["subject", "reference", "session_id", "incarnation_id", "control_epoch", "status"]),
              outer["subject"] == .string("shared"),
              outer["session_id"] == .string(playback.start.response.sessionId),
              let control = playback.start.response.control,
              outer["incarnation_id"] == .string(control.generation),
              outer["control_epoch"] == .integer(Int64(control.controlEpoch)),
              let status = outer["status"]?.object,
              let original = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              let rawStatus = original["status"] as? [String: Any]
        else { throw APIError.badURL }
        _ = try playback.start.validated(playback.context)
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        guard let reference = outer["reference"] else { throw APIError.badURL }
        try decoder.decode(SharedPlaybackFileReference.self, from: JSONEncoder().encode(reference)).validate(playback.context)
        let booleans: Set<String> = ["admitted", "suspended", "final"]
        let allowed = requiredCounters.union(optionalCounters).union(requiredWords).union(optionalWords).union(booleans).union(["server_ready_seconds", "active_encode_candidate_id"])
        guard Set(status.keys).isSubset(of: allowed), requiredCounters.union(requiredWords).union(booleans).isSubset(of: Set(status.keys)) else { throw APIError.badURL }
        guard requiredCounters.union(requiredWords).allSatisfy({ status[$0] != .null }) else { throw APIError.badURL }
        for key in requiredCounters.union(optionalCounters) {
            guard let value = status[key], value != .null else { continue }
            guard let number = rawStatus[key] as? NSNumber, !["d", "f"].contains(String(cString: number.objCType)) else { throw APIError.badURL }
            switch value {
            case .integer(let n): guard n >= 0 else { throw APIError.badURL }
            case .unsigned: break
            default: throw APIError.badURL
            }
        }
        let u32Fields: Set<String> = ["active_encode_milli_realtime", "active_encode_age_ms", "active_encode_active_ms", "active_encode_segments", "tone_map_peak_nits"]
        let wideFields: Set<String> = ["materialized_segments", "planned_segments", "materialized_bytes", "planned_bytes", "working_set_bytes", "working_set_budget_bytes", "completed_cache_bytes", "http_wait_count"]
        for key in requiredCounters.union(optionalCounters) {
            if case .unsigned(let number) = status[key], !wideFields.contains(key), number > UInt64(Int64.max) { throw APIError.badURL }
            if u32Fields.contains(key) {
                switch status[key] { case .integer(let n) where n > Int64(UInt32.max): throw APIError.badURL; case .unsigned(let n) where n > UInt64(UInt32.max): throw APIError.badURL; default: break }
            }
        }
        for key in requiredWords.union(optionalWords) {
            guard let value = status[key], value != .null else { continue }
            guard let text = value.string, isToken(text) else { throw APIError.badURL }
        }
        for key in booleans { guard case .bool = status[key] else { throw APIError.badURL } }
        if let candidate = status["active_encode_candidate_id"], candidate != .null {
            guard let value = candidate.string, PlaybackFileContext.matches(value, "^[0-9a-f]{32}$") else { throw APIError.badURL }
        }
        if let seconds = status["server_ready_seconds"], seconds != .null {
            let value: Double
            switch seconds { case .number(let n): value = n; case .integer(let n): value = Double(n); case .unsigned(let n): value = Double(n); default: throw APIError.badURL }
            guard value.isFinite, value >= 0 else { throw APIError.badURL }
        }
        guard case .integer(let height) = status["target_height"], (1...16384).contains(height),
              let encoder = status["encoder"]?.string, let producer = status["producer_state"]?.string,
              let ready = status["server_ready_state"]?.string else { throw APIError.badURL }
        var ahead: Int64?
        if case .integer(let value)? = status["ahead_seconds"] { ahead = value }
        return Self(wire: status, targetHeight: height, encoder: encoder, producerState: producer, serverReadyState: ready, aheadSeconds: ahead)
    }
}
