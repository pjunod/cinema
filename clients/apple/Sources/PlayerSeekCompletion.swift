import Foundation

/// Bounds the caller even when AVFoundation never invokes its completion or
/// cooperates with task cancellation. A late result can settle the wait once.
/// Operation closures capture the player adapter, never the controller owner.
@MainActor
final class PlayerSeekCompletion {
    enum Outcome: Equatable {
        case finished(Bool)
        case timedOut
        case cancelled
    }

    private var outcome: Outcome?
    private var continuation: CheckedContinuation<Outcome, Never>?
    private var operationTask: Task<Void, Never>?
    private var deadlineTask: Task<Void, Never>?

    static func run(
        operation: @escaping @MainActor () async -> Bool,
        waitForDeadline: @escaping @MainActor () async throws -> Void
    ) async -> Outcome {
        let completion = PlayerSeekCompletion()
        return await withTaskCancellationHandler {
            await withCheckedContinuation { continuation in
                completion.continuation = continuation
                if let outcome = completion.outcome {
                    completion.continuation = nil
                    continuation.resume(returning: outcome)
                    return
                }
                guard !Task.isCancelled else {
                    completion.finish(.cancelled)
                    return
                }
                completion.operationTask = Task { [weak completion] in
                    let finished = await operation()
                    completion?.finish(.finished(finished))
                }
                completion.deadlineTask = Task { [weak completion] in
                    do { try await waitForDeadline() }
                    catch { return }
                    guard !Task.isCancelled else { return }
                    completion?.finish(.timedOut)
                }
            }
        } onCancel: {
            Task { @MainActor in completion.finish(.cancelled) }
        }
    }

    /// Run a native operation only while its captured owner still holds it.
    /// Cancellation and timeout return independently of its callback; disposing
    /// the native request releases that callback without letting it publish late.
    static func runOwned(
        operation: @escaping @MainActor () async -> Bool,
        cancelOperation: @escaping @MainActor () -> Void,
        ownsOperation: @escaping @MainActor () -> Bool,
        waitForDeadline: @escaping @MainActor () async throws -> Void
    ) async -> Outcome {
        guard !Task.isCancelled, ownsOperation() else { return .cancelled }
        var began = false
        let outcome = await run(operation: {
            guard !Task.isCancelled, ownsOperation() else { return false }
            began = true
            return await operation()
        }, waitForDeadline: waitForDeadline)
        let stillOwned = !Task.isCancelled && ownsOperation()
        if outcome != .finished(true) || !stillOwned {
            if began { cancelOperation() }
        }
        return stillOwned ? outcome : .cancelled
    }

    private func finish(_ result: Outcome) {
        guard outcome == nil else { return }
        outcome = result
        operationTask?.cancel()
        deadlineTask?.cancel()
        operationTask = nil
        deadlineTask = nil
        continuation?.resume(returning: result)
        continuation = nil
    }
}
