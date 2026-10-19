// Protocol regressions against the frozen CONTROL-CONTRACT.md examples. The
// example strings are the contract's own bytes; a renamed field or state on
// either side fails here instead of being coerced into a plausible state.
import XCTest
@testable import PlurxTrayCore

final class ProtocolTests: XCTestCase {
    static let statusExample = #"{"schema_version":1,"ok":true,"result":{"kind":"status","installation_id":"00000000-0000-4000-8000-000000000001","service_state":"stopped","version":"0.7.0","browser_origin":"http://127.0.0.1:8080","server_startup":{"state":"disabled","detail":null},"tray_startup":{"state":"unknown","detail":null},"transaction_id":null},"error":null}"#

    func decode(_ text: String) -> Result<ControlResult, ControlFailure> {
        decodeControlResponse(Data(text.utf8))
    }

    func testContractStatusExampleDecodes() throws {
        guard case let .success(.status(status)) = decode(Self.statusExample) else {
            return XCTFail("status example must decode: \(decode(Self.statusExample))")
        }
        XCTAssertEqual(status.serviceState, .stopped)
        XCTAssertEqual(status.version, "0.7.0")
        XCTAssertEqual(status.browserOrigin, "http://127.0.0.1:8080")
        XCTAssertEqual(status.serverStartup, StartupStateV1(state: .disabled))
        XCTAssertEqual(status.trayStartup.state, .unknown)
        XCTAssertNil(status.transactionId)
    }

    func testContractOperationStartupUpdateAndLogsExamplesDecode() {
        XCTAssertEqual(
            try decode(#"{"schema_version":1,"ok":true,"result":{"kind":"operation","operation":"start","service_state":"starting"},"error":null}"#).get(),
            .operation(.start, .starting))
        XCTAssertEqual(
            try decode(#"{"schema_version":1,"ok":true,"result":{"kind":"startup","server":{"state":"enabled","detail":null},"tray":{"state":"disabled","detail":null}},"error":null}"#).get(),
            .startup(server: StartupStateV1(state: .enabled), tray: StartupStateV1(state: .disabled)))
        XCTAssertEqual(
            try decode(#"{"schema_version":1,"ok":true,"result":{"kind":"update","state":"prepared","version":"0.7.1","transaction_id":"00000000-0000-4000-8000-000000000002","detail":null},"error":null}"#).get(),
            .update(UpdateResult(state: .prepared, version: "0.7.1",
                                 transactionId: "00000000-0000-4000-8000-000000000002", detail: nil)))
        XCTAssertEqual(
            try decode(#"{"schema_version":1,"ok":true,"result":{"kind":"logs","lines":["example"],"truncated":false},"error":null}"#).get(),
            .logs(lines: ["example"], truncated: false))
    }

    func testEveryContractStateAndErrorStringDecodes() {
        let services = ["running", "starting", "stopped", "unhealthy", "unknown"]
        XCTAssertEqual(Set(ServiceState.allCases.map(\.rawValue)), Set(services))
        let startup = ["enabled", "disabled", "masked", "static", "approval_required", "policy_managed", "unknown"]
        XCTAssertEqual(Set(StartupState.allCases.map(\.rawValue)), Set(startup))
        let updates = ["idle", "checking", "available", "prepared", "waiting_for_idle", "shutdown_committed",
                       "installing", "restarting", "complete", "cancelled", "failed", "recovery_required"]
        XCTAssertEqual(Set(UpdateState.allCases.map(\.rawValue)), Set(updates))
        let errors = ["invalid_installation", "unsupported", "authorization_required", "authorization_denied", "busy",
                      "identity_mismatch", "unsafe_shutdown", "policy_conflict", "unavailable", "rate_limited",
                      "untrusted_update", "recovery_required", "invalid_request", "io"]
        XCTAssertEqual(Set(ErrorCode.allCases.map(\.rawValue)), Set(errors))
    }

    func testControllerErrorKeepsItsCode() {
        let text = #"{"schema_version":1,"ok":false,"result":null,"error":{"code":"authorization_denied","message":"polkit said no","retryable":false}}"#
        XCTAssertEqual(decode(text), .failure(.controller(ControlErrorV1(
            code: .authorizationDenied, message: "polkit said no", retryable: false))))
    }

    func testUnknownStateIsAProtocolFailureNotStopped() {
        let text = Self.statusExample.replacingOccurrences(of: #""service_state":"stopped""#,
                                                           with: #""service_state":"halted""#)
        guard case .failure(.protocolViolation) = decode(text) else {
            return XCTFail("an unknown state string must not decode")
        }
    }

    func testOtherSchemaVersionIsRefused() {
        let text = Self.statusExample.replacingOccurrences(of: #"{"schema_version":1"#, with: #"{"schema_version":2"#)
        guard case let .failure(.protocolViolation(message)) = decode(text) else {
            return XCTFail("schema 2 must be refused")
        }
        XCTAssertTrue(message.contains("schema 2"))
    }

    func testUnknownResultFieldAndUnknownKindAreRefused() {
        let extra = Self.statusExample.replacingOccurrences(of: #""transaction_id":null}"#,
                                                            with: #""transaction_id":null,"pid":1}"#)
        guard case .failure(.protocolViolation) = decode(extra) else { return XCTFail("extra field accepted") }
        let kind = #"{"schema_version":1,"ok":true,"result":{"kind":"reboot"},"error":null}"#
        guard case .failure(.protocolViolation) = decode(kind) else { return XCTFail("unknown kind accepted") }
        let envelope = #"{"schema_version":1,"ok":true,"result":{"kind":"logs","lines":[],"truncated":false},"error":null,"x":1}"#
        guard case .failure(.protocolViolation) = decode(envelope) else { return XCTFail("extra envelope field accepted") }
    }

    func testMissingNullableFieldIsRefused() {
        let text = Self.statusExample.replacingOccurrences(of: #","transaction_id":null"#, with: "")
        guard case .failure(.protocolViolation) = decode(text) else {
            return XCTFail("an absent nullable field must not default to null")
        }
    }

    func testOkFlagMustAgreeWithResultAndError() {
        let both = #"{"schema_version":1,"ok":true,"result":null,"error":{"code":"io","message":"","retryable":false}}"#
        guard case .failure(.protocolViolation) = decode(both) else { return XCTFail("ok=true with error accepted") }
        let neither = #"{"schema_version":1,"ok":false,"result":null,"error":null}"#
        guard case .failure(.protocolViolation) = decode(neither) else { return XCTFail("empty failure accepted") }
    }

    func testOversizedResponseIsRefusedBeforeParsing() {
        let data = Data(repeating: 0x20, count: maximumResponseBytes + 1)
        XCTAssertEqual(decodeControlResponse(data), .failure(.outputTooLarge))
    }
}

final class CommandTests: XCTestCase {
    func testContractArgumentArrays() throws {
        XCTAssertEqual(try ControlCommand.status.arguments(), ["status"])
        XCTAssertEqual(try ControlCommand.start.arguments(), ["start"])
        XCTAssertEqual(try ControlCommand.stop.arguments(), ["stop", "--confirm-interruption"])
        XCTAssertEqual(try ControlCommand.restart.arguments(), ["restart", "--confirm-interruption"])
        XCTAssertEqual(try ControlCommand.startupGet.arguments(), ["startup", "get"])
        XCTAssertEqual(try ControlCommand.startupSetServer(.off).arguments(), ["startup", "set", "server", "off"])
        XCTAssertEqual(try ControlCommand.updatesCheck.arguments(), ["updates", "check"])
        XCTAssertEqual(try ControlCommand.updatesPrepare(version: "0.7.1").arguments(),
                       ["updates", "prepare", "--version", "0.7.1"])
        let id = "00000000-0000-4000-8000-000000000002"
        XCTAssertEqual(try ControlCommand.updatesApply(transaction: id).arguments(),
                       ["updates", "apply", "--transaction", id, "--confirm-interruption"])
        XCTAssertEqual(try ControlCommand.updatesStatus(transaction: nil).arguments(), ["updates", "status"])
        XCTAssertEqual(try ControlCommand.updatesStatus(transaction: id).arguments(),
                       ["updates", "status", "--transaction", id])
        XCTAssertEqual(try ControlCommand.updatesCancel(transaction: id).arguments(),
                       ["updates", "cancel", "--transaction", id])
        XCTAssertEqual(try ControlCommand.logs(lines: 1000).arguments(), ["logs", "--lines", "1000"])
    }

    func testValuesThatCouldBecomeOptionsOrPathsAreRefused() {
        XCTAssertThrowsError(try ControlCommand.updatesPrepare(version: "--installation=/x").arguments())
        XCTAssertThrowsError(try ControlCommand.updatesPrepare(version: "v0.7.1").arguments())
        XCTAssertThrowsError(try ControlCommand.updatesPrepare(version: "0.7.1 ; rm").arguments())
        XCTAssertThrowsError(try ControlCommand.updatesApply(transaction: "../../etc").arguments())
        XCTAssertThrowsError(try ControlCommand.updatesCancel(transaction: "00000000-0000-4000-8000-00000000000Z").arguments())
        XCTAssertThrowsError(try ControlCommand.logs(lines: 0).arguments())
        XCTAssertThrowsError(try ControlCommand.logs(lines: 1001).arguments())
        XCTAssertTrue(ControlCommand.isVersion("1.2.3-rc.1+build.5"))
    }

    func testInstallationOverridePrecedesTheCommand() throws {
        let runner = ControlRunner(executable: URL(fileURLWithPath: "/x/plurx-control"),
                                   installationRecord: URL(fileURLWithPath: "/rec/installation.json"))
        XCTAssertEqual(try runner.argv(for: .status), ["--installation", "/rec/installation.json", "status"])
    }

    func testControllerEnvironmentDropsProxiesAndInjectedLibraries() {
        let environment = ControlRunner.environment(from: [
            "HOME": "/Users/a", "PATH": "/evil", "https_proxy": "http://p", "HTTPS_PROXY": "http://p",
            "DYLD_INSERT_LIBRARIES": "/x.dylib", "LANG": "en_US.UTF-8",
        ])
        XCTAssertEqual(environment, ["HOME": "/Users/a", "LANG": "en_US.UTF-8", "PATH": "/usr/bin:/bin:/usr/sbin:/sbin"])
    }

    func testBrowserURLRequiresAbsoluteHTTPWithHostAndNoCredentials() {
        XCTAssertEqual(browserURL(from: "http://127.0.0.1:32400/base")?.absoluteString, "http://127.0.0.1:32400/base")
        XCTAssertNil(browserURL(from: "file:///etc/passwd"))
        XCTAssertNil(browserURL(from: "javascript:alert(1)"))
        XCTAssertNil(browserURL(from: "http://user:pw@127.0.0.1/"))
        XCTAssertNil(browserURL(from: "/relative"))
    }
}
