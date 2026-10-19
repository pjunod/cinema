// User-facing words for observed states and failures. Text and shape carry
// state, never colour alone (DESKTOP-TRAY-IMPLEMENTATION.md §4).
import Foundation

public extension ServiceState {
    var title: String {
        switch self {
        case .running: return "Running"
        case .starting: return "Starting"
        case .stopped: return "Stopped"
        case .unhealthy: return "Unhealthy"
        case .unknown: return "Unknown"
        }
    }
}

public extension UpdateState {
    var title: String {
        switch self {
        case .idle: return "Up to date"
        case .checking: return "Checking for updates"
        case .available: return "Update available"
        case .prepared: return "Downloaded and verified"
        case .waitingForIdle: return "Waiting for playback and recordings to finish"
        case .shutdownCommitted: return "Stopping the server"
        case .installing: return "Installing"
        case .restarting: return "Restarting"
        case .complete: return "Upgrade complete"
        case .cancelled: return "Upgrade cancelled"
        case .failed: return "Upgrade failed"
        case .recoveryRequired: return "Recovery required"
        }
    }
}

public extension StartupState {
    /// Explanation appended to a startup checkbox whose state is not a plain
    /// on/off. Nil for enabled/disabled.
    var qualifier: String? {
        switch self {
        case .enabled, .disabled: return nil
        case .masked: return "disabled outside plurx"
        case .static: return "fixed by the service definition"
        case .approvalRequired: return "needs approval in System Settings"
        case .policyManaged: return "managed by policy"
        case .unknown: return "state unknown"
        }
    }
}

public extension ControlFailure {
    /// One sentence for an alert or the menu's last-error line.
    var message: String {
        switch self {
        case let .controller(error):
            let prefix: String
            switch error.code {
            case .authorizationRequired: prefix = "Authorization is required"
            case .authorizationDenied: prefix = "Authorization was denied"
            case .busy: prefix = "Another plurx operation is in progress"
            case .identityMismatch: prefix = "The running server could not be verified as this installation"
            case .unsafeShutdown: prefix = "The server cannot stop safely right now"
            case .policyConflict: prefix = "A system policy prevents this change"
            case .unavailable: prefix = "The server or update service is unavailable"
            case .rateLimited: prefix = "GitHub is rate-limiting update checks"
            case .untrustedUpdate: prefix = "The release could not be verified"
            case .recoveryRequired: prefix = "An interrupted upgrade needs recovery"
            case .invalidInstallation: prefix = "This plurx installation is not managed by the tray app"
            case .unsupported: prefix = "This operation is not supported for this installation"
            case .invalidRequest: prefix = "The controller rejected the request"
            case .io: prefix = "A file or system operation failed"
            }
            let detail = error.message.trimmingCharacters(in: .whitespacesAndNewlines)
            return detail.isEmpty ? prefix + "." : prefix + ": " + detail
        case let .protocolViolation(text):
            return "The bundled controller returned an unexpected response (\(text))."
        case let .launch(text):
            return "The bundled controller could not run: \(text)."
        case let .timedOut(seconds):
            return "The controller did not answer within \(seconds) seconds; the operation may still be finishing."
        case .outputTooLarge:
            return "The controller response exceeded the 1 MiB limit."
        case let .exited(status, diagnostics):
            let tail = diagnostics.split(separator: "\n").last.map(String.init) ?? ""
            return tail.isEmpty
                ? "The controller exited with status \(status)."
                : "The controller exited with status \(status): \(tail)"
        }
    }
}

/// The browser destination. The controller already maps wildcard binds to
/// loopback; the UI only accepts an absolute http(s) URL with a host and never
/// uses it as a lifecycle-control target.
public func browserURL(from origin: String) -> URL? {
    guard origin.count <= 2048, let components = URLComponents(string: origin),
          let scheme = components.scheme?.lowercased(), scheme == "http" || scheme == "https",
          let host = components.host, !host.isEmpty,
          components.user == nil, components.password == nil
    else { return nil }
    return components.url
}
