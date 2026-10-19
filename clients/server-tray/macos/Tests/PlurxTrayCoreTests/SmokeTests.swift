import XCTest
@testable import PlurxTrayCore

final class SmokeOptionsTests: XCTestCase {
    func testOnlyExplicitReceiptEnablesSmokeAndDefaultsToQuit() throws {
        XCTAssertNil(try SmokeOptions.parse([]))
        XCTAssertNil(try SmokeOptions.parse(["-psn_0_123"]))
        let options = try XCTUnwrap(SmokeOptions.parse(["--smoke-receipt", "/owned/result.json"]))
        XCTAssertEqual(options.action, .quit)
        XCTAssertEqual(options.timeoutSeconds, 60)
        XCTAssertNil(options.installation)
        let explicit = try XCTUnwrap(SmokeOptions.parse([
            "--smoke-installation", "/owned/installation.json", "--smoke-action", "restart",
            "--smoke-timeout", "600", "--smoke-receipt", "/owned/result.json",
        ]))
        XCTAssertEqual(explicit.installation?.path, "/owned/installation.json")
        XCTAssertEqual(explicit.action, .restart)
        XCTAssertEqual(explicit.timeoutSeconds, 600)
    }

    func testMalformedCLIHasNoSideEffectsAndRejectsAmbiguousAuthority() {
        let cases = [
            ["--smoke-action", "stop"], ["--smoke-timeout", "10"], ["--smoke-installation", "/owned/install"],
            ["--smoke-receipt"], ["--smoke-receipt", "relative.json"], ["--smoke-receipt", "/"],
            ["--smoke-receipt", "/owned/a", "--smoke-receipt", "/owned/b"],
            ["--smoke-receipt", "/owned/a", "--smoke-action", "start"],
            ["--smoke-receipt", "/owned/a", "--unknown", "yes"],
            ["--smoke-receipt", "/owned/a", "--smoke-installation", "relative.json"],
            ["--smoke-receipt", "/owned/a\0"],
        ] + ["0", "601", "-1", "nan", "1.0", "+1", "999999999999999999999999"].map {
            ["--smoke-receipt", "/owned/a", "--smoke-timeout", $0]
        }
        for arguments in cases { XCTAssertThrowsError(try SmokeOptions.parse(arguments), "\(arguments)") }
    }
}

@MainActor
final class SmokeActionTests: XCTestCase {
    func testQuitObservationAndNormalQuitIssueNoServerCommand() async {
        let fake = FakeControl()
        let control = SmokeControl(fake)
        let environment = RecordingEnvironment()
        let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "smoke-test")
        let ok = await performSmokeAction(.quit, controller: controller, control: control)
        XCTAssertTrue(ok)
        XCTAssertTrue(control.commands.isEmpty)
        await controller.perform(.quit)
        XCTAssertEqual(environment.quitCount, 1)
        XCTAssertTrue(control.commands.isEmpty)
        XCTAssertTrue(environment.loginSets.isEmpty)
    }

    func testStopAndRestartUseConfirmationMutationAndFollowupStatusOnly() async {
        for requested in [SmokeOptions.RequestedAction.stop, .restart] {
            let fake = FakeControl()
            let command: ControlCommand = requested == .stop ? .stop : .restart
            let kind: OperationKind = requested == .stop ? .stop : .restart
            let after: ServiceState = requested == .stop ? .stopped : .running
            fake.on(.status, .success(status(.running)), .success(status(after)))
            fake.on(command, .success(.operation(kind, after)))
            let control = SmokeControl(fake)
            let environment = RecordingEnvironment()
            let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "smoke-test")
            await controller.refresh()
            let ok = await performSmokeAction(requested, controller: controller, control: control)
            XCTAssertTrue(ok)
            XCTAssertEqual(control.commands, [.status, command, .status])
            XCTAssertEqual(environment.confirmations.count, 1)
            XCTAssertTrue(environment.loginSets.isEmpty)
            XCTAssertEqual(environment.quitCount, 0, "UI survives the server action")
            guard case let .success(.status(first))? = control.firstStatus else { return XCTFail("first status missing") }
            XCTAssertEqual(first.serviceState, .running, "first observation never changes after Stop")
            XCTAssertEqual(controller.snapshot.serviceState, after)
        }
    }

    func testUnknownDisabledAndDeclinedActionsCannotMutate() async {
        for requested in [SmokeOptions.RequestedAction.stop, .restart] {
            for observed in [ServiceState.unknown, .stopped, .running] {
                let fake = FakeControl()
                fake.on(.status, .success(status(observed)))
                let control = SmokeControl(fake)
                let environment = RecordingEnvironment()
                environment.confirmAnswer = false
                let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "smoke-test")
                await controller.refresh()
                let ok = await performSmokeAction(requested, controller: controller, control: control)
                XCTAssertFalse(ok)
                XCTAssertEqual(control.commands, [.status])
                XCTAssertTrue(environment.loginSets.isEmpty)
            }
        }
    }

    func testFailedStatusDoesNotReusePriorRunningAuthority() async {
        let fake = FakeControl()
        fake.on(.status, .success(status(.running)), .failure(.timedOut(seconds: 15)))
        let control = SmokeControl(fake)
        let environment = RecordingEnvironment()
        let controller = TrayController(control: control, environment: environment, appVersion: "0.7.0", appBuild: "smoke-test")
        await controller.refresh()
        await controller.refresh()
        let ok = await performSmokeAction(.stop, controller: controller, control: control)
        XCTAssertFalse(ok)
        XCTAssertEqual(controller.snapshot.serviceState, .unknown)
        XCTAssertEqual(control.commands, [.status, .status])
    }
}
