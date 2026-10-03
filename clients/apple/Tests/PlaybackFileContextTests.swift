import Foundation
import XCTest
@testable import plurx

private final class FileContextHTTP: URLProtocol {
    static var body = Data()
    static var beforeResponse: (() -> Void)?
    static var lastRequest: URLRequest?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Self.lastRequest = request; Self.beforeResponse?()
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: 200,
                                httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.body); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class PlaybackFileContextTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111",
        serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333",
        libraryId: "9007199254740993", itemId: "9223372036854775807")
    private let revision = String(repeating: "a", count: 64)
    private let file = "9007199254740993"
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/signed_locator-ABC123" }
    override func tearDown() { FileContextHTTP.beforeResponse = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func fetch(_ base: String?, mutate: (inout [String: Any]) -> Void = { _ in }) async throws -> PlaybackFileContext {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let reference = try JSONSerialization.jsonObject(with: encoder.encode(ref))
        var row: [String: Any] = ["file_id": file, "revision": revision,
                                  "reference": ["item": reference, "file_id": file, "revision": revision]]
        row["file_base"] = base
        mutate(&row)
        FileContextHTTP.body = try JSONSerialization.data(withJSONObject: ["files": [row]])
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [FileContextHTTP.self]
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: file,
                                                                 testTransport: URLSession(configuration: config))
    }
    func testAuthenticatedBContextPreservesExactReferenceAndSessionAuthority() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        XCTAssertEqual(FileContextHTTP.lastRequest?.value(forHTTPHeaderField: "Authorization"), "Bearer fixture-bearer")
        XCTAssertEqual(FileContextHTTP.lastRequest?.url?.host, "b.test")
        XCTAssertEqual(context.sourceFileId, file); XCTAssertEqual(context.reference, ref)
        XCTAssertNotEqual(context.sourceKey, try PlaybackFileContext.local(file).sourceKey)
        XCTAssertEqual(try context.path("decision"), base + "/decision")
        XCTAssertThrowsError(try context.path("direct"))
        let bound = try context.withSession("44444444-4444-4444-8444-444444444444")
        XCTAssertEqual(try bound.path("direct"), base + "/direct?session=44444444-4444-4444-8444-444444444444")
        XCTAssertNil(context.sessionId)
        XCTAssertThrowsError(try context.withSession("44444444-4444-1444-8444-444444444444"))
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        XCTAssertNoThrow(try context.path("decision"))
        Session.shared.setCredentials(origin: "https://b.test", token: nil)
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        XCTAssertThrowsError(try context.path("decision"))
    }
    func testMissingForeignOrMalformedLocatorCannotBecomeLocal() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        for candidate in [nil, "/api/v1/files/7", "https://a.test" + base, base + "?token=x", base + "/..", base + "%2f", base + "#fragment", base + "\n"] {
            do { _ = try await fetch(candidate); XCTFail("accepted invalid locator") } catch {}
        }
        FileContextHTTP.beforeResponse = { Session.shared.setCredentials(origin: "https://other.test", token: "other") }
        do { _ = try await fetch(base); XCTFail("accepted changed account during request") } catch {}
    }
    func testExactReferenceFileAndRevisionMustMatchAuthenticatedDetail() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let changes: [(inout [String: Any]) -> Void] = [
            { $0["file_id"] = 9_007_199_254_740_993 as Int64 },
            { $0["revision"] = "malformed" },
            { var binding = $0["reference"] as! [String: Any]; binding["file_id"] = "7"; $0["reference"] = binding },
            { var binding = $0["reference"] as! [String: Any]; var item = binding["item"] as! [String: Any]; item["server_id"] = "55555555-5555-4555-8555-555555555555"; binding["item"] = item; $0["reference"] = binding },
        ]
        for change in changes {
            do { _ = try await fetch(base, mutate: change); XCTFail("accepted mismatched file reference") } catch {}
        }
    }
    func testLocalExactIDsAndClosedResources() throws {
        XCTAssertEqual(try PlaybackFileContext.local(Int64.max.description).path("direct"), "/api/v1/files/9223372036854775807/direct")
        for id in ["01", "-1", "9223372036854775808", "1.0", "1/2"] { XCTAssertThrowsError(try PlaybackFileContext.local(id)) }
        let context = try PlaybackFileContext.local(7)
        for resource in ["../direct", "direct?token=x", "//a.test", "subs/1/../2", "hls/foreign/status"] {
            XCTAssertThrowsError(try context.path(resource))
        }
    }
}
