import Foundation
import CryptoKit

/// Source-clock captions have an identity separate from source subtitle indices.
enum CaptionIntent: Equatable {
    case off
    case source(Int)
    case progressive(String?)
}

enum CaptionTask: String, Codable, CaseIterable, Identifiable {
    case transcribe, translate
    var id: String { rawValue }
    var label: String { self == .transcribe ? "Generate in spoken language" : "Translate speech to English" }
}

enum CaptionProtocolError: LocalizedError {
    case invalid, budget, expired
    var errorDescription: String? {
        switch self {
        case .invalid: return "The caption response failed identity or format validation."
        case .budget: return "The caption window exceeded its memory limit."
        case .expired: return "Caption coverage changed. Refresh this window."
        }
    }
}

enum CaptionValue: Encodable {
    case string(String), integer(Int), number(Double), bool(Bool), array([String]), null
    func encode(to encoder: Encoder) throws {
        var box = encoder.singleValueContainer()
        switch self {
        case .string(let value): try box.encode(value)
        case .integer(let value): try box.encode(value)
        case .number(let value): try box.encode(value)
        case .bool(let value): try box.encode(value)
        case .array(let value): try box.encode(value)
        case .null: try box.encodeNil()
        }
    }
}

enum CaptionValidation {
    static func digest(_ value: String) -> Bool {
        value.utf8.count == 64 && value.utf8.allSatisfy { (48...57).contains($0) || (97...102).contains($0) }
    }
    static func uuid(_ value: String) -> Bool { UUID(uuidString: value)?.uuidString.lowercased() == value }
    static func counter(_ value: String) -> Bool {
        !value.isEmpty && value.utf8.allSatisfy { (48...57).contains($0) } &&
        (value == "0" || value.first != "0") && value.count <= 19 &&
        (value.count < 19 || value <= "9223372036854775807")
    }
    static func compare(_ a: String, _ b: String) -> ComparisonResult {
        if a.count != b.count { return a.count < b.count ? .orderedAscending : .orderedDescending }
        return a == b ? .orderedSame : a < b ? .orderedAscending : .orderedDescending
    }
    static func next(_ value: String) throws -> String {
        guard counter(value), let n = Int64(value), n < Int64.max else { throw CaptionProtocolError.invalid }
        return String(n + 1)
    }
    static func range(_ start: Int, _ end: Int, duration: Int) -> Bool {
        start >= 0 && end > start && end <= duration && duration <= 14_400_000
    }
    static func text(_ value: String) -> Bool {
        !value.isEmpty && !value.unicodeScalars.contains { (0...8).contains($0.value) || (11...31).contains($0.value) || (127...159).contains($0.value) }
    }
    static func sha256(_ bytes: Data) -> String { SHA256.hash(data: bytes).map { String(format: "%02x", $0) }.joined() }
}

struct CaptionLimits: Decodable {
    let segmentBytes: Int, segmentCues: Int, pageBytes: Int, pageEntries: Int
    let coverageIntervals: Int, concurrentFetches: Int, cueMemoryBytes: Int, cueCount: Int
    let cueWindowBeforeMs: Int, cueWindowAfterMs: Int, pollMinMs: Int, pollMaxMs: Int
    let retryMaxMs: Int, heartbeatIntervalMs: Int
    func validate() throws {
        let values = [segmentBytes, segmentCues, pageBytes, pageEntries, coverageIntervals, concurrentFetches,
                      cueMemoryBytes, cueCount, cueWindowBeforeMs, cueWindowAfterMs, pollMinMs, pollMaxMs,
                      retryMaxMs, heartbeatIntervalMs]
        guard values.allSatisfy({ $0 > 0 }), segmentBytes <= 65_536, pageBytes <= 65_536,
              pageEntries <= 128, concurrentFetches <= 16, cueMemoryBytes <= 1_048_576,
              cueCount <= 2048, segmentCues <= 256, pollMinMs >= 250, pollMaxMs <= 30_000,
              heartbeatIntervalMs <= 30_000, retryMaxMs <= 60_000 else { throw CaptionProtocolError.budget }
    }
}
struct CaptionProfile: Decodable, Identifiable {
    let profileId: String, modelId: String, modelSha256: String
    let task: CaptionTask
    let label: String?
    let `default`: Bool?
    let outputLanguage: String?
    let inputLanguages: [String]
    let timelineContract: String
    let limits: CaptionLimits
    var id: String { profileId }
    func validate() throws {
        try limits.validate()
        guard CaptionValidation.digest(profileId), CaptionValidation.digest(modelSha256),
              !modelId.isEmpty, modelId.utf8.count <= 64,
              modelId.utf8.allSatisfy({ (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0) || [45, 46, 95].contains($0) }), inputLanguages.count <= 128,
              !inputLanguages.isEmpty, timelineContract == "source-audio-clock-v1" else { throw CaptionProtocolError.invalid }
    }
}
struct CaptionAudio: Decodable, Identifiable {
    let index: Int
    let sourceIndex: Int?
    let audioOrdinal: Int?
    let language: String?, normalizedLanguage: String?, title: String?
    var id: Int { audioOrdinal ?? index }
    var ordinal: Int { audioOrdinal ?? index }
}
struct CaptionPermissions: Decodable { let read: Bool, demand: Bool, stop: Bool, resume: Bool }
struct CaptionCapability: Decodable { let supported: Bool; let reason: String? }
struct CaptionAsset: Decodable, Identifiable {
    let assetId: String, profileId: String, operationId: String
    let audioIndex: Int
    let spokenLanguage: String
    let outputLanguage: String?
    let task: CaptionTask?
    var id: String { assetId }
}
struct CaptionOptions: Decodable {
    let version: Int, fileId: String, sourceSize: String, sourceMtime: String, durationMs: Int
    let audioTracks: [CaptionAudio], models: [CaptionProfile], capability: CaptionCapability
    let permissions: CaptionPermissions, assets: [CaptionAsset]
    let assetsNextCursor: String?
    let defaultProfileId: String?
    let serverNowMs: Int
    func validate(file: Int) throws {
        guard version == 1, fileId == String(file), CaptionValidation.counter(sourceSize),
              Int64(sourceMtime) != nil, durationMs > 0, durationMs <= 14_400_000,
              audioTracks.count <= 256, models.count <= 16, assets.count <= 64,
              defaultProfileId.map({ hint in CaptionValidation.digest(hint) && models.contains(where: { $0.profileId == hint }) }) ?? true,
              Set(audioTracks.map(\.ordinal)).count == audioTracks.count,
              audioTracks.allSatisfy({ (0...255).contains($0.ordinal) }) else { throw CaptionProtocolError.invalid }
        for model in models { try model.validate() }
    }
}
struct CaptionReceipt: Decodable {
    let version: Int, outcome: String, operationId: String, operationEpoch: String, viewRevision: String
    let demandId: String?, demandGeneration: String?, continuityId: String?, assetId: String?, state: String?
    let serverNowMs: Int
    func validate(continuity expected: String? = nil) throws {
        guard version == 1, CaptionValidation.uuid(operationId), CaptionValidation.counter(operationEpoch),
              CaptionValidation.counter(viewRevision), demandId.map(CaptionValidation.uuid) ?? true,
              demandGeneration.map(CaptionValidation.counter) ?? true,
              continuityId.map(CaptionValidation.uuid) ?? true,
              expected == nil || demandId == nil || continuityId == expected,
              assetId.map(CaptionValidation.digest) ?? true else { throw CaptionProtocolError.invalid }
    }
}
struct CaptionFailedRange: Decodable, Identifiable {
    let coreIndex: Int, rangeRevision: String, retryable: Bool
    let reason: String?
    var id: Int { coreIndex }
}
struct CaptionOperation: Decodable {
    let version: Int, operationId: String, operationEpoch: String, viewRevision: String, state: String
    let reason: String?, assetId: String?, contentRevision: String?
    let fileId: String, sourceSize: String, sourceMtime: String
    let audioIndex: Int, spokenLanguage: String, outputLanguage: String, task: CaptionTask, profileId: String
    let retryable: Bool
    let failedRanges: [CaptionFailedRange]
    let failedRangesNextCursor: String?
    let serverNowMs: Int
}
struct CaptionRange: Decodable { let startMs: Int, endMs: Int }
struct CaptionSegmentReference: Decodable {
    let segmentId: String, digest: String
    let startMs: Int, endMs: Int
}
struct CaptionSnapshot: Decodable { let leaseId: String, revision: String; let expiresInMs: Int }
struct CaptionManifestOperation: Decodable {
    let operationId: String, epoch: String, state: String
    let reason: String?
}
struct CaptionManifest: Decodable {
    let version: Int, assetId: String, fileId: String, contentRevision: String, profileId: String
    let audioIndex: Int, spokenLanguage: String, outputLanguage: String, timelineContract: String
    let manifestRevision: String, viewRevision: String
    let operation: CaptionManifestOperation, snapshot: CaptionSnapshot
    let coverage: [CaptionRange], segments: [CaptionSegmentReference]
    let coverageNextCursor: String?, segmentNextCursor: String?
    let serverNowMs: Int
}
struct CaptionPage<T: Decodable>: Decodable {
    let version: Int, assetId: String, snapshotRevision: String
    let entries: [T], nextCursor: String?
}
struct CaptionCue: Decodable, Equatable {
    let cueId: String, startMs: Int, endMs: Int, text: String
}
struct CaptionProvenance: Decodable {
    let engineSha256: String, modelSha256: String, operationEpoch: String, platform: String
    let profileId: String, sourceSha256: String, workerFence: String
}
struct CaptionSegment: Decodable {
    let version: Int, assetId: String, segmentId: String, coreIndex: Int, startMs: Int, endMs: Int
    let cues: [CaptionCue], provenance: CaptionProvenance
    func validate(reference: CaptionSegmentReference, asset: String, source: String, profile: CaptionProfile, duration: Int) throws {
        guard version == 1, assetId == asset, segmentId == reference.segmentId,
              startMs == reference.startMs, endMs == reference.endMs, coreIndex >= 0,
              CaptionValidation.range(startMs, endMs, duration: duration), cues.count <= profile.limits.segmentCues,
              provenance.profileId == profile.profileId, provenance.sourceSha256 == source,
              provenance.modelSha256 == profile.modelSha256, CaptionValidation.digest(provenance.engineSha256),
              CaptionValidation.counter(provenance.operationEpoch), CaptionValidation.counter(provenance.workerFence),
              Set(cues.map(\.cueId)).count == cues.count else { throw CaptionProtocolError.invalid }
        for cue in cues {
            guard CaptionValidation.digest(cue.cueId), CaptionValidation.text(cue.text),
                  cue.startMs >= startMs, cue.endMs <= endMs, cue.endMs > cue.startMs else { throw CaptionProtocolError.invalid }
        }
    }
}
struct CaptionRenderWindow: Equatable {
    let revision: Int, baseMs: Int, offsetMs: Int
    let cues: [CaptionCue]
    static func itemInterval(_ cue: CaptionCue, baseMs: Int, offsetMs: Int) -> Range<Int>? {
        let start = max(0, cue.startMs - baseMs + offsetMs)
        let end = cue.endMs - baseMs + offsetMs
        return end > start ? start..<end : nil
    }
}

struct CaptionAssetPage: Decodable {
    let version: Int, fileId: String, entries: [CaptionAsset], nextCursor: String?
    let serverNowMs: Int
}
struct CaptionFailurePage: Decodable {
    let version: Int, operationId: String, operationEpoch: String, viewRevision: String
    let entries: [CaptionFailedRange], nextCursor: String?
}
