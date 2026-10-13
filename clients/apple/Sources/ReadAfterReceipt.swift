import Foundation

/// A short-lived client floor, not the node's own write fence or an account
/// credential. Unknown writes retire the epoch so in-flight known replies
/// cannot make an older floor look current again.
final class ReadAfterReceipt: @unchecked Sendable {
    struct Request {
        let generation: UInt64
        let epoch: UInt64
        let index: String?
    }

    private let lock = NSLock()
    private let now: () -> TimeInterval
    private var generation: UInt64?
    private var epoch: UInt64 = 0
    private var index: UInt64?
    private var expires: TimeInterval = 0

    init(now: @escaping () -> TimeInterval = { ProcessInfo.processInfo.systemUptime }) {
        self.now = now
    }

    /// The caller captures credentials atomically. Recheck their generation
    /// inside this lock: a delayed old request must not reset a newer login.
    func request(generation captured: UInt64, currentGeneration: () -> UInt64) -> Request? {
        lock.lock(); defer { lock.unlock() }
        guard captured == currentGeneration() else { return nil }
        if generation != captured {
            generation = captured
            forget()
        }
        if now() >= expires { index = nil }
        return Request(generation: captured, epoch: epoch, index: index.map(String.init))
    }

    func observe(_ value: String?, mutating: Bool, request: Request,
                 currentGeneration: () -> UInt64) {
        lock.lock(); defer { lock.unlock() }
        guard request.generation == currentGeneration(), generation == request.generation else { return }
        guard let value else {
            if mutating { forget() }
            return
        }
        guard let parsed = UInt64(value), parsed > 0, String(parsed) == value else {
            forget()
            return
        }
        guard request.epoch == epoch else { return }
        if index == nil || parsed >= index! {
            index = parsed
            expires = now() + 60
        }
    }

    func transportFailed(mutating: Bool, request: Request, currentGeneration: () -> UInt64) {
        guard mutating else { return }
        lock.lock(); defer { lock.unlock() }
        guard request.generation == currentGeneration(), generation == request.generation else { return }
        forget()
    }

    private func forget() {
        epoch &+= 1
        index = nil
        expires = 0
    }
}
