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
    struct Reservation {
        fileprivate let id: UUID
        let owner: UUID
        let command: CinemaRemoteCommand
        fileprivate let context: Context
        fileprivate let creditDeadline: UInt64
        fileprivate let resultDeadline: UInt64
    }
    enum ReservationResult { case admitted(Reservation), rejected(CinemaRemoteOutcome) }
    private var reservation: Reservation?
    private var context: Context?
    private var active = false
    private var credits: [LocalCredit] = []
    private var results: [LocalResult] = []
    private var lastSequence: UInt64 = 0
    private var lastNow: UInt64?
    var currentCredits: [CinemaRemoteCredit] { credits.map(\.wire) }
    func invalidate() { credits.removeAll(); reservation = nil }
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
        if context != value { reservation = nil }
        context = value
        active = true
    }
    private func observe(_ now: UInt64) throws {
        if let lastNow, now < lastNow { invalidate(); throw CinemaRemoteOutcome.invalid }
        lastNow = now
        credits.removeAll { now >= $0.deadline }
        results.removeAll { now >= $0.deadline }
        if let reservation, now >= reservation.resultDeadline { self.reservation = nil }
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
    private func admission(_ command: CinemaRemoteCommand, now: UInt64, semantic: CinemaRemoteOutcome?) throws -> LocalCredit {
        try observe(now)
        try command.validate()
        guard active, let current = context else { throw CinemaRemoteOutcome.unavailable }
        guard command.target == current.target else { throw CinemaRemoteOutcome.staleTarget }
        guard command.grantID == current.grantID else { throw CinemaRemoteOutcome.unauthorized }
        guard command.controlEpoch == current.controlEpoch else { throw CinemaRemoteOutcome.staleControl }
        guard command.sequence > lastSequence else { throw CinemaRemoteOutcome.duplicateOrOld }
        guard let credit = credits.first(where: { $0.wire.nonce == command.credit }) else { throw CinemaRemoteOutcome.expired }
        guard credit.wire.kind == command.action.creditKind else { throw CinemaRemoteOutcome.invalid }
        guard command.contextRevision == current.contextRevision else { throw CinemaRemoteOutcome.staleContext }
        if command.action.type == .select && command.focusRevision != current.focusRevision { throw CinemaRemoteOutcome.staleFocus }
        if command.action.type == .textReplace && command.action.textNonce != current.textNonce { throw CinemaRemoteOutcome.staleContext }
        if let semantic { throw semantic == .applied ? CinemaRemoteOutcome.invalid : semantic }
        return credit
    }
    private func store(_ command: CinemaRemoteCommand, outcome: CinemaRemoteOutcome, deadline: UInt64) {
        if results.count == 64 { results.removeFirst() }
        results.append(LocalResult(ack: Acknowledgement(controlEpoch: command.controlEpoch, sequence: command.sequence, outcome: outcome), deadline: deadline))
    }
    /// Consume before synchronous effects, retaining the canonical admission order.
    func apply(_ command: CinemaRemoteCommand, now: UInt64, semantic: CinemaRemoteOutcome?, effect: () -> CinemaRemoteOutcome) -> CinemaRemoteOutcome {
        do {
            _ = try admission(command, now: now, semantic: semantic)
            let (deadline, overflow) = now.addingReportingOverflow(10_000)
            guard !overflow else { return .invalid }
            lastSequence = command.sequence
            let outcome = effect()
            store(command, outcome: outcome, deadline: deadline)
            return outcome
        } catch let outcome as CinemaRemoteOutcome { return outcome }
        catch { return .invalid }
    }
    /// One async operation may own a consumed sequence. Its original local credit
    /// deadline remains the dispatch deadline; waiting for a controller is no renewal.
    func reserve(_ command: CinemaRemoteCommand, owner: UUID, now: UInt64, semantic: CinemaRemoteOutcome?, replacingPending: Bool = false) -> ReservationResult {
        do {
            let credit = try admission(command, now: now, semantic: semantic)
            guard (reservation == nil || replacingPending), let context else { return .rejected(.unavailable) }
            let (deadline, overflow) = now.addingReportingOverflow(10_000)
            guard !overflow else { return .rejected(.invalid) }
            let value = Reservation(id: UUID(), owner: owner, command: command, context: context, creditDeadline: credit.deadline, resultDeadline: deadline)
            lastSequence = command.sequence
            reservation = value
            return .admitted(value)
        } catch let outcome as CinemaRemoteOutcome { return .rejected(outcome) }
        catch { return .rejected(.invalid) }
    }
    /// Call immediately before every new B request/local renderer mutation, after
    /// any queue wait. Necessary cleanup of an already-owned session is separate.
    func permitsDispatch(_ value: Reservation, owner: UUID, now: UInt64) -> Bool {
        guard (try? observe(now)) != nil else { return false }
        return active && reservation?.id == value.id && value.owner == owner && context == value.context && now < value.creditDeadline
    }
    /// After a request was sent, expiry cannot prove rollback. The owner supplies
    /// applied or the existing unavailable/unconfirmed outcome, never replays it.
    func complete(_ value: Reservation, owner: UUID, now: UInt64, outcome: CinemaRemoteOutcome) -> Acknowledgement? {
        guard (try? observe(now)) != nil, active, reservation?.id == value.id,
              value.owner == owner, context == value.context, now < value.resultDeadline else { return nil }
        reservation = nil
        store(value.command, outcome: outcome, deadline: value.resultDeadline)
        return results.last?.ack
    }
    func retire(_ value: Reservation) {
        if reservation?.id == value.id { reservation = nil }
    }
    func isPending(_ command: CinemaRemoteCommand, now: UInt64) -> Bool {
        guard (try? observe(now)) != nil else { return false }
        return reservation?.command == command
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
