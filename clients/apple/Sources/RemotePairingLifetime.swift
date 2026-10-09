import Foundation

/// Closing or selecting another target retires every in-flight phone claim.
struct RemotePairingLifetime {
    struct Ticket {
        let generation: UUID
        let receiverID: UUID
        let target: CinemaRemoteTarget
    }
    private var generation = UUID()
    mutating func begin(receiverID: UUID, target: CinemaRemoteTarget) -> Ticket {
        retire()
        return Ticket(generation: generation, receiverID: receiverID, target: target)
    }
    mutating func retire() { generation = UUID() }
    func accepts(_ ticket: Ticket, receiverID: UUID?, target: CinemaRemoteTarget?) -> Bool {
        ticket.generation == generation && ticket.receiverID == receiverID && ticket.target == target
    }
}

struct RemotePairingDeadline {
    private let start: UInt64
    private let deadline: UInt64?
    init(start: UInt64, budget: UInt64 = 120_000) {
        self.start = start
        let (value, overflow) = start.addingReportingOverflow(budget)
        deadline = !overflow && budget > 0 ? value : nil
    }
    func admits(_ now: UInt64) -> Bool { guard let deadline else { return false }; return now >= start && now < deadline }
    func remaining(_ now: UInt64) -> UInt64 { guard admits(now), let deadline else { return 0 }; return deadline - now }
}
struct RemoteCommandResult {
    private var epoch: UUID?
    private var sequence: UInt64 = 0
    private var outcome: CinemaRemoteOutcome?
    mutating func begin(epoch: UUID, sequence: UInt64) { self.epoch = epoch; self.sequence = sequence; outcome = nil }
    mutating func observe(epoch: UUID, sequence: UInt64, outcome: CinemaRemoteOutcome) -> Bool {
        guard self.epoch == epoch, self.sequence == sequence else { return false }
        self.outcome = outcome; return true
    }
    func known(epoch: UUID, sequence: UInt64) -> CinemaRemoteOutcome? { self.epoch == epoch && self.sequence == sequence ? outcome : nil }
    mutating func retire() { epoch = nil; sequence = 0; outcome = nil }
}

/// Captured owned options cannot authorize a different refreshed media source.
struct RemoteOwnedChoiceSource: Equatable {
    let token: UUID
    let epoch: UUID
    let signature: Data
    func accepts(token: UUID?, epoch: UUID, signature: Data) -> Bool {
        self.token == token && self.epoch == epoch && self.signature == signature
    }
}
