import AVFoundation
import Foundation

/// Facts about one attached item. Consumers decide what to do with them; a
/// stalled item and a new log entry are evidence, not terminal failures.
enum PlayerItemEvent: Sendable {
    case status(AVPlayerItem.Status)
    case timeControl(AVPlayer.TimeControlStatus, AVPlayer.WaitingReason?)
    case playedToEnd
    case failedToPlayToEnd(NSError?)
    case newErrorLogEntry
    case playbackStalled
    case interruption(AudioInterruptionResponse)
    case routeLost
}

/// Owns the observation tokens for exactly one item and its player. The
/// consumer's stream finishes when this item is replaced or playback stops.
@MainActor
final class AVPlayerItemObserver {
    let item: AVPlayerItem
    let events: AsyncStream<PlayerItemEvent>

    private let continuation: AsyncStream<PlayerItemEvent>.Continuation
    private var statusObservation: NSKeyValueObservation?
    private var timeControlObservation: NSKeyValueObservation?
    private var notificationTokens: [NSObjectProtocol] = []
    private var cancelled = false

    init(item: AVPlayerItem, player: AVPlayer? = nil) {
        self.item = item
        var streamContinuation: AsyncStream<PlayerItemEvent>.Continuation!
        events = AsyncStream(bufferingPolicy: .bufferingNewest(16)) {
            streamContinuation = $0
        }
        continuation = streamContinuation

        statusObservation = item.observe(\.status, options: [.initial, .new]) { [weak self] item, _ in
            let status = item.status
            Task { @MainActor [weak self] in self?.emit(.status(status)) }
        }
        if let player {
            timeControlObservation = player.observe(\.timeControlStatus, options: [.initial, .new]) {
                [weak self] player, _ in
                let status = player.timeControlStatus
                let reason = player.reasonForWaitingToPlay
                Task { @MainActor [weak self] in self?.emit(.timeControl(status, reason)) }
            }
        }

        let center = NotificationCenter.default
        notificationTokens.append(center.addObserver(
            forName: .AVPlayerItemDidPlayToEndTime, object: item, queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.emit(.playedToEnd) }
        })
        notificationTokens.append(center.addObserver(
            forName: .AVPlayerItemFailedToPlayToEndTime, object: item, queue: .main
        ) { [weak self] notification in
            let error = notification.userInfo?[AVPlayerItemFailedToPlayToEndTimeErrorKey] as? NSError
            Task { @MainActor [weak self] in self?.emit(.failedToPlayToEnd(error)) }
        })
        notificationTokens.append(center.addObserver(
            forName: .AVPlayerItemNewErrorLogEntry, object: item, queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.emit(.newErrorLogEntry) }
        })
        notificationTokens.append(center.addObserver(
            forName: .AVPlayerItemPlaybackStalled, object: item, queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in self?.emit(.playbackStalled) }
        })
    }

    private func emit(_ event: PlayerItemEvent) {
        guard !cancelled else { return }
        continuation.yield(event)
    }

    func cancel() {
        guard !cancelled else { return }
        cancelled = true
        statusObservation = nil
        timeControlObservation = nil
        for token in notificationTokens { NotificationCenter.default.removeObserver(token) }
        notificationTokens.removeAll()
        continuation.finish()
    }
}

/// The error's own identity wins over an unrelated entry in the append-only
/// AVFoundation log. A log entry is usable only for the named failed URL.
enum PlayerItemFailure {
    struct LogEntry {
        let uri: String?
        let domain: String?
        let status: Int
        let comment: String?

        init(uri: String?, domain: String?, status: Int, comment: String?) {
            self.uri = uri
            self.domain = domain
            self.status = status
            self.comment = comment
        }

        init(_ event: AVPlayerItemErrorLogEvent) {
            self.init(
                uri: event.uri,
                domain: event.errorDomain,
                status: event.errorStatusCode,
                comment: event.errorComment
            )
        }
    }

    struct Detail {
        let error: NSError?
        let eventDomain: String?
        let eventStatus: Int?
        let eventComment: String?
    }

    static func classify(
        fatal: NSError?,
        item: NSError?,
        log: [LogEntry],
        failedURI: String?
    ) -> Detail? {
        if let fatal {
            return Detail(error: fatal, eventDomain: nil, eventStatus: nil, eventComment: nil)
        }
        if let item {
            return Detail(error: item, eventDomain: nil, eventStatus: nil, eventComment: nil)
        }
        guard let failedURI,
              let event = log.last(where: { $0.uri == failedURI })
        else { return nil }
        return Detail(
            error: nil,
            eventDomain: event.domain,
            eventStatus: event.status,
            eventComment: event.comment
        )
    }
}
