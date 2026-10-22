// Behaviour of the menu model and TrayController against a scripted
// controller and a recording environment. No real controller, login item or
// server is touched.
import XCTest
@testable import PlurxTrayCore

final class FakeControl: ControlInvoking, @unchecked Sendable {
    private let lock = NSLock()
    private var scripted: [String: [Result<ControlResult, ControlFailure>]] = [:]
    private(set) var calls: [ControlCommand] = []

    func on(_ command: ControlCommand, _ results: Result<ControlResult, ControlFailure>...) {
        lock.withLock { scripted[key(command), default: []] += results }
    }

    private func key(_ command: ControlCommand) -> String {
        (try? command.arguments().joined(separator: " ")) ?? "invalid"
    }

    func run(_ command: ControlCommand) async -> Result<ControlResult, ControlFailure> {
        lock.withLock {
            calls.append(command)
            let k = key(command)
            guard var queue = scripted[k], !queue.isEmpty else {
                return .failure(.launch("unscripted \(k)"))
            }
            let next = queue.removeFirst()
            // The last scripted answer repeats, like a stable server.
            scripted[k] = queue.isEmpty ? [next] : queue
            return next
        }
    }

    var commands: [ControlCommand] { lock.withLock { calls } }
}

@MainActor
final class RecordingEnvironment: TrayEnvironment {
    var confirmAnswer = true
    var confirmations: [String] = []
    var opened: [URL] = []
    var errors: [String] = []
    var login: TrayLoginStatus = .notRegistered
    var loginSetResult: Result<TrayLoginStatus, LoginItemFailure>?
    var loginSets: [Bool] = []
    var settingsOpened = 0
    var snapshots: [TraySnapshot] = []
    var logs: [[String]] = []
    var upgrades: [UpdateResult?] = []
    var notes: [String] = []
    var quitCount = 0

    func confirm(title: String, message: String, proceed: String) async -> Bool {
        confirmations.append(title + "|" + message + "|" + proceed)
        return confirmAnswer
    }
    func open(_ url: URL) { opened.append(url) }
    func show(error: String) { errors.append(error) }
    func trayLoginStatus() -> TrayLoginStatus { login }
    func setTrayLogin(_ enabled: Bool) -> Result<TrayLoginStatus, LoginItemFailure> {
        loginSets.append(enabled)
        let result = loginSetResult ?? .success(enabled ? .enabled : .notRegistered)
        if case let .success(status) = result { login = status }
        return result
    }
    func openLoginItemsSettings() { settingsOpened += 1 }
    func snapshotDidChange(_ snapshot: TraySnapshot) { snapshots.append(snapshot) }
    func showLogs(lines: [String], truncated: Bool) { logs.append(lines) }
    func showUpgrade(_ update: UpdateResult?) { upgrades.append(update) }
    func openReleaseNotes(version: String) { notes.append(version) }
    func quit() { quitCount += 1 }
}

func status(_ state: ServiceState, server: StartupState = .enabled, version: String? = "0.7.0",
            transaction: String? = nil) -> ControlResult {
    .status(StatusResult(installationId: "00000000-0000-4000-8000-000000000001", serviceState: state,
                         version: version, browserOrigin: "http://127.0.0.1:32400",
                         serverStartup: StartupStateV1(state: server), trayStartup: StartupStateV1(state: .unknown),
                         transactionId: transaction))
}

let txn = "00000000-0000-4000-8000-000000000002"

func entry(_ menu: [MenuEntry], _ action: Action) -> MenuEntry? { menu.first { $0.action == action } }

final class MenuModelTests: XCTestCase {
    func snapshot(_ state: ServiceState, server: StartupState = .enabled) -> TraySnapshot {
        guard case let .status(s) = status(state, server: server) else { fatalError() }
        return TraySnapshot(status: s, appVersion: "0.7.0", appBuild: "abc123")
    }

    func testHeaderNamesEachObservedStateInText() {
        for state in ServiceState.allCases {
            let menu = buildMenu(snapshot(state))
            XCTAssertEqual(menu[0].title, "plurx — \(state.title)")
            XCTAssertEqual(menu[0].accessibility, "plurx server \(state.title)")
        }
        XCTAssertTrue(buildMenu(snapshot(.running)).contains { $0.title == "Version 0.7.0" })
    }

    func testStartOnlyForObservedStoppedServer() {
        for state in ServiceState.allCases {
            let menu = buildMenu(snapshot(state))
            XCTAssertEqual(entry(menu, .start)?.enabled, state == .stopped, "start for \(state)")
            let canStop = state == .running || state == .unhealthy
            XCTAssertEqual(entry(menu, .stop)?.enabled, canStop, "stop for \(state)")
            XCTAssertEqual(entry(menu, .restart)?.enabled, canStop, "restart for \(state)")
            XCTAssertEqual(entry(menu, .openBrowser)?.enabled, state == .running)
        }
    }

    func testFailedStatusIsUnknownNeverStopped() {
        var s = snapshot(.stopped)
        s.statusFailure = .timedOut(seconds: 15)
        XCTAssertEqual(s.serviceState, .unknown)
        XCTAssertEqual(entry(buildMenu(s), .start)?.enabled, false)
        let fresh = TraySnapshot(statusFailure: .launch("missing"))
        let menu = buildMenu(fresh)
        XCTAssertEqual(menu[0].title, "plurx — Unknown")
        XCTAssertEqual(entry(menu, .retryStatus)?.enabled, true)
        XCTAssertEqual(entry(menu, .start)?.enabled, false)
        XCTAssertEqual(entry(menu, .viewLogs)?.enabled, false)
    }

    func testStartupCheckMarksOnlyForObservedState() {
        XCTAssertEqual(entry(buildMenu(snapshot(.running, server: .enabled)), .toggleServerStartup)?.check, .on)
        XCTAssertEqual(entry(buildMenu(snapshot(.running, server: .disabled)), .toggleServerStartup)?.check, .off)
        for odd in [StartupState.masked, .static, .approvalRequired, .policyManaged, .unknown] {
            let item = entry(buildMenu(snapshot(.running, server: odd)), .toggleServerStartup)
            XCTAssertEqual(item?.enabled, false, "\(odd) is explanatory, not a boolean to overwrite")
            XCTAssertTrue(item?.title.contains(odd.qualifier ?? "?") ?? false)
        }
    }

    func testTrayLoginStatesAreDistinct() {
        var s = snapshot(.running)
        s.trayLogin = .requiresApproval
        let approval = entry(buildMenu(s), .toggleTrayLogin)
        XCTAssertEqual(approval?.check, .mixed)
        XCTAssertTrue(approval?.title.contains("approval") ?? false)
        s.trayLogin = .notFound
        XCTAssertEqual(entry(buildMenu(s), .toggleTrayLogin)?.enabled, false)
        s.trayLogin = .enabled
        XCTAssertEqual(entry(buildMenu(s), .toggleTrayLogin)?.check, .on)
    }

    func testBusyDisablesMutationsAndShowsProgress() {
        var s = snapshot(.running)
        s.busy = .stop
        let menu = buildMenu(s)
        XCTAssertTrue(menu.contains { $0.title.hasPrefix("Stopping the server") })
        XCTAssertEqual(entry(menu, .stop)?.enabled, false)
        XCTAssertEqual(entry(menu, .checkForUpdates)?.enabled, false)
        XCTAssertEqual(entry(menu, .quit)?.enabled, true, "Quit is always available")
    }

    func testUpgradeOfferedOnlyForAvailableNamedVersion() {
        var s = snapshot(.running)
        s.update = UpdateResult(state: .available, version: "0.7.1", transactionId: nil, detail: nil)
        let menu = buildMenu(s)
        XCTAssertEqual(entry(menu, .upgrade(version: "0.7.1"))?.title, "Upgrade to 0.7.1…")
        XCTAssertNotNil(entry(menu, .openReleaseNotes(version: "0.7.1")))
        s.update = UpdateResult(state: .available, version: "not semver", transactionId: nil, detail: nil)
        XCTAssertFalse(buildMenu(s).contains { if case .upgrade = $0.action { return true }; return false })
    }

    func testInFlightTransactionReplacesUpgradeAndDisablesLifecycle() {
        var s = snapshot(.running)
        s.update = UpdateResult(state: .installing, version: "0.7.1", transactionId: txn, detail: nil)
        let menu = buildMenu(s)
        XCTAssertEqual(entry(menu, .showUpgradeProgress)?.title, "Upgrade: Installing…")
        XCTAssertNil(entry(menu, .checkForUpdates))
        XCTAssertEqual(entry(menu, .stop)?.enabled, false)
        XCTAssertEqual(pollInterval(visible: false, snapshot: s), 2)
    }

    func testPollIntervals() {
        XCTAssertEqual(pollInterval(visible: true, snapshot: snapshot(.running)), 5)
        XCTAssertEqual(pollInterval(visible: false, snapshot: snapshot(.running)), 30)
        XCTAssertEqual(pollInterval(visible: false, snapshot: snapshot(.starting)), 2)
    }

    func testLogFilter() {
        XCTAssertEqual(filterLogLines(["INFO start", "WARN Disk", "info done"], query: " info "), ["INFO start", "info done"])
        XCTAssertEqual(filterLogLines(["a", "b"], query: ""), ["a", "b"])
    }
}

@MainActor
final class TrayControllerTests: XCTestCase {
    var control: FakeControl!
    var environment: RecordingEnvironment!
    var controller: TrayController!

    override func setUp() async throws {
        control = FakeControl()
        environment = RecordingEnvironment()
        controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "b")
    }

    func testErrorDetailsShowFullFailureWithoutControllerCalls() async {
        let failure = ControlFailure.launch(String(repeating: "signing configuration detail ", count: 20))
        for fromStatus in [false, true] {
            let control = FakeControl()
            let environment = RecordingEnvironment()
            let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "test")
            if fromStatus {
                control.on(.status, .failure(failure))
                await controller.refresh()
            } else {
                control.on(.updatesCheck, .failure(failure))
                await controller.perform(.checkForUpdates)
            }
            environment.errors.removeAll()
            let before = control.commands
            XCTAssertEqual(entry(buildMenu(controller.snapshot), .showErrorDetails)?.title, "View error details…")
            await controller.perform(.showErrorDetails)
            XCTAssertEqual(environment.errors, [failure.message])
            XCTAssertEqual(control.commands, before)
        }
    }

    func testImmediateUpgradeAndCancellationFinishQuietly() async {
        for cancel in [false, true] {
            let control = FakeControl()
            let environment = RecordingEnvironment()
            let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "test")
            control.on(.status, .success(status(.running)))
            control.on(.updatesPrepare(version: "0.7.1"), .success(.update(UpdateResult(state: .prepared, version: "0.7.1", transactionId: txn, detail: nil))))
            control.on(.updatesApply(transaction: txn), .success(.update(UpdateResult(state: cancel ? .waitingForIdle : .complete, version: "0.7.1", transactionId: txn, detail: nil))))
            control.on(.updatesStatus(transaction: txn), .success(.update(UpdateResult(state: .waitingForIdle, version: "0.7.1", transactionId: txn, detail: nil))))
            control.on(.updatesCancel(transaction: txn), .success(.update(UpdateResult(state: .cancelled, version: "0.7.1", transactionId: txn, detail: nil))))
            await controller.refresh()
            await controller.perform(.upgrade(version: "0.7.1"))
            if cancel {
                let windows = environment.upgrades.count
                await controller.cancelUpgrade()
                XCTAssertEqual(environment.upgrades.count, windows)
                XCTAssertEqual(controller.snapshot.update?.state, .cancelled)
            } else {
                XCTAssertTrue(environment.upgrades.isEmpty)
                XCTAssertEqual(controller.snapshot.update?.state, .complete)
            }
        }
    }

    func testUpgradeResultsOnlyRequestAttentionForFailures() async {
        for result in [UpdateState.complete, .cancelled, .failed, .recoveryRequired] {
            let control = FakeControl()
            let environment = RecordingEnvironment()
            let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "test")
            control.on(.status, .success(status(.running, transaction: txn)))
            control.on(.updatesStatus(transaction: txn),
                       .success(.update(UpdateResult(state: .installing, version: "0.7.1", transactionId: txn, detail: nil))),
                       .success(.update(UpdateResult(state: result, version: "0.7.1", transactionId: txn, detail: nil))))
            await controller.refresh()
            await controller.refresh()
            XCTAssertEqual(controller.snapshot.update?.state, result)
            XCTAssertEqual(environment.upgrades.count, result == .failed || result == .recoveryRequired ? 1 : 0)
            if result == .complete {
                // An explicit request for the last result still opens details.
                await controller.perform(.showUpgradeProgress)
                XCTAssertEqual(environment.upgrades.last??.state, .complete)
            }
        }
    }

    func testRefreshObservesStatusAndTrayLogin() async {
        control.on(.status, .success(status(.running)))
        environment.login = .requiresApproval
        await controller.refresh()
        XCTAssertEqual(controller.snapshot.serviceState, .running)
        XCTAssertEqual(controller.snapshot.trayLogin, .requiresApproval)
        XCTAssertEqual(environment.snapshots.count, 1)
    }

    func testStatusFailureAfterSuccessShowsUnknown() async {
        control.on(.status, .success(status(.running)), .failure(.timedOut(seconds: 15)))
        await controller.refresh()
        await controller.refresh()
        XCTAssertEqual(controller.snapshot.serviceState, .unknown)
    }

    func testStopRequiresConfirmationAndDeclineDoesNothing() async {
        control.on(.status, .success(status(.running)))
        await controller.refresh()
        environment.confirmAnswer = false
        await controller.perform(.stop)
        XCTAssertEqual(environment.confirmations.count, 1)
        XCTAssertFalse(control.commands.contains(.stop))
    }

    func testStopLeavesAppRunningAndRereadsStatus() async {
        control.on(.status, .success(status(.running)), .success(status(.stopped)))
        control.on(.stop, .success(.operation(.stop, .stopped)))
        await controller.refresh()
        await controller.perform(.stop)
        XCTAssertEqual(control.commands, [.status, .stop, .status])
        XCTAssertEqual(controller.snapshot.serviceState, .stopped)
        XCTAssertEqual(environment.quitCount, 0, "Stop never quits the tray app")
        XCTAssertTrue(environment.snapshots.contains { $0.busy == .stop }, "progress was visible")
        XCTAssertEqual(entry(buildMenu(controller.snapshot), .start)?.enabled, true)
    }

    func testQuitNeverCallsTheController() async {
        control.on(.status, .success(status(.running)))
        await controller.refresh()
        await controller.perform(.quit)
        XCTAssertEqual(environment.quitCount, 1)
        XCTAssertEqual(control.commands, [.status], "Quit sends no stop/startup command")
    }

    func testUnsafeShutdownErrorIsShownAndStateReread() async {
        control.on(.status, .success(status(.running)))
        control.on(.restart, .failure(.controller(ControlErrorV1(
            code: .unsafeShutdown, message: "two voters reachable", retryable: false))))
        await controller.refresh()
        await controller.perform(.restart)
        XCTAssertEqual(environment.errors.count, 1)
        XCTAssertTrue(environment.errors[0].contains("cannot stop safely"))
        XCTAssertTrue(environment.errors[0].contains("two voters reachable"))
        XCTAssertEqual(controller.snapshot.serviceState, .running)
        XCTAssertEqual(control.commands.last, .status)
    }

    func testServerStartupCheckboxSurvivesCancelledAuthorization() async {
        control.on(.status, .success(status(.running, server: .enabled)))
        control.on(.startupSetServer(.off), .failure(.controller(ControlErrorV1(
            code: .authorizationDenied, message: "cancelled", retryable: false))))
        await controller.refresh()
        await controller.perform(.toggleServerStartup)
        XCTAssertTrue(control.commands.contains(.startupSetServer(.off)))
        XCTAssertEqual(controller.snapshot.status?.serverStartup.state, .enabled)
        XCTAssertEqual(entry(buildMenu(controller.snapshot), .toggleServerStartup)?.check, .on)
        XCTAssertEqual(environment.errors.count, 1)
    }

    func testServerStartupAppliesObservedResultNotRequestedOne() async {
        control.on(.status, .success(status(.running, server: .disabled)), .success(status(.running, server: .policyManaged)))
        control.on(.startupSetServer(.on), .success(.startup(server: StartupStateV1(state: .policyManaged), tray: StartupStateV1(state: .unknown))))
        await controller.refresh()
        await controller.perform(.toggleServerStartup)
        XCTAssertEqual(controller.snapshot.status?.serverStartup.state, .policyManaged)
    }

    func testMaskedStartupIsNeverToggled() async {
        control.on(.status, .success(status(.running, server: .masked)))
        await controller.refresh()
        await controller.perform(.toggleServerStartup)
        XCTAssertEqual(control.commands, [.status])
    }

    func testTrayLoginToggleReadsBackOSStatusAndOpensApproval() async {
        environment.loginSetResult = .success(.requiresApproval)
        await controller.perform(.toggleTrayLogin)
        XCTAssertEqual(environment.loginSets, [true])
        XCTAssertEqual(controller.snapshot.trayLogin, .requiresApproval)
        XCTAssertEqual(environment.settingsOpened, 1)
        // While approval is pending the action opens Settings, never re-registers.
        await controller.perform(.toggleTrayLogin)
        XCTAssertEqual(environment.loginSets, [true])
        XCTAssertEqual(environment.settingsOpened, 2)
        XCTAssertTrue(control.commands.isEmpty, "the tray login item never involves the server controller")
    }

    func testTrayLoginFailureKeepsOSReportedState() async {
        environment.login = .enabled
        environment.loginSetResult = .failure(LoginItemFailure("denied"))
        await controller.perform(.toggleTrayLogin)
        XCTAssertEqual(controller.snapshot.trayLogin, .enabled)
        XCTAssertEqual(environment.errors, ["denied"])
    }

    func testControllerConstructionDoesNotRegisterLoginItem() {
        XCTAssertTrue(environment.loginSets.isEmpty)
    }

    func testOpenBrowserUsesStatusOrigin() async {
        control.on(.status, .success(status(.running)))
        await controller.refresh()
        await controller.perform(.openBrowser)
        XCTAssertEqual(environment.opened, [URL(string: "http://127.0.0.1:32400")!])
    }

    func testViewLogsShowsBoundedLines() async {
        control.on(.logs(lines: 1000), .success(.logs(lines: ["one", "two"], truncated: true)))
        await controller.perform(.viewLogs)
        XCTAssertEqual(environment.logs, [["one", "two"]])
    }

    func testUpgradePreparesConfirmsNamedVersionThenApplies() async {
        control.on(.status, .success(status(.running)))
        control.on(.updatesPrepare(version: "0.7.1"), .success(.update(UpdateResult(state: .prepared, version: "0.7.1", transactionId: txn, detail: nil))))
        control.on(.updatesApply(transaction: txn), .success(.update(UpdateResult(state: .waitingForIdle, version: "0.7.1", transactionId: txn, detail: nil))))
        control.on(.updatesStatus(transaction: txn), .success(.update(UpdateResult(state: .installing, version: "0.7.1", transactionId: txn, detail: nil))))
        await controller.refresh()
        await controller.perform(.upgrade(version: "0.7.1"))
        XCTAssertEqual(environment.confirmations.count, 1)
        XCTAssertTrue(environment.confirmations[0].contains("Upgrade plurx to 0.7.1?"))
        XCTAssertTrue(environment.confirmations[0].contains("from 0.7.0"))
        XCTAssertTrue(environment.confirmations[0].contains("Recovery required"))
        XCTAssertTrue(environment.confirmations[0].contains("never restored automatically"))
        XCTAssertFalse(environment.confirmations[0].contains("a compatible previous version is restored"))
        XCTAssertTrue(environment.confirmations[0].hasSuffix("|Upgrade to 0.7.1"))
        XCTAssertTrue(control.commands.contains(.updatesApply(transaction: txn)))
        XCTAssertEqual(controller.snapshot.update?.state, .installing)
        XCTAssertFalse(environment.upgrades.isEmpty)
    }

    func testDeclinedUpgradeCancelsThePreparation() async {
        control.on(.status, .success(status(.running)))
        control.on(.updatesPrepare(version: "0.7.1"), .success(.update(UpdateResult(state: .prepared, version: "0.7.1", transactionId: txn, detail: nil))))
        control.on(.updatesCancel(transaction: txn), .success(.update(UpdateResult(state: .cancelled, version: "0.7.1", transactionId: txn, detail: nil))))
        await controller.refresh()
        environment.confirmAnswer = false
        await controller.perform(.upgrade(version: "0.7.1"))
        XCTAssertTrue(control.commands.contains(.updatesCancel(transaction: txn)))
        XCTAssertFalse(control.commands.contains(.updatesApply(transaction: txn)))
    }

    func testPrepareOfDifferentVersionIsNotApplied() async {
        control.on(.status, .success(status(.running)))
        control.on(.updatesPrepare(version: "0.7.1"), .success(.update(UpdateResult(state: .prepared, version: "0.8.0", transactionId: txn, detail: nil))))
        await controller.refresh()
        await controller.perform(.upgrade(version: "0.7.1"))
        XCTAssertTrue(environment.confirmations.isEmpty)
        XCTAssertFalse(control.commands.contains(.updatesApply(transaction: txn)))
        XCTAssertEqual(environment.errors.count, 1)
    }

    func testCancelOnlyBeforeShutdownCommitment() async {
        control.on(.status, .success(status(.running, transaction: txn)))
        control.on(.updatesStatus(transaction: txn), .success(.update(UpdateResult(state: .shutdownCommitted, version: "0.7.1", transactionId: txn, detail: nil))))
        await controller.refresh()
        await controller.cancelUpgrade()
        XCTAssertFalse(control.commands.contains(.updatesCancel(transaction: txn)))
    }

    func testRecoveryRequiredIsFollowedAndShown() async {
        control.on(.status, .success(status(.stopped, transaction: txn)))
        control.on(.updatesStatus(transaction: txn), .success(.update(UpdateResult(state: .recoveryRequired, version: "0.7.1", transactionId: txn, detail: "journal phase installing"))))
        await controller.refresh()
        XCTAssertEqual(controller.snapshot.update?.state, .recoveryRequired)
        XCTAssertEqual(environment.upgrades.last??.state, .recoveryRequired)
        XCTAssertEqual(entry(buildMenu(controller.snapshot), .start)?.enabled, false, "no Start over an unrecovered transaction")
    }

    func testMutationsDoNotOverlap() async {
        control.on(.status, .success(status(.stopped)))
        control.on(.start, .success(.operation(.start, .starting)))
        await controller.refresh()
        async let first: Void = controller.perform(.start)
        async let second: Void = controller.perform(.start)
        _ = await (first, second)
        XCTAssertEqual(control.commands.filter { $0 == .start }.count, 1)
    }
}
