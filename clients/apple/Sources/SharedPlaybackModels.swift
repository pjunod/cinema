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
              response.durationMs.map({ $0 >= 0 }) ?? true,
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

