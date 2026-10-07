import Foundation
import XCTest
@testable import plurx

private final class CatalogueHTTP: URLProtocol {
    static var respond: ((URLRequest, Data) throws -> (Int, Any))!
    static var requests: [URLRequest] = []
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            Self.requests.append(request)
            var body = request.httpBody ?? Data()
            if body.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 4096)
                while true { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; body.append(contentsOf: buffer.prefix(count)) }
            }
            let (status, object) = try Self.respond(request, body)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: object))
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

/// Synthetic B catalogue answers through actual authenticated URLProtocol I/O:
/// manual watched state (§5.3) and next-episode resolution in Source order.
final class SharedCatalogueActionsTests: XCTestCase {
    private let library = SharedLibraryIdentity(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222",
        catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "9007199254740993")
    override func setUp() { CatalogueHTTP.requests = []; Session.shared.setCredentials(origin: "https://b.test", token: "catalogue-bearer") }
    override func tearDown() { CatalogueHTTP.respond = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func client() throws -> SharedLibraryClient {
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [CatalogueHTTP.self]
        return try SharedLibraryClient(testTransport: URLSession(configuration: config))
    }
    private func object<T: Encodable>(_ value: T) throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return try JSONSerialization.jsonObject(with: encoder.encode(value)) as! [String: Any]
    }
    private func item(_ id: String, kind: String, parent: String?, in source: SharedLibraryIdentity? = nil) throws -> [String: Any] {
        let source = source ?? library
        var value: [String: Any] = ["source": "shared", "reference": try object(source.reference(id)), "title": "Title \(id)", "kind": kind, "genres": []]
        if let parent { value["parent"] = try object(source.reference(parent)) }
        return value
    }
    private func page(_ items: [[String: Any]], next: String? = nil) -> [String: Any] {
        ["items": items, "next_cursor": next as Any? ?? NSNull(), "catalogue_revision": 1, "scope_generation": 1, "catalogue_generation": 1]
    }

    func testManualWatchedPostsOnlyTheBPrivateRouteAndBindsTheAnswer() async throws {
        let reference = library.reference("9223372036854775807")
        var sent: [String: Any]?
        CatalogueHTTP.respond = { request, body in
            XCTAssertEqual(request.httpMethod, "POST")
            XCTAssertEqual(request.url?.host, "b.test")
            XCTAssertEqual(request.url?.path, "/api/v1/shared/imports/\(reference.importId)/items/\(reference.itemId)/watched")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer catalogue-bearer")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")
            sent = try JSONSerialization.jsonObject(with: body) as? [String: Any]
            let watched = sent?["watched"] as? Bool ?? false
            return (200, ["updated": 1, "watch": ["position_ms": 0, "duration_ms": 600_000, "watched": watched, "sequence": 9_007_199_254_740_993 as Int64, "updated_at_ms": 5]])
        }
        let api = try client()
        let unwatched = try await api.setWatched(reference, watched: false)
        XCTAssertEqual(sent?.keys.sorted(), ["watched"]); XCTAssertEqual(sent?["watched"] as? Bool, false)
        XCTAssertFalse(unwatched.watched); XCTAssertEqual(unwatched.sequence, 9_007_199_254_740_993)
        let watched = try await api.setWatched(reference, watched: true)
        XCTAssertTrue(watched.watched)
        // An answer for the other state, a refusal or a malformed watch is not adopted.
        CatalogueHTTP.respond = { _, _ in (200, ["updated": 1, "watch": ["position_ms": 0, "watched": true, "sequence": 1, "updated_at_ms": 1]]) }
        do { _ = try await api.setWatched(reference, watched: false); XCTFail("adopted the opposite watched state") } catch {}
        CatalogueHTTP.respond = { _, _ in (200, ["updated": 1, "watch": ["position_ms": -1, "watched": false, "sequence": 1, "updated_at_ms": 1]]) }
        do { _ = try await api.setWatched(reference, watched: false); XCTFail("adopted a negative position") } catch {}
        CatalogueHTTP.respond = { _, _ in (409, ["code": "sharing_watch_unsupported", "message": "Not watchable"]) }
        do { _ = try await api.setWatched(reference, watched: true); XCTFail("accepted a refusal") }
        catch { XCTAssertEqual((error as? APIError)?.refusalCode, "sharing_watch_unsupported") }
        XCTAssertTrue(CatalogueHTTP.requests.allSatisfy { $0.url!.path.hasPrefix("/api/v1/shared/imports/") })
        Session.shared.setCredentials(origin: "https://b.test", token: "replacement")
        CatalogueHTTP.requests = []
        do { _ = try await api.setWatched(reference, watched: true); XCTFail("posted under a replaced login") } catch {}
        XCTAssertTrue(CatalogueHTTP.requests.isEmpty)
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        for (kind, applies) in [("movie", true), ("episode", true), ("series", false), ("season", false), ("album", false)] {
            let wire = try JSONSerialization.data(withJSONObject: try item("5", kind: kind, parent: nil))
            XCTAssertEqual(SharedWatchedAction.applies(to: try decoder.decode(SharedLibraryItem.self, from: wire)), applies, kind)
        }
        XCTAssertEqual(SharedWatchedAction.title(watched: true), "Mark unwatched")
        XCTAssertEqual(SharedWatchedAction.title(watched: false), "Mark watched")
    }

    func testNextEpisodeFollowsSourceOrderThroughBAndNeverLeavesTheLibrary() async throws {
        // Series 100: season 201 holds 301, 302; season 202 holds 9007199254740993.
        let huge = "9007199254740993"
        let details: [String: [String: Any]] = [
            "100": try item("100", kind: "series", parent: nil),
            "201": try item("201", kind: "season", parent: "100"), "202": try item("202", kind: "season", parent: "100"),
            "301": try item("301", kind: "episode", parent: "201"), "302": try item("302", kind: "episode", parent: "201"),
            huge: try item(huge, kind: "episode", parent: "202"), "500": try item("500", kind: "movie", parent: nil),
        ]
        let children: [String: [[String: Any]]] = [
            "100": [try item("201", kind: "season", parent: "100"), try item("202", kind: "season", parent: "100")],
            "201": [try item("301", kind: "episode", parent: "201"), try item("302", kind: "episode", parent: "201")],
            "202": [try item(huge, kind: "episode", parent: "202")],
        ]
        var foreignChild = false
        CatalogueHTTP.respond = { request, _ in
            XCTAssertEqual(request.httpMethod ?? "GET", "GET")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer catalogue-bearer")
            let parts = request.url!.path.split(separator: "/").map(String.init)
            XCTAssertEqual(Array(parts.prefix(6)), ["api", "v1", "shared", "imports", self.library.importId, "items"])
            let id = parts[6]
            if parts.count == 8, parts[7] == "children" {
                let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
                XCTAssertEqual(query.first { $0.name == "limit" }?.value, "200")
                if foreignChild {
                    let other = SharedLibraryIdentity(importId: self.library.importId, serverId: self.library.serverId, catalogueEpoch: self.library.catalogueEpoch, libraryId: "7")
                    return (200, self.page([try self.item("999", kind: "episode", parent: id, in: other)]))
                }
                return (200, self.page(children[id] ?? []))
            }
            return (200, ["item": details[id]!, "files": [], "delivery_status": "available", "lifecycle_generation": 1])
        }
        let api = try client()
        let afterFirst = try await api.nextEpisode(after: library.reference("301"))
        XCTAssertEqual(afterFirst, library.reference("302"))
        let rollover = try await api.nextEpisode(after: library.reference("302"))
        XCTAssertEqual(rollover, library.reference(huge))
        let last = try await api.nextEpisode(after: library.reference(huge))
        XCTAssertNil(last)
        let movie = try await api.nextEpisode(after: library.reference("500"))
        XCTAssertNil(movie)
        XCTAssertTrue(CatalogueHTTP.requests.allSatisfy { $0.url!.path.hasPrefix("/api/v1/shared/imports/\(self.library.importId)/items/") })
        foreignChild = true
        do { _ = try await api.nextEpisode(after: library.reference("301")); XCTFail("followed a child from another library") } catch {}
    }

    func testNextEpisodeCursorIsFollowedAndARepeatedCursorRefuses() async throws {
        var calls = 0
        CatalogueHTTP.respond = { request, _ in
            let parts = request.url!.path.split(separator: "/").map(String.init)
            if parts.count == 8 {
                calls += 1
                let cursor = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems?.first { $0.name == "cursor" }?.value
                switch cursor {
                case nil: return (200, self.page([try self.item("301", kind: "episode", parent: "201")], next: "opaque+1"))
                case "opaque+1": return (200, self.page([try self.item("302", kind: "episode", parent: "201")], next: "opaque+1"))
                default: XCTFail("unexpected cursor"); return (500, [:])
                }
            }
            return (200, ["item": try self.item(parts[6], kind: parts[6] == "201" ? "season" : "episode", parent: parts[6] == "201" ? "100" : "201"),
                          "files": [], "delivery_status": "available"])
        }
        do { _ = try await client().nextEpisode(after: library.reference("302")); XCTFail("followed a repeated cursor") } catch {}
        XCTAssertEqual(calls, 2)
    }
    func testMissingCurrentEpisodeNeverSkipsToTheNextSeason() async throws {
        CatalogueHTTP.respond = { request, _ in
            if request.url!.path.hasSuffix("/301") {
                return (200, ["item": try self.item("301", kind: "episode", parent: "201"), "files": [], "delivery_status": "available"])
            }
            XCTAssertTrue(request.url!.path.hasSuffix("/201/children"))
            return (200, self.page([try self.item("302", kind: "episode", parent: "201")]))
        }
        let next = try await client().nextEpisode(after: library.reference("301"))
        XCTAssertNil(next)
        XCTAssertEqual(CatalogueHTTP.requests.count, 2)
    }

}
