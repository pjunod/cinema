// The finite controller command set (CONTROL-CONTRACT.md "CLI boundary").
// Every invocation is an argument array; nothing here is built from text the
// server, a release note or an installation record supplied. Values that do
// reach argv (a version, a transaction id) are validated against the exact
// shapes the controller accepts before they are passed.
import Foundation

public enum ServerStartupChoice: String, Sendable { case on, off }

public enum ControlCommand: Equatable, Sendable {
    case status
    case start
    case stop
    case restart
    case startupGet
    case startupSetServer(ServerStartupChoice)
    case updatesCheck
    case updatesPrepare(version: String)
    case updatesApply(transaction: String)
    case updatesStatus(transaction: String?)
    case updatesCancel(transaction: String)
    case logs(lines: Int)

    /// Default and maximum lines the controller returns for `logs`.
    public static let defaultLogLines = 100
    public static let maximumLogLines = 1000

    public enum Invalid: Error, Equatable { case version(String), transaction(String), lines(Int) }

    /// The arguments after the executable path. Stop, restart and apply carry
    /// `--confirm-interruption` because the UI only issues them after the user
    /// confirmed the named interruption in a dialog.
    public func arguments() throws -> [String] {
        switch self {
        case .status: return ["status"]
        case .start: return ["start"]
        case .stop: return ["stop", "--confirm-interruption"]
        case .restart: return ["restart", "--confirm-interruption"]
        case .startupGet: return ["startup", "get"]
        case let .startupSetServer(choice): return ["startup", "set", "server", choice.rawValue]
        case .updatesCheck: return ["updates", "check"]
        case let .updatesPrepare(version):
            guard Self.isVersion(version) else { throw Invalid.version(version) }
            return ["updates", "prepare", "--version", version]
        case let .updatesApply(transaction):
            guard Self.isTransaction(transaction) else { throw Invalid.transaction(transaction) }
            return ["updates", "apply", "--transaction", transaction, "--confirm-interruption"]
        case let .updatesStatus(transaction):
            guard let transaction else { return ["updates", "status"] }
            guard Self.isTransaction(transaction) else { throw Invalid.transaction(transaction) }
            return ["updates", "status", "--transaction", transaction]
        case let .updatesCancel(transaction):
            guard Self.isTransaction(transaction) else { throw Invalid.transaction(transaction) }
            return ["updates", "cancel", "--transaction", transaction]
        case let .logs(lines):
            guard (1...Self.maximumLogLines).contains(lines) else { throw Invalid.lines(lines) }
            return ["logs", "--lines", String(lines)]
        }
    }

    /// Wall-clock bound for one invocation. Stop/restart wait for the daemon's
    /// committed drain and clean-exit receipt; prepare downloads and verifies
    /// a release. Apply only hands the transaction to the detached worker.
    public var timeoutSeconds: Int {
        switch self {
        case .status, .startupGet, .updatesStatus, .logs: return 15
        case .startupSetServer, .updatesCancel: return 60
        case .start, .updatesCheck, .updatesApply: return 120
        case .stop, .restart: return 300
        case .updatesPrepare: return 900
        }
    }

    /// Mutations are serialized by the UI (one in flight) in addition to the
    /// controller's own installation lock.
    public var isMutation: Bool {
        switch self {
        case .status, .startupGet, .updatesStatus, .logs, .updatesCheck: return false
        default: return true
        }
    }

    /// Strict SemVer 2.0 (no leading `v`), the form a signed manifest names.
    public static func isVersion(_ text: String) -> Bool {
        let pattern = #"^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$"#
        return text.count <= 64 && text.range(of: pattern, options: .regularExpression) != nil
    }

    /// Lower-case hyphenated UUID as the controller emits it.
    public static func isTransaction(_ text: String) -> Bool {
        let pattern = #"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$"#
        return text.range(of: pattern, options: .regularExpression) != nil
    }
}
