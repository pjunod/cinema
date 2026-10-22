// The menu as data. AppKit renders exactly this; tests assert on it without a
// status bar. Layout follows DESKTOP-TRAY-IMPLEMENTATION.md §4.
import Foundation

/// SMAppService.mainApp status as the OS reports it, mapped without the
/// framework so the model is testable. `.notFound` means the bundle is not
/// registered (for example, run from outside ~/Applications).
public enum TrayLoginStatus: Equatable, Sendable {
    case enabled, notRegistered, requiresApproval, notFound, unavailable(String)
}

public struct LoginItemFailure: Error, Equatable, Sendable {
    public var message: String
    public init(_ message: String) { self.message = message }
}

public enum Action: Equatable, Sendable {
    case openBrowser, viewLogs, start, stop, restart
    case toggleServerStartup, toggleTrayLogin
    case checkForUpdates, openReleaseNotes(version: String), upgrade(version: String), showUpgradeProgress
    case retryStatus, showErrorDetails, quit
}

public enum CheckState: Equatable, Sendable { case on, off, mixed }

public struct MenuEntry: Equatable, Sendable {
    public enum Kind: Equatable, Sendable { case header, info, command, separator }
    public var kind: Kind
    public var title: String
    public var action: Action?
    public var enabled: Bool
    public var check: CheckState?
    public var accessibility: String?

    static func header(_ title: String, accessibility: String) -> MenuEntry {
        MenuEntry(kind: .header, title: title, action: nil, enabled: false, check: nil, accessibility: accessibility)
    }
    static func info(_ title: String) -> MenuEntry {
        MenuEntry(kind: .info, title: title, action: nil, enabled: false, check: nil, accessibility: nil)
    }
    static func command(_ title: String, _ action: Action, enabled: Bool = true, check: CheckState? = nil) -> MenuEntry {
        MenuEntry(kind: .command, title: title, action: action, enabled: enabled, check: check, accessibility: nil)
    }
    static let separator = MenuEntry(kind: .separator, title: "", action: nil, enabled: false, check: nil, accessibility: nil)
}

/// Everything the UI has observed. Nil status means no successful status yet.
public struct TraySnapshot: Equatable, Sendable {
    public var status: StatusResult?
    public var statusFailure: ControlFailure?
    public var busy: Action?
    public var update: UpdateResult?
    public var trayLogin: TrayLoginStatus
    public var lastError: String?
    public var appVersion: String
    public var appBuild: String

    public init(status: StatusResult? = nil, statusFailure: ControlFailure? = nil, busy: Action? = nil,
                update: UpdateResult? = nil, trayLogin: TrayLoginStatus = .notRegistered,
                lastError: String? = nil, appVersion: String = "", appBuild: String = "") {
        self.status = status
        self.statusFailure = statusFailure
        self.busy = busy
        self.update = update
        self.trayLogin = trayLogin
        self.lastError = lastError
        self.appVersion = appVersion
        self.appBuild = appBuild
    }

    /// The state the icon and header show. A failed status call is Unknown,
    /// never Stopped.
    public var serviceState: ServiceState {
        guard statusFailure == nil, let status else { return .unknown }
        return status.serviceState
    }

    /// The transaction the worker currently owns, from the latest update poll
    /// or the status record.
    public var activeTransaction: String? {
        if let update, update.state.isInFlight || update.state == .recoveryRequired {
            return update.transactionId
        }
        return status?.transactionId
    }
}

public func buildMenu(_ s: TraySnapshot) -> [MenuEntry] {
    let state = s.serviceState
    let idle = s.busy == nil
    let upgrading = s.activeTransaction != nil
    var entries: [MenuEntry] = []

    entries.append(.header("plurx — \(state.title)", accessibility: "plurx server \(state.title)"))
    // The server version is what the controller observed for the installation;
    // the tray app's own version/build is separate and can differ mid-upgrade.
    entries.append(.info("Version \(s.status?.version ?? "unknown")"))
    if !s.appVersion.isEmpty {
        let build = s.appBuild.isEmpty || s.appBuild == "unknown" ? "" : " (\(s.appBuild.prefix(12)))"
        entries.append(.info("Tray app \(s.appVersion)\(build)"))
    }
    if let busy = s.busy {
        entries.append(.info(progressTitle(busy)))
    }
    if s.lastError != nil || s.statusFailure != nil {
        entries.append(.command("View error details…", .showErrorDetails))
    }
    if s.status == nil, s.statusFailure != nil {
        entries.append(.command("Retry", .retryStatus, enabled: idle))
    }

    let hasOrigin = s.status.flatMap { browserURL(from: $0.browserOrigin) } != nil
    entries.append(.command("Open plurx…", .openBrowser, enabled: hasOrigin && state == .running))
    entries.append(.command("View logs…", .viewLogs, enabled: s.status != nil))
    entries.append(.separator)

    // Start is offered only for an observed Stopped server: a starting or
    // unknown server is never an invitation to start a second one.
    let lifecycle = idle && !upgrading && s.status != nil
    entries.append(.command("Start", .start, enabled: lifecycle && state == .stopped))
    entries.append(.command("Stop…", .stop, enabled: lifecycle && (state == .running || state == .unhealthy)))
    entries.append(.command("Restart…", .restart, enabled: lifecycle && (state == .running || state == .unhealthy)))

    if let server = s.status?.serverStartup {
        entries.append(.command(startupTitle("Start server at login", server.state),
                                .toggleServerStartup,
                                enabled: idle && (server.state == .enabled || server.state == .disabled),
                                check: checkState(server.state)))
    } else {
        entries.append(.command("Start server at login", .toggleServerStartup, enabled: false, check: .off))
    }
    entries.append(trayLoginEntry(s.trayLogin, idle: idle))
    entries.append(.separator)

    entries.append(contentsOf: updateEntries(s, idle: idle))
    entries.append(.separator)
    entries.append(.command("Quit tray app", .quit))
    return entries
}

func progressTitle(_ action: Action) -> String {
    switch action {
    case .start: return "Starting the server…"
    case .stop: return "Stopping the server (waiting for a clean exit)…"
    case .restart: return "Restarting the server…"
    case .toggleServerStartup: return "Changing login startup…"
    case .checkForUpdates: return "Checking for updates…"
    case .upgrade: return "Preparing the upgrade…"
    default: return "Working…"
    }
}

func startupTitle(_ base: String, _ state: StartupState) -> String {
    guard let qualifier = state.qualifier else { return base }
    return "\(base) (\(qualifier))"
}

/// A check mark is shown only for an observed effective result.
func checkState(_ state: StartupState) -> CheckState {
    switch state {
    case .enabled: return .on
    case .disabled, .masked: return .off
    case .static, .approvalRequired, .policyManaged, .unknown: return .mixed
    }
}

func trayLoginEntry(_ status: TrayLoginStatus, idle: Bool) -> MenuEntry {
    let base = "Show tray app at login"
    switch status {
    case .enabled:
        return .command(base, .toggleTrayLogin, enabled: idle, check: .on)
    case .notRegistered:
        return .command(base, .toggleTrayLogin, enabled: idle, check: .off)
    case .requiresApproval:
        return .command("\(base) (needs approval in System Settings)", .toggleTrayLogin, enabled: idle, check: .mixed)
    case .notFound:
        return .command("\(base) (install in ~/Applications first)", .toggleTrayLogin, enabled: false, check: .off)
    case let .unavailable(reason):
        return .command("\(base) (\(reason))", .toggleTrayLogin, enabled: false, check: .mixed)
    }
}

func updateEntries(_ s: TraySnapshot, idle: Bool) -> [MenuEntry] {
    var entries: [MenuEntry] = []
    if s.activeTransaction != nil {
        let title = s.update.map { "Upgrade: \($0.state.title)" } ?? "Upgrade in progress"
        entries.append(.command("\(title)…", .showUpgradeProgress))
        return entries
    }
    if let update = s.update {
        switch update.state {
        case .available:
            entries.append(.info("Update available: \(update.version ?? "unknown version")"))
        case .complete, .cancelled, .failed:
            entries.append(.command("Last upgrade: \(update.state.title)…", .showUpgradeProgress))
        case .idle:
            entries.append(.info("No update available"))
        default:
            break
        }
    }
    entries.append(.command("Check for updates…", .checkForUpdates, enabled: idle))
    if let update = s.update, update.state == .available, let version = update.version,
       ControlCommand.isVersion(version) {
        entries.append(.command("Release notes for \(version)…", .openReleaseNotes(version: version)))
        entries.append(.command("Upgrade to \(version)…", .upgrade(version: version),
                                enabled: idle && s.status != nil))
    }
    return entries
}

/// DESKTOP-TRAY-IMPLEMENTATION.md §4: five seconds while the menu or a window
/// is visible, thirty otherwise; two seconds while a transition or upgrade is
/// being followed.
public func pollInterval(visible: Bool, snapshot: TraySnapshot) -> TimeInterval {
    if snapshot.activeTransaction != nil || snapshot.serviceState == .starting { return 2 }
    return visible ? 5 : 30
}
