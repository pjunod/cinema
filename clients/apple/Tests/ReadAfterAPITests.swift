import Foundation
import XCTest
@testable import plurx

private final class ReceiptAPIProtocol: URLProtocol {
    static var answer: ((URLRequest) -> (Int, String?))?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let (status, index) = Self.answer?(request) else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse)); return
        }
        var headers = ["Content-Type": "application/json"]
        if let index { headers["X-Plurx-Commit-Index"] = index }
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status,
            httpVersion: nil, headerFields: headers)!, cacheStoragePolicy: .notAllowed)
        if status != 204 { client?.urlProtocol(self, didLoad: Data("[]".utf8)) }
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class ReadAfterAPITests: XCTestCase {
    func testActualNoContentProgressAndLibrariesShareTheUnsignedReceipt() async throws {
        let saved = Session.shared.credentials
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ReceiptAPIProtocol.self]
        let session = URLSession(configuration: config)
        defer {
            session.invalidateAndCancel(); ReceiptAPIProtocol.answer = nil
            Session.shared.setCredentials(origin: saved.origin, token: saved.token)
        }
        Session.shared.setCredentials(origin: "https://api.test", token: "api-fixture")
        let api = PlurxAPI(origin: "https://api.test", testTransport: session)
        var ordinal = 0
        ReceiptAPIProtocol.answer = { request in
            ordinal += 1
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer api-fixture")
            if ordinal == 1 {
                XCTAssertEqual(request.url?.path, "/api/v1/items/7/progress")
                XCTAssertEqual(request.httpMethod, "POST")
                return (204, "18446744073709551615")
            }
            if ordinal == 2 || ordinal == 3 {
                XCTAssertEqual(request.value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
            } else { XCTAssertNil(request.value(forHTTPHeaderField: "X-Plurx-Read-After")) }
            return (request.httpMethod == "POST" ? 204 : 200, nil)
        }
        try await api.progress(itemId: 7, positionMs: 10, durationMs: 100)
        let first = try await api.libraries(); XCTAssertTrue(first.isEmpty)
        try await api.progress(itemId: 7, positionMs: 20, durationMs: 100)
        let second = try await api.libraries(); XCTAssertTrue(second.isEmpty)
        XCTAssertEqual(ordinal, 4)
    }
}
