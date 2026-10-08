import Foundation

/// Swift counterpart of B01 ReceiverGuard. All times are receiver-local
/// ContinuousClock elapsed milliseconds, never client wall/monotonic clocks.
@MainActor
final class RemoteReceiverGuard {
    struct Context: Equatable {
        let grantID: UUID
        let target: CinemaRemoteTarget
        let controlEpoch: UUID
        let contextRevision: UInt64
        let focusRevision: UInt64
        let textNonce: UUID?
    }
    struct Acknowledgement: Codable {
        let controlEpoch: UUID
        let sequence: UInt64
        let outcome: CinemaRemoteOutcome
        enum CodingKeys: String, CodingKey { case controlEpoch = "control_epoch", sequence, outcome }
    }
    private struct LocalCredit { let wire: CinemaRemoteCredit; let deadline: UInt64 }
    private struct LocalResult { let ack: Acknowledgement; let deadline: UInt64 }
    private var context: Context?
    private var active = false
    private var credits: [LocalCredit] = []
    private var results: [LocalResult] = []
    private var lastSequence: UInt64 = 0
    private var lastNow: UInt64?
    var currentCredits: [CinemaRemoteCredit] { credits.map(\.wire) }
    func invalidate() { credits.removeAll() }
    func deactivate() { active = false; invalidate(); results.removeAll() }
    func setContext(_ value: Context) throws {
        guard [value.contextRevision, value.focusRevision].allSatisfy({ $0 > 0 && $0 <= CinemaRemoteCommand.maximumInteger }),
              !value.target.ownerNodeID.isEmpty, value.target.ownerNodeID.utf8.count <= 128 else {
            deactivate(); throw CinemaRemoteOutcome.invalid
        }
        let changedEpoch = context?.target != value.target || context?.controlEpoch != value.controlEpoch
        if !changedEpoch && (!active || context?.grantID != value.grantID) { deactivate(); throw CinemaRemoteOutcome.invalid }
        if changedEpoch { lastSequence = 0; results.removeAll() }
        if changedEpoch || context?.contextRevision != value.contextRevision || context?.textNonce != value.textNonce { invalidate() }
        context = value
        active = true
    }
    private func observe(_ now: UInt64) throws {
        if let lastNow, now < lastNow { invalidate(); throw CinemaRemoteOutcome.invalid }
        lastNow = now
        credits.removeAll { now >= $0.deadline }
        results.removeAll { now >= $0.deadline }
    }
    func mint(_ kind: CinemaRemoteCreditKind, now: UInt64) throws -> CinemaRemoteCredit {
        try observe(now)
        guard active else { throw CinemaRemoteOutcome.unavailable }
        let (deadline, overflow) = now.addingReportingOverflow(kind.ttlMs)
        guard !overflow else { throw CinemaRemoteOutcome.invalid }
        let credit = CinemaRemoteCredit(nonce: UUID(), kind: kind)
        if credits.count == 16 { credits.removeFirst() }
        credits.append(LocalCredit(wire: credit, deadline: deadline))
        return credit
    }
    /// Consume sequence before the synchronous UI closure. Store ACK afterwards,
    /// including a rejected UI effect; a failed effect is never replayable.
    func apply(_ command: CinemaRemoteCommand, now: UInt64, semantic: CinemaRemoteOutcome?, effect: () -> CinemaRemoteOutcome) -> CinemaRemoteOutcome {
        do {
            try observe(now)
            try command.validate()
            guard active, let current = context else { return .unavailable }
            guard command.target == current.target else { return .staleTarget }
            guard command.grantID == current.grantID else { return .unauthorized }
            guard command.controlEpoch == current.controlEpoch else { return .staleControl }
            guard command.sequence > lastSequence else { return .duplicateOrOld }
            guard let credit = credits.first(where: { $0.wire.nonce == command.credit }) else { return .expired }
            guard credit.wire.kind == command.action.creditKind else { return .invalid }
            guard command.contextRevision == current.contextRevision else { return .staleContext }
            if command.action.type == .select && command.focusRevision != current.focusRevision { return .staleFocus }
            if command.action.type == .textReplace && command.action.textNonce != current.textNonce { return .staleContext }
            if let semantic { return semantic == .applied ? .invalid : semantic }
            let (deadline, overflow) = now.addingReportingOverflow(10_000)
            guard !overflow else { return .invalid }
            lastSequence = command.sequence
            let outcome = effect()
            if results.count == 64 { results.removeFirst() }
            results.append(LocalResult(ack: Acknowledgement(controlEpoch: command.controlEpoch, sequence: command.sequence, outcome: outcome), deadline: deadline))
            return outcome
        } catch let outcome as CinemaRemoteOutcome { return outcome }
        catch { return .invalid }
    }
    func result(controlEpoch: UUID, sequence: UInt64, now: UInt64) -> Acknowledgement? {
        guard (try? observe(now)) != nil else { return nil }
        return results.first { $0.ack.controlEpoch == controlEpoch && $0.ack.sequence == sequence }?.ack
    }
}
struct RemoteMonotonicClock {
    private let clock = ContinuousClock()
    private let origin = ContinuousClock.now
    var milliseconds: UInt64 {
        let elapsed = origin.duration(to: clock.now).components
        guard elapsed.seconds >= 0 else { return 0 }
        let seconds = UInt64(elapsed.seconds)
        let (base, overflow) = seconds.multipliedReportingOverflow(by: 1_000)
        guard !overflow else { return .max }
        let fraction = UInt64(max(0, elapsed.attoseconds / 1_000_000_000_000_000))
        let (value, sumOverflow) = base.addingReportingOverflow(fraction)
        return sumOverflow ? .max : value
    }
}
