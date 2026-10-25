// Qualification runs the real NSApplication and AppKit objects. Its only
// mutations are an explicitly requested Stop/Restart through TrayController;
// it never changes server or login-item startup policy.
import AppKit
import CoreGraphics
import Darwin
import PlurxTrayCore

@MainActor
public final class SmokeDriver {
    public let options: SmokeOptions
    private let control: SmokeControl
    private let controllerPath: String
    private let info: AppInfo
    private let started = ContinuousClock.now
    private var deadline: Timer?
    private var finishing = false
    private var quitCallCount: Int?
    private var actionOK = false
    private var aliveAfter = false
    private var failure: String?
    private var unknownSafe = true
    private var startSafe = true
    private var opened = 0
    private var closed = 0
    public private(set) var exitCode: Int32 = 1

    public init(options: SmokeOptions, control: SmokeControl, controllerPath: String, info: AppInfo) {
        self.options = options
        self.control = control
        self.controllerPath = controllerPath
        self.info = info
    }

    public func start(on app: TrayApp) {
        app.confirmHandler = { [weak self] title, _, proceed in
            guard let self, !self.finishing, !self.expired else { return false }
            let options = self.options
            // The explicit action argument is confirmation for a disposable
            // qualification install. It cannot confirm any other operation.
            return (options.action == .stop && title == "Stop the plurx server?" && proceed == "Stop") ||
                (options.action == .restart && title == "Restart the plurx server?" && proceed == "Restart")
        }
        app.errorHandler = { [weak self] error in self?.failure = String(error.prefix(4096)) }
        let timer = Timer(timeInterval: Double(options.timeoutSeconds), repeats: false) { [weak self, weak app] _ in
            MainActor.assumeIsolated {
                guard let self, let app, !self.finishing else { return }
                self.failure = "smoke deadline exceeded"
                self.finish(on: app)
                app.menu.cancelTrackingWithoutAnimation()
            }
        }
        deadline = timer
        RunLoop.main.add(timer, forMode: .common)
        RunLoop.main.add(timer, forMode: .eventTracking)
        Task { await exercise(on: app) }
    }

    public func menuOpened() { opened += 1 }
    public func menuClosed() { closed += 1 }

    private func observeInvariants(_ app: TrayApp) {
        let snapshot = app.controller.snapshot
        let unknown = snapshot.status == nil || snapshot.statusFailure != nil || snapshot.status?.serviceState == .unknown
        let renderedUnknown = app.menu.items.first?.title == "plurx — Unknown" &&
            app.statusItem?.button?.image?.accessibilityDescription == "plurx server Unknown"
        unknownSafe = unknownSafe && (!unknown || (snapshot.serviceState == .unknown && renderedUnknown))
        let enabledStart = app.menu.items.contains {
            ($0.representedObject as? ActionBox)?.action == .start && $0.isEnabled
        }
        startSafe = startSafe && (!enabledStart || snapshot.serviceState == .stopped)
    }

    private func exercise(on app: TrayApp) async {
        await app.controller.refresh()
        // Allow independent host process observation and actual AppKit layout.
        try? await Task.sleep(nanoseconds: 300_000_000)
        guard !finishing else { return }
        observeInvariants(app)
        guard Self.session()["gui_session"] as? Bool == true,
              let button = app.statusItem?.button, button.window != nil else {
            failure = "no owned console GUI session or visible status item"
            finish(on: app)
            return
        }
        // A real popup enters AppKit menu tracking, invokes the real delegate,
        // then closes by a timer. No synthetic delegate calls count as proof.
        let dismiss = Timer(timeInterval: 0.25, repeats: false) { [weak app] _ in
            MainActor.assumeIsolated { app?.menu.cancelTrackingWithoutAnimation() }
        }
        RunLoop.main.add(dismiss, forMode: .eventTracking)
        app.menu.popUp(positioning: nil, at: NSPoint(x: 0, y: button.bounds.maxY), in: button)
        dismiss.invalidate()
        guard !finishing else { return }
        guard opened > 0, closed > 0 else {
            failure = "AppKit did not observe a complete menu open/close"
            finish(on: app)
            return
        }
        guard !expired else {
            failure = "smoke deadline exceeded"
            finish(on: app)
            return
        }
        guard case .success(.status)? = control.firstStatus else {
            failure = "initial controller status failed; no lifecycle action authorized"
            finish(on: app)
            return
        }
        actionOK = await performSmokeAction(options.action, controller: app.controller, control: control)
        guard !finishing else { return }
        if !actionOK, failure == nil { failure = "requested action is disabled or failed" }
        if actionOK && options.action != .quit {
            // A read started by menuWillOpen can still be in flight. It cannot
            // stand in for the required fresh status after Stop/Restart.
            while app.controller.isRefreshing && !finishing {
                try? await Task.sleep(nanoseconds: 10_000_000)
            }
            guard !finishing else { return }
            await app.controller.refresh()
        }
        let expected: ServiceState = options.action == .stop ? .stopped : .running
        // Restart may report Starting while launchd starts the new process.
        // Follow status through the same controller until the requested state.
        while actionOK && options.action != .quit && app.controller.snapshot.serviceState != expected && !finishing {
            try? await Task.sleep(nanoseconds: 250_000_000)
            guard !finishing else { return }
            await app.controller.refresh()
            observeInvariants(app)
        }
        finish(on: app)
    }

    private var expired: Bool { started.duration(to: .now) >= .seconds(options.timeoutSeconds) }

    private func finish(on app: TrayApp) {
        guard !finishing else { return }
        finishing = true
        deadline?.invalidate()
        observeInvariants(app)
        aliveAfter = NSApp.isRunning && app.statusItem?.button?.window != nil
        Task {
            self.quitCallCount = self.control.commands.count
            await app.controller.perform(.quit)
        }
    }

    /// Called by the real applicationWillTerminate, after normal polling has
    /// stopped. Captures the entire Quit path, then durably publishes once.
    public func applicationWillTerminate(_ app: TrayApp) {
        deadline?.invalidate()
        let commands = control.commands
        let duringQuit = quitCallCount.map { commands.count - $0 } ?? commands.count
        let mutations = commands.filter(\.isMutation)
        let permitted: [ControlCommand] = options.action == .quit ? [] : [options.action == .stop ? .stop : .restart]
        let commandsSafe = mutations == permitted && commands.allSatisfy { $0 == .status || permitted.contains($0) }
        let session = Self.session()
        let itemPresent = app.statusItem?.isVisible == true && app.statusItem?.button?.window != nil
        let template = app.statusItem?.button?.image?.isTemplate == true
        let first = Self.firstStatus(control.firstStatus)
        let menuItems = app.menu.items.filter { !$0.isSeparatorItem }
        let titles = menuItems.prefix(32).map { String($0.title.prefix(512)) }
        let enabled = menuItems.filter(\.isEnabled).prefix(32).map { String($0.title.prefix(512)) }
        let truncated = menuItems.count > 32 || menuItems.contains { $0.title.count > 512 }
        let successful = !truncated && finishing && failure == nil && actionOK && aliveAfter && duringQuit == 0 && commandsSafe &&
            unknownSafe && startSafe && opened > 0 && closed > 0 && itemPresent && template &&
            session["gui_session"] as? Bool == true && first["ok"] as? Bool == true
        let elapsed = started.duration(to: .now).components
        var receipt: [String: Any] = [
            "schema": "plurx-tray-smoke-v1", "platform": "macos",
            "app_version": String(info.version.prefix(128)), "app_build": String(info.build.prefix(128)), "arch": Self.architecture,
            "ui_pid": getpid(), "uid": getuid(), "session": session,
            "tray": ["status_item_present": itemPresent, "button_image_template": template,
                     "accessibility_title": String((app.statusItem?.button?.accessibilityTitle() ?? "").prefix(256))],
            "window": NSNull(),
            "menu": ["titles": titles, "enabled": enabled, "observations_truncated": truncated,
                     "open_count": opened, "close_count": closed],
            "controller": ["path": controllerPath, "first_status": first],
            "action": ["name": options.action.rawValue, "ok": actionOK && failure == nil,
                       "service_state_after": app.controller.snapshot.serviceState.rawValue,
                       "error": failure as Any? ?? NSNull(), "ui_alive_after": aliveAfter],
            "quit": ["via": quitCallCount == nil ? "external_termination" : "quit_action", "controller_calls_during_quit": duringQuit],
            "invariants": ["unknown_not_stopped": unknownSafe, "start_only_when_stopped": startSafe,
                           "quit_sent_no_server_command": duringQuit == 0,
                           "ui_alive_after_stop": options.action == .stop ? aliveAfter as Any : NSNull()],
            "elapsed_ms": elapsed.seconds * 1000 + elapsed.attoseconds / 1_000_000_000_000_000,
        ]
        if !successful { receipt["incomplete"] = failure ?? "one or more native smoke invariants failed" }
        do {
            try SmokeReceipt.write(receipt, to: options.receipt)
            exitCode = successful ? 0 : 1
        } catch {
            fputs("smoke receipt failed: \(error)\n", stderr)
            exitCode = 1
        }
    }

    static var architecture: String {
        #if arch(arm64)
        return "arm64"
        #else
        return "x86_64"
        #endif
    }

    static func session() -> [String: Any] {
        let session = CGSessionCopyCurrentDictionary() as? [String: Any]
        let uid = session?["kCGSSessionUserIDKey"] as? NSNumber
        let onConsole = session?["kCGSSessionOnConsoleKey"] as? Bool ?? false
        let loginDone = session?["kCGSessionLoginDoneKey"] as? Bool ?? false
        return ["gui_session": onConsole && loginDone && uid?.uint32Value == getuid(),
                "console_user_uid": uid as Any? ?? NSNull(),
                "session_id": session?["kCGSSessionIDKey"] ?? NSNull(),
                "on_console": onConsole, "login_done": loginDone]
    }

    static func firstStatus(_ result: Result<ControlResult, ControlFailure>?) -> [String: Any] {
        switch result {
        case let .success(.status(status)):
            return ["ok": true, "service_state": status.serviceState.rawValue,
                    "version": status.version.map { String($0.prefix(128)) } as Any? ?? NSNull(), "error_code": NSNull(), "failure": NSNull()]
        case let .failure(error):
            return ["ok": false, "service_state": NSNull(), "version": NSNull(),
                    "error_code": error.code?.rawValue as Any? ?? NSNull(), "failure": String(error.message.prefix(4096))]
        default:
            return ["ok": false, "service_state": NSNull(), "version": NSNull(),
                    "error_code": NSNull(), "failure": "no complete initial status response"]
        }
    }
}
