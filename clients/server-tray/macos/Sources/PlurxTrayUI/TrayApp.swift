// The AppKit side of TrayController: the NSStatusItem, its menu, dialogs and
// windows. Everything here runs on the main thread; controller calls are
// awaited from Tasks and run on background queues inside ControlRunner.
import AppKit
import PlurxTrayCore

@MainActor
public final class TrayApp: NSObject, NSApplicationDelegate, NSMenuDelegate, TrayEnvironment {
    public private(set) var statusItem: NSStatusItem?
    public let menu = NSMenu()
    public var smokeDriver: SmokeDriver?
    public private(set) var controller: TrayController!
    private let control: ControlInvoking
    private let info: AppInfo
    private lazy var logs: LogWindowController = {
        let window = LogWindowController()
        window.onRefresh = { [weak self] in self?.dispatch(.viewLogs) }
        return window
    }()
    private lazy var upgradeWindow: UpgradeWindowController = {
        let window = UpgradeWindowController()
        window.onCancel = { [weak self] in
            Task { await self?.controller.cancelUpgrade() }
        }
        return window
    }()
    /// Settable for tests, which must not raise modal alerts.
    public var confirmHandler: ((String, String, String) async -> Bool)?
    public var errorHandler: ((String) -> Void)?

    public init(control: ControlInvoking, info: AppInfo) {
        self.control = control
        self.info = info
        super.init()
        controller = TrayController(control: control, environment: self,
                                    appVersion: info.version, appBuild: info.build)
    }

    public func applicationDidFinishLaunching(_ notification: Notification) {
        install()
        if let smokeDriver { smokeDriver.start(on: self) } else { controller.startPolling() }
    }

    /// Creates the status item and first menu. Separate from launch so tests
    /// can exercise it without an NSApplication run loop.
    public func install() {
        let item = NSStatusBar.system.statusItem(withLength: NSStatusItem.squareLength)
        item.button?.image = StatusIcon.image(for: .unknown)
        item.button?.setAccessibilityTitle("plurx server")
        item.button?.toolTip = "plurx server — Unknown"
        menu.delegate = self
        item.menu = menu
        statusItem = item
        snapshotDidChange(controller.snapshot)
    }

    public func applicationWillTerminate(_ notification: Notification) {
        // Leaves the server and any upgrade worker running by design.
        controller.stopPolling()
        if let smokeDriver {
            smokeDriver.applicationWillTerminate(self)
            // NSApplication.terminate otherwise exits with zero regardless of
            // qualification failure. Preserve the normal Quit delegate path.
            exit(smokeDriver.exitCode)
        }
    }

    // MARK: NSMenuDelegate

    public func menuWillOpen(_ menu: NSMenu) {
        smokeDriver?.menuOpened()
        controller.menuVisible = true
    }
    public func menuDidClose(_ menu: NSMenu) {
        smokeDriver?.menuClosed()
        controller.menuVisible = false
    }

    @objc public func menuAction(_ sender: NSMenuItem) {
        // Qualification opens the real menu for observation only. The explicit
        // driver action is the sole authority; clicks cannot change startup.
        guard smokeDriver == nil else { return }
        guard let box = sender.representedObject as? ActionBox else { return }
        dispatch(box.action)
    }

    private func dispatch(_ action: Action) {
        Task { await controller.perform(action) }
    }

    // MARK: TrayEnvironment

    public func snapshotDidChange(_ snapshot: TraySnapshot) {
        let state = snapshot.serviceState
        statusItem?.button?.image = StatusIcon.image(for: state)
        statusItem?.button?.toolTip = "plurx server — \(state.title)"
        statusItem?.button?.setAccessibilityValue(state.title)
        MenuRenderer.render(buildMenu(snapshot), into: menu, target: self, selector: #selector(menuAction(_:)))
        if let upgrade = upgradeWindowIfLoaded, upgrade.window?.isVisible == true {
            upgrade.update(snapshot.update)
        }
    }

    private var upgradeWindowIfLoaded: UpgradeWindowController? {
        upgradeLoaded ? upgradeWindow : nil
    }
    private var upgradeLoaded = false

    public func confirm(title: String, message: String, proceed: String) async -> Bool {
        if let confirmHandler { return await confirmHandler(title, message, proceed) }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: proceed)
        alert.addButton(withTitle: "Cancel")
        NSApp.activate(ignoringOtherApps: true)
        return alert.runModal() == .alertFirstButtonReturn
    }

    public func open(_ url: URL) { NSWorkspace.shared.open(url) }

    public func show(error: String) {
        if let errorHandler { return errorHandler(error) }
        let alert = NSAlert()
        alert.messageText = "plurx"
        alert.informativeText = error
        alert.alertStyle = .warning
        NSApp.activate(ignoringOtherApps: true)
        alert.runModal()
    }

    public func trayLoginStatus() -> TrayLoginStatus { LoginItem.status() }
    public func setTrayLogin(_ enabled: Bool) -> Result<TrayLoginStatus, LoginItemFailure> { LoginItem.set(enabled) }
    public func openLoginItemsSettings() { LoginItem.openSettings() }

    public func showLogs(lines: [String], truncated: Bool) { logs.show(lines: lines, truncated: truncated) }

    public func showUpgrade(_ update: UpdateResult?) {
        upgradeLoaded = true
        upgradeWindow.present(update)
    }

    /// Release notes open the public GitHub release page for the offered
    /// version on the fixed trusted origin; nothing from the network chooses
    /// the URL.
    public func openReleaseNotes(version: String) {
        guard let url = releaseNotesURL(repository: info.releaseRepository, version: version) else {
            show(error: "Release notes are unavailable: this app has no release repository configured.")
            return
        }
        open(url)
    }

    public func quit() { NSApp.terminate(nil) }
}

/// Values stamped into Info.plist by `build`.
public struct AppInfo: Sendable {
    public var version: String
    public var build: String
    public var releaseRepository: String

    public init(version: String, build: String, releaseRepository: String) {
        self.version = version
        self.build = build
        self.releaseRepository = releaseRepository
    }

    public static func from(_ bundle: Bundle) -> AppInfo {
        let dictionary = bundle.infoDictionary ?? [:]
        return AppInfo(
            version: dictionary["PlurxVersion"] as? String ?? dictionary["CFBundleShortVersionString"] as? String ?? "",
            build: dictionary["PlurxBuild"] as? String ?? "unknown",
            releaseRepository: dictionary["PlurxReleaseRepository"] as? String ?? "")
    }
}

/// https://github.com/OWNER/REPO/releases/tag/vVERSION for a strict owner/repo
/// and SemVer; nil otherwise.
public func releaseNotesURL(repository: String, version: String) -> URL? {
    let parts = repository.split(separator: "/", omittingEmptySubsequences: false)
    let name = #"^[A-Za-z0-9](?:[A-Za-z0-9._-]{0,99})$"#
    guard parts.count == 2, parts.allSatisfy({ String($0).range(of: name, options: .regularExpression) != nil }),
          ControlCommand.isVersion(version)
    else { return nil }
    return URL(string: "https://github.com/\(parts[0])/\(parts[1])/releases/tag/v\(version)")
}
