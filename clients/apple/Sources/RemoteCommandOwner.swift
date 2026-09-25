import Foundation
import MediaPlayer

/// The system command center broadcasts to every registered target. Keep a
/// single active stack so a finite title cannot also react to Live TV's Play.
/// Releasing a temporary stack restores the most recent still-active owner.
final class RemoteCommandOwner: @unchecked Sendable {
    static let shared = RemoteCommandOwner()

    private let lock = NSLock()
    private var owners: [UUID] = []
    private var restore: [UUID: @MainActor () -> Void] = [:]

    private init() {}

    func claim(_ token: UUID, onRestore: @escaping @MainActor () -> Void) {
        lock.lock()
        owners.removeAll { $0 == token }
        owners.append(token)
        restore[token] = onRestore
        lock.unlock()
    }

    func release(_ token: UUID) {
        lock.lock()
        let wasCurrent = owners.last == token
        owners.removeAll { $0 == token }
        restore.removeValue(forKey: token)
        let resume = wasCurrent ? owners.last.flatMap { restore[$0] } : nil
        lock.unlock()
        if let resume { Task { @MainActor in resume() } }
    }

    func isCurrent(_ token: UUID) -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return owners.last == token
    }
}

/// Play and Pause for the two live stacks. They intentionally publish no
/// elapsed position: both follow a moving live boundary, not a seekable film.
@MainActor
final class LiveRemoteCommands {
    private let token = UUID()
    private var targets: [(MPRemoteCommand, Any)] = []
    private var title = ""
    private var playing = false

    func start(
        title: String,
        playing: Bool,
        play: @escaping @MainActor () -> Void,
        pause: @escaping @MainActor () -> Void,
        toggle: @escaping @MainActor () -> Void
    ) {
        self.title = title
        self.playing = playing
        RemoteCommandOwner.shared.claim(token) { [weak self] in self?.publish() }
        if targets.isEmpty {
            let commands = MPRemoteCommandCenter.shared()
            let token = token
            let actions: [(MPRemoteCommand, @MainActor () -> Void)] = [
                (commands.playCommand, play),
                (commands.pauseCommand, pause),
                (commands.togglePlayPauseCommand, toggle),
            ]
            for (command, action) in actions {
                command.isEnabled = true
                let target = command.addTarget { _ in
                    guard RemoteCommandOwner.shared.isCurrent(token) else { return .commandFailed }
                    Task { @MainActor in action() }
                    return .success
                }
                targets.append((command, target))
            }
        }
        publish()
    }

    func update(title: String, playing: Bool) {
        guard self.title != title || self.playing != playing else { return }
        self.title = title
        self.playing = playing
        publish()
    }

    func stop() {
        let wasCurrent = RemoteCommandOwner.shared.isCurrent(token)
        for (command, target) in targets { command.removeTarget(target) }
        targets = []
        RemoteCommandOwner.shared.release(token)
        if wasCurrent {
            MPNowPlayingInfoCenter.default().nowPlayingInfo = nil
            MPNowPlayingInfoCenter.default().playbackState = .stopped
        }
    }

    private func publish() {
        guard RemoteCommandOwner.shared.isCurrent(token) else { return }
        MPNowPlayingInfoCenter.default().nowPlayingInfo = [
            MPMediaItemPropertyTitle: title,
            MPNowPlayingInfoPropertyIsLiveStream: true,
            MPNowPlayingInfoPropertyPlaybackRate: playing ? 1.0 : 0.0,
        ]
        MPNowPlayingInfoCenter.default().playbackState = playing ? .playing : .paused
    }
}
