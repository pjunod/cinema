import Foundation
import XCTest
@testable import plurx

// The rules are `ReadAfterFloor`'s; the cases mirror tests/web/read-after.test.js
// row for row so the two clients cannot drift apart silently.

// MARK: - Fixtures

private final class ReadAfterTestClock: @unchecked Sendable {
    private let lock = NSLock()
    private var nanoseconds: UInt64 = 1_000_000_000

    var now: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return nanoseconds
    }

    func advance(ms: UInt64) {
        lock.lock()
        nanoseconds += ms * 1_000_000
        lock.unlock()
    }
}

private func makeFloor() -> (ReadAfterFloor, ReadAfterTestClock) {
    let clock = ReadAfterTestClock()
    return (ReadAfterFloor(now: { clock.now }), clock)
}

/// Capture `value` from a reply to a fresh `method` request.
private func capture(_ floor: ReadAfterFloor, _ value: String?, method: String = "PUT") {
    floor.observe(reply: value, method: method, ticket: floor.ticket())
}

/// Serves every request the API seam sends, records it, and answers from a
/// queue. A reply may be held until the test releases it, so a test can make
/// it arrive "late".
private final class ReadAfterURLProtocol: URLProtocol {
    struct Reply {
        var status = 200
        var commitIndex: String?
        /// Decodes as `MutationResponse`, `Hubs` and `ServerInfo` alike.
        var body = Data(#"{"ok":true}"#.utf8)
        var failure: URLError.Code?
        var release: DispatchSemaphore?
    }

    private static let lock = NSLock()
    private static var queued: [Reply] = []
    private static var recorded: [URLRequest] = []

    static func reset(_ replies: [Reply] = []) {
        lock.lock()
        queued = replies
        recorded = []
        lock.unlock()
    }

    static var requests: [URLRequest] {
        lock.lock()
        defer { lock.unlock() }
        return recorded
    }

    private static func next(recording request: URLRequest) -> Reply {
        lock.lock()
        defer { lock.unlock() }
        recorded.append(request)
        return queued.isEmpty ? Reply() : queued.removeFirst()
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        let reply = Self.next(recording: request)
        let deliver = { [self] in
            if let failure = reply.failure {
                client?.urlProtocol(self, didFailWithError: URLError(failure))
                return
            }
            var headers = ["Content-Type": "application/json"]
            if let index = reply.commitIndex { headers[ReadAfterFloor.responseHeader] = index }
            let response = HTTPURLResponse(
                url: request.url!,
                statusCode: reply.status,
                httpVersion: "HTTP/1.1",
                headerFields: headers
            )!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: reply.body)
            client?.urlProtocolDidFinishLoading(self)
        }
        if let release = reply.release {
            DispatchQueue.global().async {
                release.wait()
                deliver()
            }
        } else {
            deliver()
        }
    }

    override func stopLoading() {}
}

private func stubSession() -> URLSession {
    let configuration = URLSessionConfiguration.ephemeral
    configuration.protocolClasses = [ReadAfterURLProtocol.self]
    return URLSession(configuration: configuration)
}

private func sentFloor(_ request: URLRequest) -> String? {
    request.value(forHTTPHeaderField: ReadAfterFloor.requestHeader)
}

private func waitForRequests(_ count: Int) async {
    for _ in 0..<300 where ReadAfterURLProtocol.requests.count < count {
        try? await Task.sleep(nanoseconds: 10_000_000)
    }
}

// MARK: - The contract table, on the type

final class ReadAfterFloorTests: XCTestCase {
    func testCaptureKeepsTheFullU64AndSendsItOnTheNextRequest() {
        let (floor, _) = makeFloor()
        XCTAssertNil(floor.ticket().index)
        capture(floor, "18446744073709551614")
        XCTAssertEqual(floor.ticket().index, 18_446_744_073_709_551_614)
        capture(floor, "18446744073709551615")
        XCTAssertEqual(floor.held, UInt64.max)
    }

    func testTheValueExpiresSixtySecondsAfterItsLastCapture() {
        let (floor, clock) = makeFloor()
        capture(floor, "7")
        clock.advance(ms: 59_999)
        XCTAssertEqual(floor.ticket().index, 7)
        clock.advance(ms: 2)
        XCTAssertNil(floor.ticket().index)
    }

    func testAnEqualValueRestartsTheExpiry() {
        let (floor, clock) = makeFloor()
        capture(floor, "42")
        clock.advance(ms: 50_000)
        capture(floor, "42", method: "GET")
        clock.advance(ms: 50_000)
        XCTAssertEqual(floor.ticket().index, 42)
        clock.advance(ms: 10_001)
        XCTAssertNil(floor.ticket().index)
    }

    func testOutOfOrderIndexedRepliesNeverLowerTheFloor() {
        let (floor, clock) = makeFloor()
        let first = floor.ticket()
        let second = floor.ticket()
        floor.observe(reply: "42", method: "PUT", ticket: second)
        clock.advance(ms: 30_000)
        floor.observe(reply: "41", method: "PUT", ticket: first)
        XCTAssertEqual(floor.ticket().index, 42)
        // The smaller reply did not refresh the expiry either.
        clock.advance(ms: 30_001)
        XCTAssertNil(floor.ticket().index)
    }

    func testUnknownInvalidatesReceiptsFromWritesAlreadyInFlight() {
        let (floor, _) = makeFloor()
        let first = floor.ticket()
        let second = floor.ticket()
        floor.observe(reply: "unknown", method: "PUT", ticket: first)
        floor.observe(reply: "43", method: "PUT", ticket: second)
        XCTAssertNil(floor.ticket().index)
        capture(floor, "44")
        XCTAssertEqual(floor.ticket().index, 44)
    }

    func testAnUnindexedMutationReplyForgetsAndAnUnindexedReadDoesNot() {
        for method in ["PUT", "POST", "DELETE", "PATCH", "post"] {
            let (floor, _) = makeFloor()
            capture(floor, "10")
            capture(floor, nil, method: "GET")
            capture(floor, nil, method: "HEAD")
            XCTAssertEqual(floor.held, 10, method)
            capture(floor, nil, method: method)
            XCTAssertNil(floor.held, method)
        }
    }

    func testAMutationTransportErrorForgetsAndAReadTransportErrorDoesNot() {
        let (floor, _) = makeFloor()
        capture(floor, "10")
        floor.transportFailed(method: "GET", ticket: floor.ticket())
        XCTAssertEqual(floor.held, 10)
        floor.transportFailed(method: "POST", ticket: floor.ticket())
        XCTAssertNil(floor.held)
    }

    /// The row a port most easily drops: clearing without bumping the epoch
    /// would accept the late, older reply and then read stale from a replica.
    func testEveryForgetBumpsTheEpoch() {
        let forgets: [(String, (ReadAfterFloor, ReadAfterFloor.Ticket) -> Void)] = [
            ("unindexed mutation", { $0.observe(reply: nil, method: "POST", ticket: $1) }),
            ("mutation transport error", { $0.transportFailed(method: "PUT", ticket: $1) }),
            ("unknown", { $0.observe(reply: "unknown", method: "GET", ticket: $1) }),
            ("malformed", { $0.observe(reply: "01", method: "GET", ticket: $1) }),
            ("explicit", { $0.forget($1) }),
        ]
        for (name, forget) in forgets {
            let (floor, _) = makeFloor()
            capture(floor, "10")
            let inFlight = floor.ticket()
            forget(floor, floor.ticket())
            XCTAssertNil(floor.held, name)
            floor.observe(reply: "50", method: "PUT", ticket: inFlight)
            XCTAssertNil(floor.held, "\(name): a reply sent before the forget was accepted")
        }
    }

    func testMalformedIsCheckedBeforeTheEpoch() {
        let (floor, _) = makeFloor()
        let stale = floor.ticket()
        capture(floor, "unknown")
        capture(floor, "20")
        XCTAssertEqual(floor.held, 20)
        // A valid index from the stale epoch is ignored ...
        floor.observe(reply: "30", method: "PUT", ticket: stale)
        XCTAssertEqual(floor.held, 20)
        // ... but a malformed one from it still forgets.
        floor.observe(reply: "garbage", method: "GET", ticket: stale)
        XCTAssertNil(floor.held)
    }

    func testMalformedAndOverflowingReceiptsNeverBecomeReadHeaders() {
        let malformed = [
            "0", "01", "-1", "18446744073709551616", "99999999999999999999",
            "123456789012345678901", "12, 13", "", " 12", "12 ", "+12", "1e3",
            "unknown", "UNKNOWN", "\u{0663}", "\u{FF11}\u{FF12}",
        ]
        for value in malformed {
            let (floor, _) = makeFloor()
            capture(floor, "10")
            capture(floor, value, method: "GET")
            XCTAssertNil(floor.ticket().index, "accepted \(value.debugDescription)")
            XCTAssertNil(ReadAfterFloor.parse(value), value.debugDescription)
        }
        XCTAssertEqual(ReadAfterFloor.parse("1"), 1)
        XCTAssertEqual(ReadAfterFloor.parse("18446744073709551615"), UInt64.max)
    }

    func testAuthenticationChangesDiscardThePriorAccountsFloorAndReplies() {
        let (floor, _) = makeFloor()
        capture(floor, "12")
        let late = floor.ticket()
        floor.authorizationChanged()
        XCTAssertNil(floor.ticket().index)
        floor.observe(reply: "99", method: "PUT", ticket: late)
        XCTAssertNil(floor.held)

        // Nor can the earlier account's replies forget the new one's floor.
        capture(floor, "13")
        floor.observe(reply: nil, method: "POST", ticket: late)
        floor.observe(reply: "unknown", method: "PUT", ticket: late)
        floor.transportFailed(method: "DELETE", ticket: late)
        floor.forget(late)
        XCTAssertEqual(floor.held, 13)
    }

    func testTheAuthChangeAlsoBumpsTheEpochWithinTheNewGeneration() {
        let (floor, _) = makeFloor()
        let before = floor.ticket()
        floor.authorizationChanged()
        let after = floor.ticket()
        XCTAssertNotEqual(before.generation, after.generation)
        XCTAssertNotEqual(before.epoch, after.epoch)
    }

    func testSessionCredentialChangesForgetTheSharedFloor() {
        let saved = Session.shared.credentials
        defer { Session.shared.setCredentials(origin: saved.origin, token: saved.token) }
        Session.shared.setCredentials(origin: "http://server-a", token: "one")
        let floor = Session.shared.readAfter
        capture(floor, "12")
        // Re-installing the same credential is not an account change.
        Session.shared.setCredentials(origin: "http://server-a", token: "one")
        XCTAssertEqual(floor.held, 12)
        let late = floor.ticket()
        Session.shared.setCredentials(origin: "http://server-a", token: nil)   // sign-out
        XCTAssertNil(floor.held)
        floor.observe(reply: "99", method: "PUT", ticket: late)
        XCTAssertNil(floor.held)
        Session.shared.setCredentials(origin: "http://server-a", token: "two")  // another account
        capture(floor, "14")
        Session.shared.setCredentials(origin: "http://server-b", token: "two")  // another server
        XCTAssertNil(floor.held)
    }
}

// MARK: - The wiring: only API verbs echo and capture

final class ReadAfterAPITests: XCTestCase {
    private var session: URLSession!

    override func setUp() {
        super.setUp()
        ReadAfterURLProtocol.reset()
        session = stubSession()
    }

    override func tearDown() {
        session.invalidateAndCancel()
        session = nil
        ReadAfterURLProtocol.reset()
        super.tearDown()
    }

    private func api(_ floor: ReadAfterFloor, origin: String = "http://node-a") -> PlurxAPI {
        PlurxAPI(origin: origin, transport: session, readAfter: floor)
    }

    func testTheFloorSurvivesNodeChangesAndExpires() async throws {
        let (floor, clock) = makeFloor()
        ReadAfterURLProtocol.reset([.init(commitIndex: "18446744073709551614")])
        _ = try await api(floor).setWatched(itemId: 1, watched: true)
        _ = try await api(floor, origin: "http://node-b").hubs()
        clock.advance(ms: 60_001)
        _ = try await api(floor, origin: "http://node-b").hubs()
        let sent = ReadAfterURLProtocol.requests.map(sentFloor)
        XCTAssertEqual(sent, [nil, "18446744073709551614", nil])
    }

    func testEveryVerbEchoesTheHeldValue() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "5")
        let api = api(floor)
        ReadAfterURLProtocol.reset([
            .init(commitIndex: "5"), .init(commitIndex: "5"), .init(commitIndex: "5"),
            .init(commitIndex: "5"), .init(commitIndex: "5"), .init(commitIndex: "5"),
        ])
        _ = try await api.hubs()                                              // get
        _ = try await api.setWatched(itemId: 1, watched: false)               // post, no body
        try await api.setLibraryChannelFavourite("c", favourite: true)        // putNoContent
        try await api.deleteOfflinePackage("p")                               // deleteNoContent
        try await api.completeOfflinePackage("p")                             // postNoContent
        try await api.progress(itemId: 1, positionMs: 1_000, durationMs: 2_000) // postNoContent(body)
        let requests = ReadAfterURLProtocol.requests
        XCTAssertEqual(requests.map { $0.httpMethod }, ["GET", "POST", "PUT", "DELETE", "POST", "POST"])
        XCTAssertEqual(requests.map(sentFloor), Array(repeating: "5", count: 6))
        XCTAssertEqual(floor.held, 5)
    }

    func testAProgressPostReplyIsCaptured() async throws {
        let (floor, _) = makeFloor()
        ReadAfterURLProtocol.reset([.init(status: 204, commitIndex: "31", body: Data())])
        try await api(floor).progress(itemId: 9, positionMs: 1, durationMs: nil)
        XCTAssertEqual(floor.held, 31)
    }

    func testUnindexedAndFailedNoContentWritesDiscardAnOlderFloor() async throws {
        for failure in [false, true] {
            let (floor, _) = makeFloor()
            capture(floor, "10")
            let inFlight = floor.ticket()
            ReadAfterURLProtocol.reset(failure ? [.init(failure: .networkConnectionLost)] : [])
            if failure {
                do {
                    try await api(floor).progress(itemId: 1, positionMs: 1, durationMs: nil)
                    XCTFail("the transport error was swallowed")
                } catch let error as URLError {
                    // The *NoContent helpers still throw the raw error.
                    XCTAssertEqual(error.code, .networkConnectionLost)
                }
            } else {
                try await api(floor).completeOfflinePackage("p")
            }
            XCTAssertNil(floor.held, "failure=\(failure)")
            floor.observe(reply: "11", method: "PUT", ticket: inFlight)
            XCTAssertNil(floor.held, "failure=\(failure): the forget did not bump the epoch")
        }
    }

    func testADecodedVerbTransportErrorForgetsAndStillMapsTheError() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "10")
        ReadAfterURLProtocol.reset([.init(failure: .timedOut)])
        do {
            _ = try await api(floor).setWatched(itemId: 1, watched: true)
            XCTFail("expected a transport error")
        } catch APIError.transport(_) {}
        XCTAssertNil(floor.held)

        capture(floor, "12")
        ReadAfterURLProtocol.reset([.init(failure: .timedOut)])
        _ = try? await api(floor).hubs()
        XCTAssertEqual(floor.held, 12, "a failed read must not forget")
    }

    func testARefusedReplyIsStillObserved() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "10")
        ReadAfterURLProtocol.reset([.init(status: 500)])
        _ = try? await api(floor).setWatched(itemId: 1, watched: true)
        XCTAssertNil(floor.held)
    }

    func testAReplyAfterAnAccountChangeIsIgnored() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "12")
        let release = DispatchSemaphore(value: 0)
        ReadAfterURLProtocol.reset([.init(commitIndex: "99", release: release)])
        let api = api(floor)
        let late = Task { try await api.setWatched(itemId: 1, watched: true) }
        await waitForRequests(1)
        XCTAssertEqual(ReadAfterURLProtocol.requests.first.flatMap(sentFloor), "12")
        floor.authorizationChanged()
        release.signal()
        _ = try await late.value
        XCTAssertNil(floor.held)
    }

    func testTheUnverifiedServerProbeNeitherEchoesNorCaptures() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "10")
        ReadAfterURLProtocol.reset([.init(commitIndex: "77")])
        _ = try await api(floor, origin: "http://candidate").serverInfo()
        XCTAssertNil(sentFloor(ReadAfterURLProtocol.requests[0]))
        XCTAssertEqual(floor.held, 10)
    }

    func testLogoutWithAForeignTokenNeitherEchoesNorCaptures() async throws {
        for reply in [ReadAfterURLProtocol.Reply(commitIndex: "99"), .init(), .init(commitIndex: "unknown")] {
            let (floor, _) = makeFloor()
            capture(floor, "10")
            ReadAfterURLProtocol.reset([reply])
            try await api(floor).logout(token: "foreign")
            let request = ReadAfterURLProtocol.requests[0]
            XCTAssertNil(sentFloor(request))
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer foreign")
            XCTAssertEqual(floor.held, 10, "logout reply \(String(describing: reply.commitIndex)) moved the floor")
        }
    }

    func testALateLogoutReplyDoesNotChangeTheStoredValue() async throws {
        let (floor, _) = makeFloor()
        capture(floor, "10")
        let release = DispatchSemaphore(value: 0)
        // The logout is held; a watch write lands and raises the floor; then
        // the logout's unindexed POST reply arrives.
        ReadAfterURLProtocol.reset([.init(release: release), .init(commitIndex: "20")])
        let api = api(floor)
        let logout = Task { try await api.logout(token: "captured") }
        await waitForRequests(1)
        _ = try await api.setWatched(itemId: 1, watched: true)
        XCTAssertEqual(floor.held, 20)
        release.signal()
        try await logout.value
        XCTAssertEqual(floor.held, 20)
        XCTAssertNil(sentFloor(ReadAfterURLProtocol.requests[0]))
        XCTAssertEqual(sentFloor(ReadAfterURLProtocol.requests[1]), "10")
    }
}

// MARK: - Requests that are not API verbs carry no floor

final class ReadAfterNonAPIRequestTests: XCTestCase {
    private var saved: (origin: String, token: String?)!
    private var session: URLSession!

    override func setUp() {
        super.setUp()
        saved = Session.shared.credentials
        Session.shared.setCredentials(origin: "http://server", token: "token")
        ReadAfterURLProtocol.reset()
        session = stubSession()
        // Hold a value in the session's own floor, and prove it is held by
        // having a verb on the same floor echo it.
        capture(Session.shared.readAfter, "77")
    }

    override func tearDown() {
        session.invalidateAndCancel()
        session = nil
        ReadAfterURLProtocol.reset()
        Session.shared.setCredentials(origin: saved.origin, token: saved.token)
        super.tearDown()
    }

    private func assertSharedFloorIsHeld(file: StaticString = #filePath, line: UInt = #line) async throws {
        let before = ReadAfterURLProtocol.requests.count
        _ = try await PlurxAPI(origin: "http://server", transport: session, readAfter: Session.shared.readAfter).hubs()
        XCTAssertEqual(sentFloor(ReadAfterURLProtocol.requests[before]), "77", file: file, line: line)
    }

    func testAnImageRequestCarriesNoFloor() async throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("plurx-read-after-image-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let cache = AuthImageCache(
            diskCache: AuthImageDiskCache(directory: directory, byteLimit: 10_000, maximumStaleAge: 60),
            session: session
        )
        ReadAfterURLProtocol.reset([.init(status: 404, commitIndex: "88")])
        _ = await cache.refreshImage(path: "/api/v1/items/1/poster", maxPixelSize: 100, key: "k")
        let images = ReadAfterURLProtocol.requests
        XCTAssertFalse(images.isEmpty)
        for request in images {
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")
            XCTAssertNil(sentFloor(request))
        }
        XCTAssertEqual(Session.shared.readAfter.held, 77, "an image reply was read for a floor")
        try await assertSharedFloorIsHeld()
    }

    func testAnOfflineDownloadRequestCarriesNoFloor() async throws {
        let request = try PlurxAPI(origin: "http://server").bookContentRequest(fileId: 4)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")
        XCTAssertNil(sentFloor(request))
        try await assertSharedFloorIsHeld()
    }

    func testAClientLogPostCarriesNoFloor() async throws {
        let request = try XCTUnwrap(PlayerController.clientLogRequest(body: Data(#"{"ok":true}"#.utf8)))
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.url?.absoluteString, "http://server/api/v1/client-log")
        XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer token")
        XCTAssertNil(sentFloor(request))
        try await assertSharedFloorIsHeld()
    }

    func testSessionAuthorizeNeverAddsTheFloor() async throws {
        var request = URLRequest(url: URL(string: "http://server/api/v1/hls/s/index.m3u8")!)
        Session.shared.authorize(&request)
        XCTAssertNil(sentFloor(request))
        try await assertSharedFloorIsHeld()
    }
}
