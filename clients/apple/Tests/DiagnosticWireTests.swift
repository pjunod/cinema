import XCTest
@testable import plurx
final class DiagnosticWireTests: XCTestCase {
    func testSharedManifestFixturesRejectPrivacyAndEncodingViolations() throws {
        let url=try XCTUnwrap(Bundle(for:Self.self).url(forResource:"diagnostics-v1",withExtension:"json"))
        let f=try JSONSerialization.jsonObject(with:Data(contentsOf:url)) as! [String:Any]
        for c in f["cases"] as! [[String:Any]] {
            let data=Data((c["json"] as! String).utf8)
            XCTAssertEqual((try? DiagnosticWire.manifest(data)) != nil,c["valid"] as! Bool,c["name"] as! String)
        }
    }
}
