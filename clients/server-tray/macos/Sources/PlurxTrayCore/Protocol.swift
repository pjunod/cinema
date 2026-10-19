// Desktop control protocol v1 as the UI consumes it. The Rust source of truth
// is plurx_desktop_control::protocol (ControlResponseV1 / ControlResultV1);
// the frozen wire contract is CONTROL-CONTRACT.md for task t_a421d903.
//
// Decoding is strict on everything the UI acts on: a schema version other
// than 1, an unknown `kind`, an unknown state string, or an `ok` flag that
// disagrees with its result/error is a protocol failure, shown as such, never
// coerced into a plausible state. In particular nothing unknown becomes
// "stopped" — an unknown server must not invite a Start.
import Foundation

public let controlSchemaVersion = 1
/// The controller writes at most this many bytes to stdout per invocation.
public let maximumResponseBytes = 1 << 20

public enum ServiceState: String, Codable, CaseIterable, Sendable {
    case running, starting, stopped, unhealthy, unknown
}

public enum StartupState: String, Codable, CaseIterable, Sendable {
    case enabled, disabled, masked
    case `static` = "static"
    case approvalRequired = "approval_required"
    case policyManaged = "policy_managed"
    case unknown
}

public struct StartupStateV1: Codable, Equatable, Sendable {
    public var state: StartupState
    public var detail: String?
    public init(state: StartupState, detail: String? = nil) {
        self.state = state
        self.detail = detail
    }
}

public enum OperationKind: String, Codable, Sendable {
    case start, stop, restart, upgrade
}

public enum UpdateState: String, Codable, CaseIterable, Sendable {
    case idle, checking, available, prepared
    case waitingForIdle = "waiting_for_idle"
    case shutdownCommitted = "shutdown_committed"
    case installing, restarting, complete, cancelled, failed
    case recoveryRequired = "recovery_required"

    /// Before the daemon has committed to shutting down, the preparation can
    /// still be cancelled; afterwards the transaction only runs forward or
    /// into recovery (DESKTOP-UPGRADE-TRANSACTION.md §6.1).
    public var isCancellable: Bool {
        switch self {
        case .prepared, .waitingForIdle: return true
        default: return false
        }
    }

    /// A transaction in one of these states is still owned by the worker and
    /// must be followed; the UI polls it and does not offer a second upgrade.
    public var isInFlight: Bool {
        switch self {
        case .checking, .prepared, .waitingForIdle, .shutdownCommitted, .installing, .restarting:
            return true
        default:
            return false
        }
    }

    public var isTerminal: Bool {
        switch self {
        case .complete, .cancelled, .failed, .recoveryRequired: return true
        default: return false
        }
    }
}

public enum ErrorCode: String, Codable, CaseIterable, Sendable {
    case invalidInstallation = "invalid_installation"
    case unsupported
    case authorizationRequired = "authorization_required"
    case authorizationDenied = "authorization_denied"
    case busy
    case identityMismatch = "identity_mismatch"
    case unsafeShutdown = "unsafe_shutdown"
    case policyConflict = "policy_conflict"
    case unavailable
    case rateLimited = "rate_limited"
    case untrustedUpdate = "untrusted_update"
    case recoveryRequired = "recovery_required"
    case invalidRequest = "invalid_request"
    case io
}

public struct ControlErrorV1: Codable, Equatable, Sendable {
    public var code: ErrorCode
    public var message: String
    public var retryable: Bool
    public init(code: ErrorCode, message: String, retryable: Bool) {
        self.code = code
        self.message = message
        self.retryable = retryable
    }
}

public struct StatusResult: Equatable, Sendable {
    public var installationId: String
    public var serviceState: ServiceState
    public var version: String?
    public var browserOrigin: String
    public var serverStartup: StartupStateV1
    public var trayStartup: StartupStateV1
    public var transactionId: String?
}

public struct UpdateResult: Equatable, Sendable {
    public var state: UpdateState
    public var version: String?
    public var transactionId: String?
    public var detail: String?
}

public enum ControlResult: Equatable, Sendable {
    case status(StatusResult)
    case operation(OperationKind, ServiceState)
    case startup(server: StartupStateV1, tray: StartupStateV1)
    case update(UpdateResult)
    case logs(lines: [String], truncated: Bool)
}

extension ControlResult: Decodable {
    private enum Keys: String, CodingKey {
        case kind
        case installationId = "installation_id"
        case serviceState = "service_state"
        case version
        case browserOrigin = "browser_origin"
        case serverStartup = "server_startup"
        case trayStartup = "tray_startup"
        case transactionId = "transaction_id"
        case operation, server, tray, state, detail, lines, truncated
    }

    private static let fields: [String: Set<String>] = [
        "status": ["kind", "installation_id", "service_state", "version", "browser_origin",
                   "server_startup", "tray_startup", "transaction_id"],
        "operation": ["kind", "operation", "service_state"],
        "startup": ["kind", "server", "tray"],
        "update": ["kind", "state", "version", "transaction_id", "detail"],
        "logs": ["kind", "lines", "truncated"],
    ]

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: Keys.self)
        let kind = try c.decode(String.self, forKey: .kind)
        guard let allowed = Self.fields[kind] else {
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: c, debugDescription: "unknown result kind \(kind)")
        }
        // The Rust side uses deny_unknown_fields; mirror it so a renamed field
        // is found by the protocol regression, not silently defaulted.
        let any = try decoder.container(keyedBy: AnyKey.self)
        for key in any.allKeys where !allowed.contains(key.stringValue) {
            throw DecodingError.dataCorruptedError(
                forKey: .kind, in: c, debugDescription: "unexpected field \(key.stringValue) in \(kind)")
        }
        switch kind {
        case "status":
            self = .status(StatusResult(
                installationId: try c.decode(String.self, forKey: .installationId),
                serviceState: try c.decode(ServiceState.self, forKey: .serviceState),
                version: try c.decodeNullable(String.self, forKey: .version),
                browserOrigin: try c.decode(String.self, forKey: .browserOrigin),
                serverStartup: try c.decode(StartupStateV1.self, forKey: .serverStartup),
                trayStartup: try c.decode(StartupStateV1.self, forKey: .trayStartup),
                transactionId: try c.decodeNullable(String.self, forKey: .transactionId)))
        case "operation":
            self = .operation(
                try c.decode(OperationKind.self, forKey: .operation),
                try c.decode(ServiceState.self, forKey: .serviceState))
        case "startup":
            self = .startup(
                server: try c.decode(StartupStateV1.self, forKey: .server),
                tray: try c.decode(StartupStateV1.self, forKey: .tray))
        case "update":
            self = .update(UpdateResult(
                state: try c.decode(UpdateState.self, forKey: .state),
                version: try c.decodeNullable(String.self, forKey: .version),
                transactionId: try c.decodeNullable(String.self, forKey: .transactionId),
                detail: try c.decodeNullable(String.self, forKey: .detail)))
        default: // "logs"
            self = .logs(
                lines: try c.decode([String].self, forKey: .lines),
                truncated: try c.decode(Bool.self, forKey: .truncated))
        }
    }
}

/// The contract writes `null` explicitly; a missing nullable field is a
/// different (older or broken) producer and is rejected.
private extension KeyedDecodingContainer {
    func decodeNullable<T: Decodable>(_ type: T.Type, forKey key: Key) throws -> T? {
        guard contains(key) else {
            throw DecodingError.keyNotFound(key, .init(
                codingPath: codingPath, debugDescription: "required nullable field is absent"))
        }
        return try decodeIfPresent(type, forKey: key)
    }
}

struct AnyKey: CodingKey {
    var stringValue: String
    var intValue: Int? { nil }
    init?(stringValue: String) { self.stringValue = stringValue }
    init?(intValue: Int) { return nil }
}

/// A failure the UI can show. Controller-reported errors keep their code;
/// everything else (missing binary, timeout, malformed output) is local.
public enum ControlFailure: Error, Equatable, Sendable {
    case controller(ControlErrorV1)
    case protocolViolation(String)
    case launch(String)
    case timedOut(seconds: Int)
    case outputTooLarge
    case exited(status: Int32, diagnostics: String)

    public var code: ErrorCode? {
        if case let .controller(error) = self { return error.code }
        return nil
    }
}

/// Decodes one complete ControlResponseV1 from the controller's stdout.
public func decodeControlResponse(_ data: Data) -> Result<ControlResult, ControlFailure> {
    guard data.count <= maximumResponseBytes else { return .failure(.outputTooLarge) }
    struct Envelope: Decodable {
        let schema_version: Int
        let ok: Bool
        let result: ControlResult?
        let error: ControlErrorV1?
    }
    do {
        let object = try JSONSerialization.jsonObject(with: data)
        guard let dictionary = object as? [String: Any] else {
            return .failure(.protocolViolation("response is not a JSON object"))
        }
        let expected: Set<String> = ["schema_version", "ok", "result", "error"]
        let keys = Set(dictionary.keys)
        guard keys == expected else {
            return .failure(.protocolViolation(
                "response fields \(keys.sorted()) differ from \(expected.sorted())"))
        }
        let envelope = try JSONDecoder().decode(Envelope.self, from: data)
        guard envelope.schema_version == controlSchemaVersion else {
            return .failure(.protocolViolation(
                "controller speaks schema \(envelope.schema_version), this app speaks \(controlSchemaVersion)"))
        }
        switch (envelope.ok, envelope.result, envelope.error) {
        case let (true, result?, nil): return .success(result)
        case let (false, nil, error?): return .failure(.controller(error))
        default:
            return .failure(.protocolViolation("ok flag disagrees with result/error"))
        }
    } catch {
        return .failure(.protocolViolation("malformed controller response: \(error)"))
    }
}
