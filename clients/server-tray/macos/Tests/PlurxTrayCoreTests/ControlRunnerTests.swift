// Real child-process behaviour of ControlRunner against small stub
// executables: argv is passed as an array, stdout is bounded, stderr stays
// diagnostics, and a hung read is terminated.
import XCTest
@testable import PlurxTrayCore

final class ControlRunnerTests: XCTestCase {
    var directory: URL!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("plurx-tray-runner-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    func stub(_ body: String) throws -> URL {
        let url = directory.appendingPathComponent("plurx-control")
        try ("#!/bin/sh\n" + body + "\n").write(to: url, atomically: true, encoding: .utf8)
        try FileManager.default.setAttributes([.posixPermissions: 0o755], ofItemAtPath: url.path)
        return url
    }

    func testArgumentsArriveAsOneArrayWithoutShellInterpretation() async throws {
        // The stub echoes its argv count and the transaction argument back
        // inside a valid update response.
        let url = try stub(#"""
        printf '{"schema_version":1,"ok":true,"result":{"kind":"update","state":"cancelled","version":null,"transaction_id":"%s","detail":"argc=%s"},"error":null}' "$4" "$#"
        echo "diagnostic on stderr" >&2
        """#)
        let result = await ControlRunner(executable: url).run(.updatesCancel(transaction: txn))
        XCTAssertEqual(try result.get(), .update(UpdateResult(state: .cancelled, version: nil, transactionId: txn, detail: "argc=4")))
    }

    func testControllerErrorWithNonzeroExitKeepsStructuredCode() async throws {
        let url = try stub(#"""
        printf '{"schema_version":1,"ok":false,"result":null,"error":{"code":"busy","message":"upgrade running","retryable":true}}'
        exit 3
        """#)
        let result = await ControlRunner(executable: url).run(.start)
        XCTAssertEqual(result, .failure(.controller(ControlErrorV1(code: .busy, message: "upgrade running", retryable: true))))
    }

    func testCrashWithoutJSONReportsExitAndLastDiagnostic() async throws {
        let url = try stub("echo 'first' >&2; echo 'panicked at main.rs' >&2; exit 101")
        let result = await ControlRunner(executable: url).run(.status)
        guard case let .failure(.exited(status, diagnostics)) = result else { return XCTFail("\(result)") }
        XCTAssertEqual(status, 101)
        XCTAssertTrue(diagnostics.contains("panicked"))
        XCTAssertTrue(ControlFailure.exited(status: status, diagnostics: diagnostics).message.hasSuffix("panicked at main.rs"))
    }

    func testSuccessJSONWithNonzeroExitIsNotTrusted() async throws {
        let url = try stub(#"printf '{"schema_version":1,"ok":true,"result":{"kind":"operation","operation":"start","service_state":"starting"},"error":null}'; exit 1"#)
        guard case .failure(.exited(1, _)) = await ControlRunner(executable: url).run(.start) else {
            return XCTFail("success body with failing exit must not be acted on")
        }
    }

    func testOversizedStdoutIsRefused() async throws {
        let url = try stub("head -c 1100000 /dev/zero | tr '\\0' 'a'")
        let result = await ControlRunner(executable: url).run(.status)
        XCTAssertEqual(result, .failure(.outputTooLarge))
    }

    func testMissingExecutableIsALaunchFailure() async {
        let result = await ControlRunner(executable: directory.appendingPathComponent("absent")).run(.status)
        guard case .failure(.launch) = result else { return XCTFail("\(result)") }
    }

    func testHungReadIsTerminatedAtItsBound() throws {
        let url = try stub("exec sleep 30")
        let started = Date()
        let result = ControlRunner.execute(executable: url, arguments: ["status"], timeout: 1, terminateOnTimeout: true)
        XCTAssertEqual(result, .failure(.timedOut(seconds: 1)))
        XCTAssertLessThan(Date().timeIntervalSince(started), 10)
    }
}
