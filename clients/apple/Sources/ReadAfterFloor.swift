import Foundation

/// The signed-in session's watch-write floor: the highest Raft commit index a
/// watch write of ours is known to have reached, echoed so a replica may answer
/// the next watch read locally behind its fence instead of asking the leader
/// (`docs/cluster/BOUNDED-REPLICA-READS-ROLLOUT.md` §3.3).
///
/// This is the native port of the web client's `READ_AFTER`
/// (`crates/plurxd/src/web/core/api.js`), rule for rule, and its tests mirror
/// `tests/web/read-after.test.js`. Sending nothing is always correct — the
/// server then answers watch reads from Authority, merely slower — so every
/// doubt resolves to forgetting, and forgetting is never wrong.
///
/// * Capture accepts only `^[1-9][0-9]{0,19}$` within `u64`. `unknown` or
///   anything malformed forgets.
/// * Forward only: a larger value replaces, an equal one restarts the expiry,
///   a smaller one is ignored.
/// * A value expires 60 s after it was last captured, on a clock that keeps
///   counting while the device sleeps.
/// * Each request is sent under a `Ticket` naming the epoch and the auth
///   generation it left under. A reply from an older generation is ignored
///   entirely; a valid index from an older epoch is ignored. Malformed and
///   `unknown` are checked *before* the epoch, so a stale malformed reply
///   still forgets.
/// * A mutation (neither GET nor HEAD) whose reply carries no header, or which
///   failed in transport, forgets: an older peer may have acknowledged a watch
///   write without an indexed receipt, or the write may have landed unseen.
/// * Every forget bumps the epoch, so indexed replies already in flight from
///   before an unknown write cannot reinstate a floor that write overtook.
/// * A change of account bumps the generation and the epoch and forgets.
///
/// The type holds one value per signed-in session, whichever node answers: no
/// origin is part of its state. It is lock-guarded and independent of the main
/// actor because native API requests run concurrently from any task.
final class ReadAfterFloor: @unchecked Sendable {
    /// Sent on API verbs while a value is held.
    static let requestHeader = "X-Plurx-Read-After"
    /// Read from every API verb's reply.
    static let responseHeader = "X-Plurx-Commit-Index"
    static let lifetimeNanoseconds: UInt64 = 60_000_000_000

    /// What one request was sent under. `index` is the header value to send,
    /// or nil to send none.
    struct Ticket: Equatable, Sendable {
        let generation: UInt64
        let epoch: UInt64
        let index: UInt64?
    }

    private let lock = NSLock()
    private let now: @Sendable () -> UInt64
    private var generation: UInt64 = 0
    private var epoch: UInt64 = 0
    private var index: UInt64?
    private var expiresAt: UInt64 = 0

    /// `now` is a monotonic nanosecond clock; tests inject their own.
    init(now: @escaping @Sendable () -> UInt64 = { ReadAfterFloor.monotonicNanoseconds() }) {
        self.now = now
    }

    /// `CLOCK_MONOTONIC` on Darwin keeps counting while the system sleeps, so a
    /// device woken after an hour does not echo a floor from before it slept.
    static func monotonicNanoseconds() -> UInt64 {
        clock_gettime_nsec_np(CLOCK_MONOTONIC)
    }

    /// Take the ticket a request is about to be sent under.
    func ticket() -> Ticket {
        lock.lock()
        defer { lock.unlock() }
        return Ticket(generation: generation, epoch: epoch, index: heldLocked())
    }

    /// The value a request sent now would carry. For tests and diagnostics.
    var held: UInt64? {
        lock.lock()
        defer { lock.unlock() }
        return heldLocked()
    }

    /// The credential behind every later request changed: sign-in, sign-out,
    /// a rotated token or another server. Replies to requests sent before it
    /// are ignored, and the previous account's floor is gone.
    func authorizationChanged() {
        lock.lock()
        generation &+= 1
        forgetLocked()
        lock.unlock()
    }

    /// Forget the floor on behalf of a request sent under `ticket`. A ticket
    /// from an earlier account has no say over this one.
    func forget(_ ticket: Ticket) {
        lock.lock()
        if ticket.generation == generation { forgetLocked() }
        lock.unlock()
    }

    /// One API reply arrived. `value` is its `x-plurx-commit-index` header,
    /// nil when it carried none.
    func observe(reply value: String?, method: String, ticket: Ticket) {
        lock.lock()
        defer { lock.unlock() }
        guard ticket.generation == generation else { return }
        guard let value else {
            // Older peers may acknowledge a write without an indexed receipt.
            if Self.isMutation(method) { forgetLocked() }
            return
        }
        guard let parsed = Self.parse(value) else {
            forgetLocked()
            return
        }
        // An unknown write invalidates already-in-flight indexed replies too.
        guard ticket.epoch == epoch else { return }
        if let current = heldLocked(), parsed < current { return }
        index = parsed
        expiresAt = now() &+ Self.lifetimeNanoseconds
    }

    /// One API request failed before any reply arrived. A mutation may have
    /// landed unseen, so the floor can no longer be trusted.
    func transportFailed(method: String, ticket: Ticket) {
        guard Self.isMutation(method) else { return }
        forget(ticket)
    }

    /// `^[1-9][0-9]{0,19}$` within `u64`, exactly as the web client and the
    /// server's `extract.rs` accept it. A header URLSession folded from two
    /// values ("12, 13") is malformed.
    static func parse(_ value: String) -> UInt64? {
        let bytes = Array(value.utf8)
        guard (1...20).contains(bytes.count),
              bytes[0] != UInt8(ascii: "0"),
              bytes.allSatisfy({ $0 >= UInt8(ascii: "0") && $0 <= UInt8(ascii: "9") })
        else { return nil }
        return UInt64(value)
    }

    static func isMutation(_ method: String) -> Bool {
        let method = method.uppercased()
        return method != "GET" && method != "HEAD"
    }

    private func heldLocked() -> UInt64? {
        guard let index else { return nil }
        guard now() < expiresAt else {
            self.index = nil
            return nil
        }
        return index
    }

    private func forgetLocked() {
        epoch &+= 1
        index = nil
        expiresAt = 0
    }
}
