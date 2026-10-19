import AppKit
import CoreGraphics
import Darwin
import XCTest
@testable import PlurxTrayCore
@testable import PlurxTrayUI

final class SmokeReceiptTests: XCTestCase {
    func scratch() throws -> URL {
        // Durable task-local scratch, never the operator's application state.
        let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../../../../target/mac-smoke-test-fixtures").standardizedFileURL
        let directory = root.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        return directory
    }

    func testAtomicReceiptReplacesCompleteJSONWithPrivateMode() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        let output = directory.appendingPathComponent("receipt.json")
        try SmokeReceipt.write(["schema": "plurx-tray-smoke-v1", "nullable": NSNull()], to: output)
        try SmokeReceipt.write(["schema": "plurx-tray-smoke-v1", "ok": true], to: output)
        let receipt = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: output)) as? [String: Any])
        XCTAssertEqual(receipt["ok"] as? Bool, true)
        XCTAssertNil(receipt["nullable"])
        var metadata = stat()
        XCTAssertEqual(lstat(output.path, &metadata), 0)
        XCTAssertEqual(metadata.st_mode & 0o777, 0o600)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: directory.path), ["receipt.json"])
    }

    func testOversizedReceiptPreservesPreviousCompleteReceipt() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        let output = directory.appendingPathComponent("receipt.json")
        try SmokeReceipt.write(["previous": true], to: output)
        let previous = try Data(contentsOf: output)
        XCTAssertThrowsError(try SmokeReceipt.write(["error": String(repeating: "x", count: 65536)], to: output))
        XCTAssertEqual(try Data(contentsOf: output), previous)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(atPath: directory.path), ["receipt.json"])
    }

    func testSymlinkHardlinkAndDirectoryDestinationsRefused() throws {
        let directory = try scratch()
        defer { try? FileManager.default.removeItem(at: directory) }
        let original = directory.appendingPathComponent("original")
        try Data("keep".utf8).write(to: original)
        let symlink = directory.appendingPathComponent("symlink")
        try FileManager.default.createSymbolicLink(at: symlink, withDestinationURL: original)
        XCTAssertThrowsError(try SmokeReceipt.write(["bad": true], to: symlink))
        let hardlink = directory.appendingPathComponent("hardlink")
        XCTAssertEqual(link(original.path, hardlink.path), 0)
        XCTAssertThrowsError(try SmokeReceipt.write(["bad": true], to: hardlink))
        XCTAssertThrowsError(try SmokeReceipt.write(["bad": true], to: directory))
        let alias = directory.appendingPathComponent("alias")
        try FileManager.default.createSymbolicLink(at: alias, withDestinationURL: directory)
        XCTAssertThrowsError(try SmokeReceipt.write(["bad": true], to: alias.appendingPathComponent("receipt.json")))
        XCTAssertEqual(try String(contentsOf: original), "keep")
    }
}

@MainActor
final class SmokeObservationTests: XCTestCase {
    func testFirstStatusFailurePreservesControllerCodeAndExplicitNulls() {
        let first = SmokeDriver.firstStatus(.failure(.controller(.init(
            code: .invalidInstallation, message: "no owned install", retryable: false))))
        XCTAssertEqual(first["ok"] as? Bool, false)
        XCTAssertEqual(first["error_code"] as? String, "invalid_installation")
        XCTAssertTrue(first["service_state"] is NSNull)
        XCTAssertTrue(first["version"] is NSNull)
        XCTAssertTrue((first["failure"] as? String)?.contains("no owned install") == true)
        XCTAssertEqual(SmokeDriver.firstStatus(nil)["ok"] as? Bool, false)
    }

    func testSessionObservationMatchesIndependentCoreGraphicsDictionary() {
        let actual = SmokeDriver.session()
        let raw = CGSessionCopyCurrentDictionary() as? [String: Any]
        let uid = raw?["kCGSSessionUserIDKey"] as? NSNumber
        let console = raw?["kCGSSessionOnConsoleKey"] as? Bool ?? false
        let loggedIn = raw?["kCGSessionLoginDoneKey"] as? Bool ?? false
        XCTAssertEqual(actual["console_user_uid"] as? NSNumber, uid)
        XCTAssertEqual(actual["session_id"] as? NSNumber, raw?["kCGSSessionIDKey"] as? NSNumber)
        XCTAssertEqual(actual["gui_session"] as? Bool, console && loggedIn && uid?.uint32Value == getuid())
    }
}
