import Foundation
import XCTest
@testable import plurx

#if DEBUG
final class AcceptanceBootstrapTests: XCTestCase {
    func testAcceptanceBootstrapUsesArgumentBearerWithoutChangingVault() throws {
        final class Vault: TokenStoring {
            var value: String? = "saved-session"
            var reads = 0
            var writes = 0
            func read() -> String? { reads += 1; return value }
            func write(_ value: String) -> Bool { writes += 1; self.value = value; return true }
            func clear() { value = nil }
        }
        let name = "acceptance-bootstrap-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer { defaults.removePersistentDomain(forName: name) }
        let vault = Vault()
        let settings = SettingsStore(defaults: defaults, tokenVault: vault)
        XCTAssertEqual(settings.acceptanceBootstrapToken(active: true, argumentDomain: ["plurx.token": "current-lab-session"]), "current-lab-session")
        XCTAssertEqual(vault.value, "saved-session")
        XCTAssertEqual(vault.reads, 0)
        XCTAssertEqual(vault.writes, 0)
        XCTAssertEqual(settings.acceptanceBootstrapToken(active: false, argumentDomain: ["plurx.token": "current-lab-session"]), "saved-session")
        XCTAssertEqual(vault.writes, 0)
        XCTAssertEqual(settings.acceptanceBootstrapToken(active: true, argumentDomain: ["plurx.token": ""]), "saved-session")
        vault.value = nil
        XCTAssertEqual(settings.acceptanceBootstrapToken(active: true, argumentDomain: ["plurx.token": "second-lab-session"]), "second-lab-session")
        XCTAssertNil(vault.value)
        XCTAssertEqual(vault.writes, 0)
    }
}
#endif
