import Foundation
import XCTest
@testable import plurx

final class AutoQualityPolicyTests: XCTestCase {
    private func fixture() throws -> [String: Any] {
        let url = Bundle(for: AutoQualityPolicyTests.self).url(forResource: "auto-quality-policy", withExtension: "json")
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: XCTUnwrap(url))) as? [String: Any])
    }

    private func decode<T: Decodable>(_ type: T.Type, _ value: Any) throws -> T {
        try JSONDecoder().decode(type, from: JSONSerialization.data(withJSONObject: value))
    }

    private func fields(_ d: AutoQualityPolicy.Decision) -> [String: Any] {
        var result: [String: Any] = ["height": d.height.map { $0 as Any } ?? NSNull(),
            "reason": d.reason.map { $0 as Any } ?? NSNull(), "emergency": d.emergency,
            "mildSamples": d.mildSamples, "upgradeSinceMs": d.upgradeSinceMs.map { $0 as Any } ?? NSNull()]
        if let action = d.action { result["action"] = action }
        if let blocked = d.blockedHeights { result["blockedHeights"] = blocked }
        if let evidence = d.evidence {
            result["evidence"] = ["kind": evidence.kind,
                "age_ms": evidence.ageMs.map { $0 as Any } ?? NSNull(),
                "runway_seconds": evidence.runwaySeconds.map { $0 as Any } ?? NSNull(),
                "throughput_kbps": evidence.throughputKbps.map { $0 as Any } ?? NSNull()]
        }
        return result
    }

    func testSharedAutoQualityFixture() throws {
        let root = try fixture()
        XCTAssertEqual(root["schema"] as? Int, 1)
        let rawDefaults = try XCTUnwrap(root["defaults"] as? [String: Any])
        let defaults = try decode(AutoQualityPolicy.Defaults.self, rawDefaults)
        // No private platform constants: the web equality test pins this data
        // to AUTO_DEFAULTS, while native ports require the same parameter keys.
        XCTAssertEqual(Set(rawDefaults.keys), Set(Mirror(reflecting: defaults).children.compactMap(\.label)))
        XCTAssertNil(rawDefaults["switchBudgetPerHour"])
        XCTAssertEqual((root["proposed_defaults"] as? [String: Any])?["switchBudgetPerHour"] as? Int, 6)
        let ladder = try decode([AutoQualityPolicy.Rung].self, XCTUnwrap(root["ladder"]))
        let base = try XCTUnwrap(root["sample_defaults"] as? [String: Any])
        let cases = try XCTUnwrap(root["cases"] as? [[String: Any]])
        XCTAssertFalse(cases.isEmpty)
        var names = Set<String>()
        let keys: Set<String> = ["height", "reason", "emergency", "action", "evidence", "mildSamples", "upgradeSinceMs", "blockedHeights"]
        for item in cases {
            let name = try XCTUnwrap(item["name"] as? String)
            XCTAssertTrue(names.insert(name).inserted, "duplicate case: \(name)")
            let sample = base.merging(try XCTUnwrap(item["sample"] as? [String: Any])) { _, new in new }
            let decision = fields(AutoQualityPolicy.decideRung(ladder: ladder,
                sample: try decode(AutoQualityPolicy.Sample.self, sample), defaults: defaults))
            let expected = try XCTUnwrap(item["expect"] as? [String: Any])
            let current = item["web_current"] as? [String: Any]
            if let current {
                XCTAssertNotEqual(current as NSDictionary, expected as NSDictionary, name)
                XCTAssertGreaterThanOrEqual((item["finding"] as? String)?.count ?? 0, 200, name)
            }
            for (key, value) in current ?? expected {
                guard keys.contains(key) else {
                    XCTAssertNotNil(current, "\(name): \(key) is not a decision field")
                    continue
                }
                if key == "evidence" {
                    let actual = try XCTUnwrap(decision[key] as? [String: Any], name)
                    for (field, wanted) in try XCTUnwrap(value as? [String: Any]) {
                        XCTAssertEqual(actual[field] as? NSObject, wanted as? NSObject, "\(name): evidence.\(field)")
                    }
                } else {
                    XCTAssertEqual(decision[key] as? NSObject, value as? NSObject, "\(name): \(key)")
                }
            }
        }
        for cause in try XCTUnwrap(root["causes"] as? [String]) {
            XCTAssertTrue(names.contains { $0.hasPrefix("\(cause): ") }, "missing cause: \(cause)")
        }
        // Metadata coverage only: no native controller/tick exists in M2.
        let gates = try XCTUnwrap(root["controller_gates"] as? [[String: Any]])
        for state in ["Original", "Manual rung", "Paused", "Background / PiP", "HDR fidelity", "A stall-scoped control verdict"] {
            XCTAssertTrue(gates.contains { $0["viewer_state"] as? String == state }, "missing viewer state: \(state)")
        }
    }
}
