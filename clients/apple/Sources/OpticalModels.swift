import Foundation

struct OpticalDriveStateDTO: Codable, Hashable {
    let state: String
    var mediaGeneration: String?
    var discId: String?
    var titleId: String?
    var sessionId: String?
    var reason: String?
}

struct OpticalRequirementDTO: Codable, Hashable, Identifiable {
    let id: String
    let status: String
    let detail: String
}

struct OpticalDiscSummaryDTO: Codable, Hashable {
    let id: String
    let mediaGeneration: String
    let format: String
    var displayTitle: String?
    var volumeLabel: String?
    var suggestedTitleId: String?

    var title: String { displayTitle ?? volumeLabel ?? "Inserted disc" }
}

struct OpticalDriveDTO: Codable, Hashable, Identifiable {
    let id: String
    let name: String
    let ownerNodeId: String
    let enabled: Bool
    let state: OpticalDriveStateDTO
    var requirements: [OpticalRequirementDTO]
    var disc: OpticalDiscSummaryDTO?
}

struct OpticalTitleSummaryDTO: Codable, Hashable, Identifiable {
    let id: String
    var durationMs: Int?
    let angles: Int
    var matchedItemId: Int?
    var matchKind: String?
}

struct OpticalDriveDiscDTO: Codable, Hashable {
    let drive: OpticalDriveDTO
    var titles: [OpticalTitleSummaryDTO]
}

struct OpticalDiscDTO: Codable, Hashable {
    let discId: String
    let format: String
    var volumeLabel: String?
    var displayTitle: String?

    var title: String { displayTitle ?? volumeLabel ?? "Inserted disc" }
}

struct OpticalTrackDTO: Codable, Hashable, Identifiable {
    var index: Int?
    let codec: String
    var channels: Int?
    var language: String?
    var title: String?
    var `default`: Bool?
    var forced: Bool?

    var id: String { "\(index ?? -1):\(language ?? ""):\(title ?? ""):\(codec)" }
}

struct OpticalFactsDTO: Codable, Hashable {
    var videoCodec: String?
    var width: Int?
    var height: Int?
    var hdr: String?
    var durationMs: Int?
    var audioStreams: [OpticalTrackDTO]?
    var subtitleStreams: [OpticalTrackDTO]?
}

struct OpticalTitleDTO: Codable, Hashable {
    let discId: String
    let titleId: String
    let angles: Int
    var facts: OpticalFactsDTO
    var durationMs: Int?
    var matchedItemId: Int?
    var matchKind: String?
}

struct OpticalChapterDTO: Codable, Hashable, Identifiable {
    var index: Int?
    var startMs: Int?
    var endMs: Int?

    var id: String { "\(index ?? 0):\(startMs ?? 0)" }
}

struct OpticalProgressDTO: Codable, Hashable {
    let discId: String
    let titleId: String
    let angle: Int
    let positionMs: Int
    var durationMs: Int?
    let watched: Bool
    var updatedAtMs: Int?
}

struct OpticalTitleDetailDTO: Codable, Hashable {
    let disc: OpticalDiscDTO
    let title: OpticalTitleDTO
    var chapters: [OpticalChapterDTO]
    var progress: OpticalProgressDTO?
}

struct OpticalDecisionRequest: Codable {
    let expectedDiscId: String
    let mediaGeneration: String
    let angle: Int
    let caps: DeviceCaps
    var force: String?
    var audio: Int?
    var subtitle: Int?
}

struct OpticalDecisionDTO: Codable {
    let method: String
    let playUrl: String
    var source: OpticalFactsDTO?
    var audio: [OpticalTrackDTO]
    var subtitles: [OpticalTrackDTO]
    var ladder: [QualityRung]
    var deliveredDynamicRange: String?
}

extension OpticalDecisionDTO {
    func playerDecision(selection: PrePlaySelection) -> Decision {
        Decision(
            fileId: nil,
            method: method,
            playUrl: playUrl,
            delivery: Delivery(
                mode: "transcode",
                url: nil,
                sessionsUrl: playUrl,
                aac: nil,
                requiresHls: true,
                preserveDolbyVision: false,
                audio: selection.audioIndex
            ),
            reasons: ["Managed optical source"],
            transcodeAudio: true,
            preserveDolbyVision: false,
            source: source.map {
                SourceSummary(
                    container: nil,
                    videoCodec: $0.videoCodec,
                    videoProfile: nil,
                    width: $0.width,
                    height: $0.height,
                    bitDepth: nil,
                    hdr: $0.hdr,
                    hdrFormat: nil,
                    bitrate: nil,
                    durationMs: $0.durationMs
                )
            },
            audio: audio.compactMap { track in
                track.index.map {
                    AudioTrack(
                        index: $0,
                        codec: track.codec,
                        channels: track.channels,
                        language: track.language,
                        title: track.title,
                        default: track.default ?? false
                    )
                }
            },
            subtitles: subtitles.compactMap { track in
                track.index.map {
                    SubtitleTrack(
                        index: $0,
                        codec: track.codec,
                        language: track.language,
                        title: track.title,
                        default: track.default ?? false,
                        forced: track.forced ?? false,
                        text: false,
                        native: false,
                        overlay: nil
                    )
                }
            },
            selection: DecisionSelection(
                audioIndex: selection.audioIndex,
                subtitleIndex: selection.subtitleIndex.flatMap { $0 >= 0 ? $0 : nil },
                subtitleRequiresBurnIn: selection.subtitleIndex.map { $0 >= 0 },
                subtitleBurnInBlockedByHdr: false
            ),
            markers: nil,
            audioOffsetMs: 0,
            declaredOffsetMs: 0,
            ladder: ladder,
            deliveredDynamicRange: deliveredDynamicRange,
            deliveredDolbyVisionProfile: nil
        )
    }
}

struct OpticalSessionRequest: Codable {
    let expectedDiscId: String
    let mediaGeneration: String
    let angle: Int
    let playbackId: String
    let requestId: String
    let start: Double
    var height: Int?
    var audio: Int?
    var subtitleBurn: Int?
    var audioOffsetMs: Int?
    var blockBudgetSecs: Double?
    let caps: DeviceCaps
}

struct OpticalProgressRequest: Codable {
    let driveId: String
    let mediaGeneration: String
    let sessionId: String
    let angle: Int
    let positionMs: Int
    var durationMs: Int?
    var audio: Int?
    var subtitle: Int?
    let recordedAtMs: Int
}

struct OpticalEjectRequest: Codable {
    let mediaGeneration: String
    var sessionId: String?
    let stopActive: Bool
}
