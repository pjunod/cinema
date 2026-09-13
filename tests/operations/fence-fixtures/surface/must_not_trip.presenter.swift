// The presenter as it is supposed to look: a pure reducer over faults,
// evidence and identities, plus log entries. It NAMES the verbs it refuses to
// perform, in comments and in strings, and the fence must not confuse a name
// with a call — otherwise the rule bans its own documentation.
import Foundation

// The presenter never calls player.pause(), player.play(), player.seek(to:) or
// prepare(); it never creates a Timer.scheduledTimer and never reports to the
// control plane. Those belong to the recovery owner (§3.0).
struct Presenter {
    enum Retirement: String {
        case timer
        case presenting
    }

    /// `player_stopped` is a FACT the owner asserts, not an instruction.
    struct Fault {
        var playerStopped: Bool
        var actions: [String]
    }

    func reduce(_ faults: [Fault], presenting: Bool, nowMs: Int) -> String {
        let blocking = faults.filter { $0.playerStopped }
        if blocking.isEmpty { return presenting ? "none" : "indicator" }
        return "blocking"
    }

    func retires(_ reason: Retirement) -> Bool { reason == .timer }
}
