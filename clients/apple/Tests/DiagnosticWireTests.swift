import XCTest
@testable import plurx
final class DiagnosticWireTests: XCTestCase {
    func testSharedEventFixturesPreserveCodecLayersAndRefusePrivateFields() throws {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "diagnostics-events-v1", withExtension: "json"))
        let fixture = try JSONSerialization.jsonObject(with: Data(contentsOf: url)) as! [String: Any]
        for item in fixture["cases"] as! [[String: Any]] {
            XCTAssertEqual((try? DiagnosticWire.event(Data((item["json"] as! String).utf8))) != nil, item["valid"] as! Bool, item["name"] as! String)
        }
    }
    func testSharedManifestFixturesRejectPrivacyAndEncodingViolations() throws {
        let url=try XCTUnwrap(Bundle(for:Self.self).url(forResource:"diagnostics-v1",withExtension:"json"))
        let f=try JSONSerialization.jsonObject(with:Data(contentsOf:url)) as! [String:Any]
        for c in f["cases"] as! [[String:Any]] {
            let data=Data((c["json"] as! String).utf8)
            XCTAssertEqual((try? DiagnosticWire.manifest(data)) != nil,c["valid"] as! Bool,c["name"] as! String)
        }
    }
}
