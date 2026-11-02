// Main-actor coordination between the menu, the controller and the OS. All
// controller work is awaited off the main thread (ControlRunner); this type
// only holds the observed snapshot, serializes mutations and decides what a
// user action does. Platform effects (dialogs, the browser, the login item)
// are behind TrayEnvironment so the decisions are unit-tested.
import Foundation

@MainActor
public protocol TrayEnvironment: AnyObject {
    /// Ask the user to confirm an interruption. Returns true to proceed.
    func confirm(title: String, message: String, proceed: String) async -> Bool
    func open(_ url: URL)
    func show(error: String)
    func trayLoginStatus() -> TrayLoginStatus
    /// Register or unregister the app's own login item; returns the status the
    /// OS reports afterwards.
    func setTrayLogin(_ enabled: Bool) -> Result<TrayLoginStatus, LoginItemFailure>
    func openLoginItemsSettings()
    func snapshotDidChange(_ snapshot: TraySnapshot)
    func showLogs(lines: [String], truncated: Bool)
    func showUpgrade(_ update: UpdateResult?)
    func openReleaseNotes(version: String)
    func quit()
}

@MainActor
public final class TrayController {
    public private(set) var snapshot: TraySnapshot
    private let control: ControlInvoking
    private weak var environment: TrayEnvironment?
    private var polling = false
    /// Read-only observation for qualification: wait out a coalesced menu read
    /// before requesting status after the lifecycle operation has completed.
    public var isRefreshing: Bool { polling }
    private var pollTask: Task<Void, Never>?
    public var menuVisible = false {
        didSet { if menuVisible && !oldValue { refreshSoon() } }
    }

    public init(control: ControlInvoking, environment: TrayEnvironment,
                appVersion: String, appBuild: String) {
        self.control = control
        self.environment = environment
        snapshot = TraySnapshot(trayLogin: environment.trayLoginStatus(),
                                appVersion: appVersion, appBuild: appBuild)
    }

    private func publish() { environment?.snapshotDidChange(snapshot) }

    // MARK: polling

    /// Starts the poll loop. It never overlaps itself: each cycle awaits the
    /// previous status call before sleeping.
    public func startPolling() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refresh()
                let interval = pollInterval(visible: self.menuVisible, snapshot: self.snapshot)
                try? await Task.sleep(nanoseconds: UInt64(interval * 1_000_000_000))
            }
        }
    }

    public func stopPolling() {
        pollTask?.cancel()
        pollTask = nil
    }

    private func refreshSoon() {
        Task { [weak self] in await self?.refresh() }
    }

    /// One status poll (plus the transaction, if one is being followed).
    /// Concurrent callers coalesce into the poll already running.
    public func refresh() async {
        guard !polling else { return }
        polling = true
        defer { polling = false }
        switch await control.run(.status) {
        case let .success(.status(status)):
            snapshot.status = status
            snapshot.statusFailure = nil
        case let .success(other):
            snapshot.statusFailure = .protocolViolation("status returned \(other)")
        case let .failure(failure):
            snapshot.statusFailure = failure
        }
        snapshot.trayLogin = environment?.trayLoginStatus() ?? snapshot.trayLogin
        if let transaction = snapshot.activeTransaction {
            if case let .success(.update(update)) = await control.run(.updatesStatus(transaction: transaction)) {
                snapshot.update = update
                if update.state == .failed || update.state == .recoveryRequired { environment?.showUpgrade(update) }
            }
        }
        publish()
    }

    // MARK: actions

    public func perform(_ action: Action) async {
        switch action {
        case .openBrowser:
            guard let origin = snapshot.status?.browserOrigin, let url = browserURL(from: origin) else {
                environment?.show(error: "This installation has no usable browser address.")
                return
            }
            environment?.open(url)
        case .viewLogs:
            await viewLogs()
        case .start:
            await mutate(action, .start, expecting: .start)
        case .stop:
            guard await confirm(
                "Stop the plurx server?",
                "Playback and recordings on this server end once they can stop safely. The tray icon stays available to start it again.",
                "Stop") else { return }
            await mutate(action, .stop, expecting: .stop)
        case .restart:
            guard await confirm(
                "Restart the plurx server?",
                "Playback and recordings are interrupted while the server restarts. Login and startup settings are unchanged.",
                "Restart") else { return }
            await mutate(action, .restart, expecting: .restart)
        case .toggleServerStartup:
            await toggleServerStartup()
        case .toggleTrayLogin:
            toggleTrayLogin()
        case .checkForUpdates:
            await checkForUpdates()
        case let .openReleaseNotes(version):
            environment?.openReleaseNotes(version: version)
        case let .upgrade(version):
            await upgrade(to: version)
        case .showUpgradeProgress:
            environment?.showUpgrade(snapshot.update)
        case .showErrorDetails:
            if let error = snapshot.lastError ?? snapshot.statusFailure?.message {
                environment?.show(error: error)
            }
        case .retryStatus:
            await refresh()
        case .quit:
            // Quitting the companion never touches the server.
            stopPolling()
            environment?.quit()
        }
    }

    private func confirm(_ title: String, _ message: String, _ proceed: String) async -> Bool {
        await environment?.confirm(title: title, message: message, proceed: proceed) ?? false
    }

    /// Runs one serialized mutation. Busy state is visible in the menu while it
    /// runs; the result or failure is shown, then status is re-read: the
    /// operation's own reported state is not taken as the final word.
    private func mutate(_ action: Action, _ command: ControlCommand, expecting operation: OperationKind) async {
        guard snapshot.busy == nil else { return }
        snapshot.busy = action
        snapshot.lastError = nil
        publish()
        let result = await control.run(command)
        snapshot.busy = nil
        switch result {
        case let .success(.operation(kind, state)) where kind == operation:
            snapshot.status?.serviceState = state
        case let .success(other):
            report(.protocolViolation("\(operation.rawValue) returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
        publish()
        await refresh()
    }

    private func report(_ failure: ControlFailure) {
        snapshot.lastError = failure.message
        environment?.show(error: failure.message)
    }

    private func viewLogs() async {
        switch await control.run(.logs(lines: ControlCommand.maximumLogLines)) {
        case let .success(.logs(lines, truncated)):
            environment?.showLogs(lines: lines, truncated: truncated)
        case let .success(other):
            report(.protocolViolation("logs returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
    }

    /// The checkbox follows the controller's observed result; on any failure
    /// (including a cancelled authorization) it keeps showing the previous
    /// observed state.
    private func toggleServerStartup() async {
        guard snapshot.busy == nil, let current = snapshot.status?.serverStartup.state else { return }
        let choice: ServerStartupChoice
        switch current {
        case .enabled: choice = .off
        case .disabled: choice = .on
        default: return
        }
        snapshot.busy = .toggleServerStartup
        publish()
        let result = await control.run(.startupSetServer(choice))
        snapshot.busy = nil
        switch result {
        case let .success(.startup(server, _)):
            snapshot.status?.serverStartup = server
        case let .success(other):
            report(.protocolViolation("startup set returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
        publish()
        await refresh()
    }

    /// SMAppService is the authority for the tray's own login item. The app
    /// changes it only on this explicit action, never at launch.
    private func toggleTrayLogin() {
        guard let environment else { return }
        let current = environment.trayLoginStatus()
        switch current {
        case .requiresApproval:
            environment.openLoginItemsSettings()
            snapshot.trayLogin = environment.trayLoginStatus()
        case .enabled, .notRegistered:
            switch environment.setTrayLogin(current != .enabled) {
            case let .success(status):
                snapshot.trayLogin = status
                if status == .requiresApproval { environment.openLoginItemsSettings() }
            case let .failure(failure):
                snapshot.trayLogin = environment.trayLoginStatus()
                snapshot.lastError = failure.message
                environment.show(error: failure.message)
            }
        case .notFound, .unavailable:
            return
        }
        publish()
    }

    private func checkForUpdates() async {
        guard snapshot.busy == nil else { return }
        snapshot.busy = .checkForUpdates
        snapshot.lastError = nil
        publish()
        let result = await control.run(.updatesCheck)
        snapshot.busy = nil
        switch result {
        case let .success(.update(update)):
            snapshot.update = update
        case let .success(other):
            report(.protocolViolation("updates check returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
        publish()
    }

    /// Prepare (download + verify) first, then name the version and the
    /// interruption before applying. Apply hands the transaction to the
    /// controller's detached worker, so quitting or replacing this app after
    /// that point does not stop the upgrade.
    private func upgrade(to version: String) async {
        guard snapshot.busy == nil, ControlCommand.isVersion(version) else { return }
        snapshot.busy = .upgrade(version: version)
        snapshot.lastError = nil
        publish()
        let prepared = await control.run(.updatesPrepare(version: version))
        snapshot.busy = nil
        guard case let .success(.update(update)) = prepared else {
            if case let .failure(failure) = prepared { report(failure) } else {
                report(.protocolViolation("updates prepare returned an unexpected result"))
            }
            publish()
            return
        }
        snapshot.update = update
        publish()
        guard update.state == .prepared, let transaction = update.transactionId,
              update.version == version else {
            report(.protocolViolation("prepare did not produce a verified \(version) transaction"))
            return
        }
        let current = snapshot.status?.version ?? "the installed version"
        guard await confirm(
            "Upgrade plurx to \(version)?",
            "plurx \(version) was downloaded and its signature verified. Upgrading from \(current) stops the server once playback and recordings can stop safely, installs \(version) and starts it again. If installation or readiness checks fail, plurx reports Recovery required and keeps the transaction evidence. Recovery may offer a compatible previous version; database or Raft data is never restored automatically.",
            "Upgrade to \(version)") else {
            _ = await control.run(.updatesCancel(transaction: transaction))
            await refresh()
            return
        }
        switch await control.run(.updatesApply(transaction: transaction)) {
        case let .success(.update(applied)):
            snapshot.update = applied
        case let .success(other):
            report(.protocolViolation("updates apply returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
        publish()
        if let update = snapshot.update,
           update.state.isInFlight || update.state == .failed || update.state == .recoveryRequired {
            environment?.showUpgrade(update)
        }
        await refresh()
    }

    /// Cancel from the upgrade window; only offered before shutdown commitment.
    public func cancelUpgrade() async {
        guard let update = snapshot.update, update.state.isCancellable,
              let transaction = update.transactionId else { return }
        switch await control.run(.updatesCancel(transaction: transaction)) {
        case let .success(.update(cancelled)):
            snapshot.update = cancelled
        case let .success(other):
            report(.protocolViolation("updates cancel returned \(other)"))
        case let .failure(failure):
            report(failure)
        }
        publish()
        if let update = snapshot.update,
           update.state.isInFlight || update.state == .failed || update.state == .recoveryRequired {
            environment?.showUpgrade(update)
        }
    }
}

/// Case-insensitive substring filter for the log viewer.
public func filterLogLines(_ lines: [String], query: String) -> [String] {
    let needle = query.trimmingCharacters(in: .whitespaces)
    guard !needle.isEmpty else { return lines }
    return lines.filter { $0.range(of: needle, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
}
