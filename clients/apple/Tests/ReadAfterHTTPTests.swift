import Foundation
import XCTest
@testable import plurx

private final class ReceiptHTTPFixture: @unchecked Sendable {
    enum Reply { case ok(Int, String?); case failure(URLError); case redirect(String) }
    private let lock = NSLock()
    private var replies: [(URLRequest) -> Reply] = []
    private var requests: [URLRequest] = []
    private var auth = ReadAfterHTTP.Authorization(origin: "https://a.test", token: "fixture-a", generation: 1)
    func authorization() -> ReadAfterHTTP.Authorization { lock.lock(); defer { lock.unlock() }; return auth }
    func changeAccount() { lock.lock(); auth = .init(origin: "https://a.test", token: "fixture-new", generation: 2); lock.unlock() }
    func enqueue(_ reply: Reply) { enqueue { _ in reply } }
    func enqueue(_ reply: @escaping (URLRequest) -> Reply) { lock.lock(); replies.append(reply); lock.unlock() }
    func answer(_ request: URLRequest) -> Reply {
        lock.lock(); requests.append(request)
        let next = replies.isEmpty ? nil : replies.removeFirst(); lock.unlock()
        return next?(request) ?? .failure(URLError(.badServerResponse))
    }
    var captured: [URLRequest] { lock.lock(); defer { lock.unlock() }; return requests }
}

private final class ReceiptHTTPProtocol: URLProtocol {
    static var fixture: ReceiptHTTPFixture?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        guard let fixture = Self.fixture else { client?.urlProtocol(self, didFailWithError: URLError(.cancelled)); return }
        switch fixture.answer(request) {
        case .ok(let status, let index):
            var headers = ["Content-Type": "application/json"]
            if let index { headers["X-Plurx-Commit-Index"] = index }
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers)!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            if status != 204 { client?.urlProtocol(self, didLoad: Data("{}".utf8)) }
            client?.urlProtocolDidFinishLoading(self)
        case .failure(let error): client?.urlProtocol(self, didFailWithError: error)
        case .redirect(let location):
            let response = HTTPURLResponse(url: request.url!, statusCode: 302, httpVersion: "HTTP/1.1", headerFields: ["Location": location])!
            var redirected = request; redirected.url = URL(string: location); redirected.httpMethod = "GET"
            client?.urlProtocol(self, wasRedirectedTo: redirected, redirectResponse: response)
        }
    }
    override func stopLoading() {}
}

/// Synthetic HTTP through the production receipt adapter, not a live server,
/// shipped API qualification, or a device/network-cost measurement.
final class ReadAfterHTTPTests: XCTestCase {
    private var fixture: ReceiptHTTPFixture!
    private var transport: ReadAfterHTTP!
    private var session: URLSession!
    override func setUp() {
        fixture = ReceiptHTTPFixture(); ReceiptHTTPProtocol.fixture = fixture
        transport = ReadAfterHTTP(authorization: { [fixture = fixture!] in fixture.authorization() })
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [ReceiptHTTPProtocol.self]
        session = URLSession(configuration: config)
    }
    override func tearDown() { session.invalidateAndCancel(); ReceiptHTTPProtocol.fixture = nil }
    private func request(_ path: String, method: String = "GET", bearer: String = "fixture-a") -> URLRequest {
        var request = URLRequest(url: URL(string: path.hasPrefix("https:") ? path : "https://a.test/api/v1/" + path)!)
        request.httpMethod = method
        request.setValue("Bearer \(bearer)", forHTTPHeaderField: "Authorization")
        return request
    }
    private func establish() async throws {
        fixture.enqueue(.ok(204, "18446744073709551615"))
        let (_, response) = try await transport.data(for: request("items/7/progress", method: "POST"), using: session)
        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 204)
    }

    func testNoContentWriteEchoesExactFloorAndAbsentWriteClearsIt() async throws {
        try await establish()
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertEqual(fixture.captured[1].value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
        XCTAssertEqual(fixture.captured[1].value(forHTTPHeaderField: "Authorization"), "Bearer fixture-a")
        fixture.enqueue(.ok(204, nil)); _ = try await transport.data(for: request("publication/s1", method: "DELETE"), using: session)
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("items/7"), using: session)
        XCTAssertNil(fixture.captured[3].value(forHTTPHeaderField: "X-Plurx-Read-After"))
    }

    func testOriginalTransportErrorsSurviveAndOnlyWritesRetireTheFloor() async throws {
        try await establish()
        fixture.enqueue(.failure(URLError(.cancelled)))
        do { _ = try await transport.data(for: request("libraries"), using: session); XCTFail("expected read cancellation") }
        catch { XCTAssertEqual((error as? URLError)?.code, .cancelled) }
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertEqual(fixture.captured[2].value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
        fixture.enqueue(.failure(URLError(.timedOut)))
        do { _ = try await transport.data(for: request("items/7/progress", method: "POST"), using: session); XCTFail("expected write failure") }
        catch { XCTAssertEqual((error as? URLError)?.code, .timedOut) }
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertNil(fixture.captured[4].value(forHTTPHeaderField: "X-Plurx-Read-After"))
    }

    func testPublicForeignAndCapturedProfileRequestsNeverBorrowReceipts() async throws {
        try await establish()
        for candidate in [request("server"), request("server/"), request("%73erver"),
                          request("https://b.test/api/v1/libraries"), request("libraries", bearer: "captured-profile")] {
            fixture.enqueue(.ok(200, "7"))
            var supplied = candidate; supplied.setValue("999", forHTTPHeaderField: "X-Plurx-Read-After")
            _ = try await transport.data(for: supplied, using: session)
            XCTAssertNil(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"))
            XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "Authorization"), candidate.value(forHTTPHeaderField: "Authorization"))
        }
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
    }

    func testChangedAuthRejectsInFlightReceiptBeforeTheNextRequest() async throws {
        try await establish()
        fixture.enqueue { [fixture = fixture!] _ in fixture.changeAccount(); return .ok(200, "99") }
        _ = try await transport.data(for: request("libraries"), using: session)
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries", bearer: "fixture-new"), using: session)
        XCTAssertNil(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"))
        XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "Authorization"), "Bearer fixture-new")
    }

    func testRedirectNeverForwardsOrLearnsTheAccountFloor() async throws {
        try await establish()
        fixture.enqueue(.redirect("https://b.test/api/v1/libraries")); fixture.enqueue(.ok(200, "19"))
        _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertEqual(fixture.captured.count, 3)
        XCTAssertNil(fixture.captured[2].value(forHTTPHeaderField: "X-Plurx-Read-After"))
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
        fixture.enqueue(.redirect("https://b.test/api/v1/libraries")); fixture.enqueue(.ok(200, "19"))
        _ = try await transport.data(for: request("items/7/progress", method: "POST"), using: session)
        fixture.enqueue(.ok(200, nil)); _ = try await transport.data(for: request("libraries"), using: session)
        XCTAssertNil(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"))
    }

    func testActualSessionBridgeSharesTheFloorAndFencesChangedCredentials() async throws {
        let saved = Session.shared.credentials
        defer { Session.shared.setCredentials(origin: saved.origin, token: saved.token) }
        Session.shared.setCredentials(origin: "https://a.test", token: "fixture-a")
        var write = request("items/7/progress", method: "POST")
        Session.shared.authorize(&write)
        fixture.enqueue(.ok(204, "18446744073709551615"))
        _ = try await ReadAfterHTTP.shared.data(for: write, using: session)
        var read = request("libraries"); Session.shared.authorize(&read)
        fixture.enqueue(.ok(200, nil)); _ = try await ReadAfterHTTP.shared.data(for: read, using: session)
        XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"), "18446744073709551615")
        Session.shared.setCredentials(origin: "https://a.test", token: "fixture-new")
        fixture.enqueue(.ok(200, "99")); _ = try await ReadAfterHTTP.shared.data(for: read, using: session)
        XCTAssertNil(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"), "captured old bearer must not borrow the new profile floor")
        var fresh = request("libraries"); Session.shared.authorize(&fresh)
        fixture.enqueue(.ok(200, nil)); _ = try await ReadAfterHTTP.shared.data(for: fresh, using: session)
        XCTAssertNil(fixture.captured.last?.value(forHTTPHeaderField: "X-Plurx-Read-After"))
        XCTAssertEqual(fixture.captured.last?.value(forHTTPHeaderField: "Authorization"), "Bearer fixture-new")
    }
}
