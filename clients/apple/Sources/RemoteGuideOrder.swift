import Foundation

/// Programme times, not just channel identity, define guide focus identity.
enum RemoteGuideOrder {
    static func keys(_ rows: [(channelID: String, programmeStarts: [Int])]) -> [String] {
        ["live:channels", "live:guide", "live:guide-time", "live:captions"] + rows.flatMap { row in
            ["live:channel:" + row.channelID] + row.programmeStarts.map { "live:programme:" + row.channelID + ":" + String($0) }
        }
    }
}
