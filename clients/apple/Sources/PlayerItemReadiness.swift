import AVFoundation
import Foundation

/// Server preparation and native item readiness have different clocks. A
/// growing playlist can legitimately be unpublished after the ordinary item
/// deadline; only fresh progress for this exact session earns additional time.
enum PlayerItemReadiness {
    struct Sample {
        let status: PlaybackSessionStatus
        let requestedAtMs: Int
        let observedAtMs: Int

        func ageMs(at nowMs: Int, sessionID: String) -> Int? {
            guard status.id == sessionID,
                  requestedAtMs >= 0, observedAtMs >= requestedAtMs,
                  nowMs >= observedAtMs, nowMs - requestedAtMs <= 5_000 else { return nil }
            return nowMs - requestedAtMs
        }
    }

    struct Context {
        var growingSessionID: String?
        var ownsItem: @MainActor () -> Bool = { true }
        var sample: @MainActor () -> Sample? = { nil }
    }

    struct Budget {
        let startedAtMs: Int
        let growingSessionID: String?
        private(set) var publishedAtMs: Int?

        mutating func remainingMs(at nowMs: Int, sample: Sample?) -> Int {
            guard nowMs >= startedAtMs else { return 0 }
            let elapsed = nowMs - startedAtMs
            let ordinary = PlayerController.itemReadinessDeadlineSeconds * 1_000
            guard let growingSessionID else { return max(0, ordinary - elapsed) }
            let absoluteRemaining = max(0, 60_000 - elapsed)
            guard absoluteRemaining > 0 else { return 0 }
            let age = sample?.ageMs(at: nowMs, sessionID: growingSessionID)
            if publishedAtMs == nil, age != nil, let sample, sample.status.playlistReady == true {
                publishedAtMs = max(startedAtMs, sample.observedAtMs)
            }
            if let publishedAtMs {
                return min(absoluteRemaining, max(0, ordinary - (nowMs - publishedAtMs)))
            }
            if elapsed < ordinary { return min(absoluteRemaining, ordinary - elapsed) }
            guard let sample, let age,
                  sample.status.playlistReady == false,
                  sample.status.producerState == "running",
                  let produced = sample.status.producedEndMs, produced > 0,
                  let progress = sample.status.outTimeMs, progress >= 0,
                  let idle = sample.status.progressIdleMs, idle >= 0,
                  idle <= 10_000 - age else { return 0 }
            // Do not sleep past this evidence's lease. A new poll may renew it,
            // but neither producer restart nor new samples reset the total cap.
            return min(absoluteRemaining, min(5_001 - age, 10_001 - age - idle))
        }
    }

    /// This is the production observer/timer race. Injecting its clock and
    /// sleep lets tests exercise native failures, cancellation and the actual
    /// incident timeline without waiting a minute or depending on a network.
    @MainActor
    static func wait(
        events: AsyncStream<PlayerItemEvent>,
        status: @escaping @MainActor () -> AVPlayerItem.Status,
        failure: @escaping @MainActor () -> Error?,
        context: Context,
        nowMs: @escaping @MainActor () -> Int = { PlaybackControlSession.monotonicMs() },
        sleep: @escaping @MainActor (Int) async throws -> Void = {
            try await Task.sleep(for: .milliseconds($0))
        }
    ) async throws {
        let startedAtMs = nowMs()
        func ready() throws -> Bool {
            try Task.checkCancellation()
            guard context.ownsItem() else { throw CancellationError() }
            if status() == .failed { throw failure() ?? PlaybackPreparationError.failed }
            return status() == .readyToPlay
        }
        if try ready() { return }
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { @MainActor in
                for await event in events {
                    // Read current native state first: a queued ready event
                    // cannot hide a failure from the same scheduling turn.
                    if try ready() { return }
                    if case .status(.failed) = event {
                        throw failure() ?? PlaybackPreparationError.failed
                    }
                }
                try Task.checkCancellation()
                throw PlaybackPreparationError.failed
            }
            group.addTask { @MainActor in
                var budget = Budget(startedAtMs: startedAtMs, growingSessionID: context.growingSessionID)
                while true {
                    if try ready() { return }
                    let remaining = budget.remainingMs(at: nowMs(), sample: context.sample())
                    guard remaining > 0 else { throw PlaybackPreparationError.timedOut }
                    try await sleep(min(1_000, remaining))
                }
            }
            defer { group.cancelAll() }
            guard let _ = try await group.next() else { throw PlaybackPreparationError.failed }
            // Recheck after suspension before allowing the caller to seek.
            _ = try ready()
        }
    }
}
