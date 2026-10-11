import Foundation

/// Exact B01 cinema.remote.v1 vocabulary. HTTP DTOs are owned by B04.
enum CinemaRemoteOutcome: String, Codable, Error {
    case applied, duplicateOrOld = "duplicate_or_old", expired
    case staleTarget = "stale_target", staleControl = "stale_control"
    case staleContext = "stale_context", staleFocus = "stale_focus"
    case restrictedSurface = "restricted_surface", unauthorized, unsupported, busy, unavailable, invalid
}
struct CinemaRemoteTarget: Codable, Hashable {
    let ownerNodeID: String
    let sessionID: UUID
    let receiverEpoch: UUID
    enum CodingKeys: String, CodingKey {
        case ownerNodeID = "owner_node_id", sessionID = "session_id", receiverEpoch = "receiver_epoch"
    }
}
enum CinemaRemoteTrackKind: String, Codable { case audio, subtitles, quality }
enum CinemaRemoteDirection: String, Codable { case up, down, left, right }
enum CinemaRemoteCreditKind: String, Codable {
    case interaction, playback
    var ttlMs: UInt64 { self == .interaction ? 1_000 : 3_000 }
}
struct CinemaRemoteCredit: Codable, Equatable { let nonce: UUID; let kind: CinemaRemoteCreditKind }
struct CinemaRemoteAction: Codable, Equatable {
    enum Kind: String, Codable {
        case navigate, select, back, home, stop
        case setPlaying = "set_playing", seekRelative = "seek_relative", seekAbsolute = "seek_absolute"
        case openTracks = "open_tracks", chooseTrack = "choose_track", textReplace = "text_replace", playItem = "play_item"
    }
    let type: Kind
    var direction: CinemaRemoteDirection?
    var playing: Bool?
    var seconds: Int?
    var positionMs: UInt64?
    var kind: CinemaRemoteTrackKind?
    var optionID: String?
    var textNonce: UUID?
    var text: String?
    var itemID: UInt64?
    var creditKind: CinemaRemoteCreditKind {
        switch type {
        case .navigate, .select, .back, .home, .textReplace, .openTracks: return .interaction
        default: return .playback
        }
    }
    enum CodingKeys: String, CodingKey {
        case type, direction, playing, seconds, kind, text
        case positionMs = "position_ms", optionID = "option_id", textNonce = "text_nonce", itemID = "item_id"
    }
    var fieldNames: Set<String> {
        switch type {
        case .navigate: return ["type", "direction"]
        case .setPlaying: return ["type", "playing"]
        case .seekRelative: return ["type", "seconds"]
        case .seekAbsolute: return ["type", "position_ms"]
        case .openTracks: return ["type", "kind"]
        case .chooseTrack: return ["type", "kind", "option_id"]
        case .textReplace: return ["type", "text_nonce", "text"]
        case .playItem: return ["type", "item_id"]
        default: return ["type"]
        }
    }
    func validate() throws {
        let present: Set<String> = Set(["type"])
            .union(direction == nil ? [] : ["direction"])
            .union(playing == nil ? [] : ["playing"])
            .union(seconds == nil ? [] : ["seconds"])
            .union(positionMs == nil ? [] : ["position_ms"])
            .union(kind == nil ? [] : ["kind"])
            .union(optionID == nil ? [] : ["option_id"])
            .union(textNonce == nil ? [] : ["text_nonce"])
            .union(text == nil ? [] : ["text"])
            .union(itemID == nil ? [] : ["item_id"])
        guard present == fieldNames else { throw CinemaRemoteOutcome.invalid }
        if let seconds, ![-30, -10, 10, 30].contains(seconds) { throw CinemaRemoteOutcome.invalid }
        if let positionMs, positionMs > CinemaRemoteCommand.maximumInteger { throw CinemaRemoteOutcome.invalid }
        if let itemID, itemID == 0 || itemID > CinemaRemoteCommand.maximumInteger { throw CinemaRemoteOutcome.invalid }
        if let optionID, optionID.isEmpty || optionID.utf8.count > 128 { throw CinemaRemoteOutcome.invalid }
        if let text, text.utf8.count > 512 { throw CinemaRemoteOutcome.invalid }
    }
}
struct CinemaRemoteCommand: Codable, Equatable {
    static let versionName = "cinema.remote.v1"
    static let maximumInteger: UInt64 = 9_007_199_254_740_991
    static let maximumBytes = 16 * 1_024
    var version = versionName
    let target: CinemaRemoteTarget
    let grantID: UUID
    let controlEpoch: UUID
    let sequence: UInt64
    let credit: UUID
    let contextRevision: UInt64
    let focusRevision: UInt64
    let action: CinemaRemoteAction
    enum CodingKeys: String, CodingKey {
        case version, target, sequence, credit, action
        case grantID = "grant_id", controlEpoch = "control_epoch"
        case contextRevision = "context_revision", focusRevision = "focus_revision"
    }
    func validate() throws {
        guard version == Self.versionName, !target.ownerNodeID.isEmpty, target.ownerNodeID.utf8.count <= 128,
              !target.ownerNodeID.unicodeScalars.contains(where: { $0.properties.generalCategory == .control }),
              [sequence, contextRevision, focusRevision].allSatisfy({ $0 > 0 && $0 <= Self.maximumInteger }) else { throw CinemaRemoteOutcome.invalid }
        try action.validate()
    }
    static func decode(_ data: Data) throws -> Self {
        guard data.count <= maximumBytes else { throw CinemaRemoteOutcome.invalid }
        try RemoteStrictJSON.check(data)
        guard let object = try JSONSerialization.jsonObject(with: data) as? [String: Any],
              Set(object.keys) == Set(["version", "target", "grant_id", "control_epoch", "sequence", "credit", "context_revision", "focus_revision", "action"]),
              let target = object["target"] as? [String: Any],
              Set(target.keys) == Set(["owner_node_id", "session_id", "receiver_epoch"]),
              let action = object["action"] as? [String: Any] else { throw CinemaRemoteOutcome.invalid }
        let value = try JSONDecoder().decode(Self.self, from: data)
        guard Set(action.keys) == value.action.fieldNames else { throw CinemaRemoteOutcome.invalid }
        try value.validate()
        return value
    }
    func encode() throws -> Data {
        try validate()
        let data = try JSONEncoder().encode(self)
        guard data.count <= Self.maximumBytes else { throw CinemaRemoteOutcome.invalid }
        return data
    }
}

/// JSONDecoder accepts duplicate keys and integral floating literals which
/// Rust serde does not. Reject those before using its bounded schema decoder.
enum RemoteStrictJSON {
    static func check(_ data: Data) throws {
        let bytes = Array(data)
        var stack: [Set<String>?] = []
        var index = 0
        while index < bytes.count {
            let byte = bytes[index]
            if byte == 123 { stack.append([]); guard stack.count <= 32 else { throw CinemaRemoteOutcome.invalid } }
            else if byte == 91 { stack.append(nil); guard stack.count <= 32 else { throw CinemaRemoteOutcome.invalid } }
            else if byte == 125 || byte == 93 { _ = stack.popLast() }
            else if byte == 34 {
                let start = index
                index += 1
                while index < bytes.count {
                    if bytes[index] == 92 { index += 2; continue }
                    if bytes[index] == 34 { break }
                    index += 1
                }
                guard index < bytes.count else { throw CinemaRemoteOutcome.invalid }
                var following = index + 1
                while following < bytes.count && [9, 10, 13, 32].contains(bytes[following]) { following += 1 }
                if following < bytes.count, bytes[following] == 58, let keys = stack.last ?? nil {
                    let key = try JSONDecoder().decode(String.self, from: Data(bytes[start...index]))
                    guard !keys.contains(key) else { throw CinemaRemoteOutcome.invalid }
                    stack[stack.count - 1]?.insert(key)
                }
            } else if byte == 45 || (48...57).contains(byte) {
                let start = index
                while index < bytes.count && ![9, 10, 13, 32, 44, 93, 125].contains(bytes[index]) { index += 1 }
                let token = bytes[start..<index]
                guard Array(token) != [45, 48], !token.contains(46), !token.contains(101), !token.contains(69) else { throw CinemaRemoteOutcome.invalid }
                index -= 1
            }
            index += 1
        }
        guard stack.isEmpty else { throw CinemaRemoteOutcome.invalid }
        _ = try JSONSerialization.jsonObject(with: data)
    }
}

extension CinemaRemoteOutcome {
    var viewerMessage: String {
        switch self {
        case .applied: return "Done."
        case .duplicateOrOld: return "Already handled."
        case .expired: return "The TV state changed. Try again."
        case .staleTarget: return "This TV session ended. Choose the TV again."
        case .staleControl: return "Remote control changed. Tap Use as remote again."
        case .staleContext, .staleFocus: return "The TV screen changed. Try again."
        case .restrictedSurface: return "Use the TV remote to finish this screen."
        case .unauthorized: return "Pair this phone again."
        case .unsupported: return "This control is not available here."
        case .busy: return "The TV is busy. Try again shortly."
        case .unavailable: return "The TV is unavailable."
        case .invalid: return "This control could not be used."
        }
    }
}
