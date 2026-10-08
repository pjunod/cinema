import Combine
import Foundation

struct CinemaRemoteTrackOption: Codable {
    let kind: CinemaRemoteTrackKind
    let optionID: String
    let label: String
    enum CodingKeys: String, CodingKey { case kind, optionID = "option_id", label }
}
struct CinemaRemoteMedia: Codable, Equatable {
    enum Kind: String, Codable { case item, liveChannel = "live_channel" }
    let type: Kind
    let itemID: UInt64?
    let channelID: String?
    static func item(_ id: UInt64) -> Self { .init(type: .item, itemID: id, channelID: nil) }
    static func liveChannel(_ id: String) -> Self { .init(type: .liveChannel, itemID: nil, channelID: id) }
    enum CodingKeys: String, CodingKey { case type, itemID = "item_id", channelID = "channel_id" }
}
struct CinemaRemotePlaybackSummary: Codable {
    let media: CinemaRemoteMedia
    let title: String
    let playing: Bool
    let positionMs: UInt64
    let durationMs: UInt64
    let tracks: [CinemaRemoteTrackOption]
    enum CodingKeys: String, CodingKey {
        case media, title, playing, positionMs = "position_ms", durationMs = "duration_ms", tracks
    }
}

/// Keeps callbacks scoped to the existing visible player, not a new AVPlayer
/// or system command owner. Replacing an owner invalidates the UI context.
@MainActor
final class RemotePlaybackAdapter: ObservableObject {
    struct Owner {
        let token: UUID
        let scope: String
        let actions: Set<CinemaRemoteAction.Kind>
        let snapshot: () -> CinemaRemotePlaybackSummary?
        let dispatch: (CinemaRemoteAction) -> CinemaRemoteOutcome
        let cancelNetworkGesture: () -> Void
        var available: () -> Bool = { true }
        var actionAvailable: (CinemaRemoteAction.Kind) -> Bool = { _ in true }
        var deferredDidComplete: (CinemaRemoteAction, CinemaRemoteOutcome) -> Void = { _, _ in }
        var deferredDispatch: ((CinemaRemoteAction, @escaping () -> Bool) async -> CinemaRemoteOutcome)? = nil
    }
    @Published private(set) var owner: Owner?
    func attach(_ owner: Owner) { self.owner = owner }
    func detach(_ token: UUID) { if owner?.token == token { owner = nil } }
    func dispatch(_ action: CinemaRemoteAction, scope: String) -> CinemaRemoteOutcome {
        guard let owner, owner.scope == scope, owner.actions.contains(action.type) else { return .unsupported }
        return owner.dispatch(action)
    }
    func physicalInput() { owner?.cancelNetworkGesture() }
}

/// Resource settlement belongs to the existing session even when a semantic
/// operation is no longer allowed to touch its renderer or replacement owner.
@MainActor
struct SharedRemoteControlCompletion {
    static func finish(cleanup: (() async -> Void)?, permit: () -> Bool,
                       effect: () async -> CinemaRemoteOutcome) async -> CinemaRemoteOutcome {
        if let cleanup {
            let task = Task { await cleanup() }
            await task.value
        }
        guard permit(), !Task.isCancelled else { return .unavailable }
        return await effect()
    }
}
