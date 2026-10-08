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
