import Foundation

/// Retained for the process lifetime across remote sheets and target selection.
/// Unknown epochs require explicit acquisition before sending any command.
struct RemoteControlSequence {
    private struct Key: Hashable { let target: CinemaRemoteTarget; let grant: UUID; let epoch: UUID }
    private var values: [Key: UInt64] = [:]
    private var order: [Key] = []
    func knows(target: CinemaRemoteTarget, grant: UUID, epoch: UUID) -> Bool {
        values[Key(target: target, grant: grant, epoch: epoch)] != nil
    }
    func recoveryEpoch(target: CinemaRemoteTarget, grant: UUID, control: CinemaRemoteControl?) -> UUID? {
        guard let control, control.activeGrantID == grant,
              !knows(target: target, grant: grant, epoch: control.controlEpoch) else { return nil }
        return control.controlEpoch
    }
    mutating func acquired(target: CinemaRemoteTarget, grant: UUID, epoch: UUID) {
        let key = Key(target: target, grant: grant, epoch: epoch)
        guard values[key] == nil else { return }
        if order.count == 32 { values.removeValue(forKey: order.removeFirst()) }
        order.append(key); values[key] = 0
    }
    mutating func next(target: CinemaRemoteTarget, grant: UUID, epoch: UUID) -> UInt64? {
        let key = Key(target: target, grant: grant, epoch: epoch)
        guard let previous = values[key], previous < CinemaRemoteCommand.maximumInteger else { return nil }
        values[key] = previous + 1
        return previous + 1
    }
}

/// State replies describe a lease; only explicit acquisition permits effects.
struct RemoteControlEligibility {
    private var acquiredGeneration: UUID?
    mutating func acquired(_ generation: UUID) { acquiredGeneration = generation }
    mutating func retire() { acquiredGeneration = nil }
    func permits(_ generation: UUID) -> Bool { acquiredGeneration == generation }
}
