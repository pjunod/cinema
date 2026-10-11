import Foundation
import XCTest
@testable import plurx

private final class SharedBrowseHTTP: URLProtocol {
    static var respond: ((URLRequest) throws -> (Int, [String: Any]))!
    static var requests: [URLRequest] = []
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            Self.requests.append(request)
            let (status, object) = try Self.respond(request)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: try JSONSerialization.data(withJSONObject: object))
            client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

final class SharedLibraryTests: XCTestCase {
    private let first = SharedLibraryIdentity(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222",
        catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "9007199254740993")
    private let other = SharedLibraryIdentity(importId: "44444444-4444-4444-8444-444444444444", serverId: "55555555-5555-4555-8555-555555555555",
        catalogueEpoch: "66666666-6666-4666-8666-666666666666", libraryId: "9007199254740993")
    private let item = "9223372036854775807"
    private let revision = String(repeating: "a", count: 64)
    override func setUp() { SharedBrowseHTTP.requests = []; Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer") }
    override func tearDown() { SharedBrowseHTTP.respond = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func client() throws -> SharedLibraryClient {
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [SharedBrowseHTTP.self]
        return try SharedLibraryClient(testTransport: URLSession(configuration: config))
    }
    private func object<T: Encodable>(_ value: T) throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return try JSONSerialization.jsonObject(with: encoder.encode(value)) as! [String: Any]
    }
    private func assignment(_ source: SharedLibraryIdentity) throws -> [String: Any] {
        var value = try object(source); value["source_name"] = source == first ? "Source A" : "Source C"; value["availability"] = "unverified"; return value
    }
    private func wireItem(_ source: SharedLibraryIdentity, id: String? = nil) throws -> [String: Any] {
        ["source": "shared", "reference": try object(source.reference(id ?? item)), "title": "Distinct Source title", "kind": "movie", "genres": []]
    }
    private func page(_ source: SharedLibraryIdentity, cursor: String?, ids: [String]) throws -> SharedLibraryPage {
        let wire: [String: Any] = ["items": try ids.map { try wireItem(source, id: $0) }, "next_cursor": cursor as Any? ?? NSNull(),
            "catalogue_revision": 2, "scope_generation": 3, "catalogue_generation": 4]
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(SharedLibraryPage.self, from: JSONSerialization.data(withJSONObject: wire))
    }
    private func detail(_ source: SharedLibraryIdentity) throws -> [String: Any] {
        ["item": try wireItem(source), "files": [["file_id": "9007199254740993", "revision": revision, "size": "9007199254740993",
            "reference": ["item": try object(source.reference(item)), "file_id": "9007199254740993", "revision": revision], "container": "mkv", "video_codec": "hevc"]],
         "watch": ["position_ms": 12000, "watched": false, "sequence": 1, "updated_at_ms": 1], "delivery_status": "unavailable"]
    }
    func testActualBOnlyBrowsePreservesPreciseIdsAndPerSourceFailure() async throws {
        SharedBrowseHTTP.respond = { request in
            XCTAssertEqual(request.url?.host, "b.test")
            XCTAssertEqual(request.value(forHTTPHeaderField: "Authorization"), "Bearer fixture-bearer")
            if request.url!.path == "/api/v1/shared/libraries" { return (200, ["libraries": [try self.assignment(self.first), try self.assignment(self.other)]]) }
            if request.url!.path.contains(self.first.importId) { return (503, ["code": "sharing_source_unavailable", "message": "Source unavailable"]) }
            return (200, ["import_id": self.other.importId, "server_id": self.other.serverId, "catalogue_epoch": self.other.catalogueEpoch,
                "libraries": [["library_id": self.other.libraryId, "name": "Remote movies", "kind": "movie", "anime": false], ["library_id": "7", "name": "Unassigned", "kind": "movie", "anime": false]]])
        }
        let api = try client(); let assigned = try await api.assignments()
        XCTAssertEqual(assigned[0].libraryId, first.libraryId); XCTAssertNotEqual(assigned[0].id, assigned[1].id)
        do { _ = try await api.libraries([assigned[0]]); XCTFail("offline Source accepted") }
        catch { XCTAssertEqual((error as? APIError)?.refusalCode, "sharing_source_unavailable") }
        let working = try await api.libraries([assigned[1]])
        XCTAssertEqual(working.count, 1); XCTAssertEqual(working[0].identity, other)
        XCTAssertTrue(SharedBrowseHTTP.requests.allSatisfy { $0.url!.path.hasPrefix("/api/v1/shared/") })
    }
    func testCursorAdvancesAcrossDuplicatePageAndRejectsCrossSourceCollision() throws {
        var browse = SharedBrowseAccumulator()
        try browse.append(page(first, cursor: "opaque-1", ids: [item]), requestedCursor: nil, library: first)
        try browse.append(page(first, cursor: "opaque-2", ids: [item]), requestedCursor: "opaque-1", library: first)
        XCTAssertEqual(browse.items.count, 1); XCTAssertEqual(browse.nextCursor, "opaque-2")
        try browse.append(page(first, cursor: nil, ids: ["9007199254740993"]), requestedCursor: "opaque-2", library: first)
        XCTAssertEqual(browse.items.count, 2)
        XCTAssertNotEqual(try page(first, cursor: nil, ids: [item]).items[0].id, try page(other, cursor: nil, ids: [item]).items[0].id)
        XCTAssertThrowsError(try browse.append(page(other, cursor: nil, ids: [item]), requestedCursor: nil, library: first))
    }
    func testActualDetailHasDistinctStringFilesAndNoLocalDeliveryFallback() async throws {
        SharedBrowseHTTP.respond = { request in
            XCTAssertEqual(request.url?.path, "/api/v1/shared/imports/\(self.first.importId)/items/\(self.item)")
            return (200, try self.detail(self.first))
        }
        let api = try client(); let result = try await api.detail(first.reference(item))
        XCTAssertEqual(result.files[0].fileId, "9007199254740993"); XCTAssertEqual(result.item.reference.itemId, item)
        XCTAssertEqual(result.deliveryStatus, "unavailable"); XCTAssertEqual(result.watch?.positionMs, 12000)
        SharedBrowseHTTP.respond = { _ in (200, try self.detail(self.other)) }
        do { _ = try await api.detail(first.reference(item)); XCTFail("different Source accepted") } catch {}
        SharedBrowseHTTP.respond = { _ in
            var detail = try self.detail(self.first); var files = detail["files"] as! [[String: Any]]
            files[0]["file_id"] = 9007199254740993 as Int64; detail["files"] = files; return (200, detail)
        }
        do { _ = try await api.detail(first.reference(item)); XCTFail("numeric Source ID accepted") } catch {}
    }
    func testActualPageRouteQueryAndAccountChangeFence() async throws {
        SharedBrowseHTTP.respond = { request in
            XCTAssertEqual(request.url?.path, "/api/v1/shared/imports/\(self.first.importId)/libraries/\(self.first.libraryId)/items")
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!
            XCTAssertEqual(query.first { $0.name == "q" }?.value, "A+B & 雪/?")
            XCTAssertEqual(query.first { $0.name == "cursor" }?.value, "opaque+=/")
            XCTAssertTrue(URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.percentEncodedQuery!.contains("opaque%2B"))
            return (200, ["items": [try self.wireItem(self.first)], "catalogue_revision": 1, "scope_generation": 1, "catalogue_generation": 1])
        }
        let api = try client(); let result = try await api.page(first, q: "A+B & 雪/?", cursor: "opaque+=/")
        XCTAssertEqual(result.items[0].reference.itemId, item)
        SharedBrowseHTTP.respond = { _ in
            Session.shared.setCredentials(origin: "https://b.test", token: "other-account")
            return (200, ["libraries": [try self.assignment(self.first)]])
        }
        do { _ = try await api.assignments(); XCTFail("changed account accepted") } catch {}
    }
    func testSavedChoiceAndPendingUserEditSurviveUnavailableReadiness() async throws {
        SharedBrowseHTTP.respond = { request in
            if request.url!.path == "/api/v1/sharing/status" { return (503, ["code": "sharing_authority_unavailable", "message": "Unknown readiness"]) }
            if request.httpMethod == "PUT" {
                let bytes: Data
                if let body = request.httpBody { bytes = body }
                else {
                    let stream = request.httpBodyStream!; stream.open(); defer { stream.close() }
                    var data = Data(); var buffer = [UInt8](repeating: 0, count: 1024)
                    while stream.hasBytesAvailable { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; data.append(contentsOf: buffer.prefix(count)) }
                    bytes = data
                }
                let body = try JSONSerialization.jsonObject(with: bytes) as! [String: Any]
                XCTAssertEqual(body["enabled"] as? Bool, false); return (200, ["enabled": false])
            }
            return (200, ["enabled": true])
        }
        let api = try client(); var draft = SharedSharingDraft()
        draft.received(try await api.settings(), requestedAt: 0); XCTAssertTrue(draft.enabled)
        do { _ = try await api.management("status"); XCTFail("expected readiness failure") } catch {}
        XCTAssertTrue(draft.enabled)
        draft.choose(false); let requestRevision = draft.revision
        let saved = try await api.save(enabled: draft.enabled); XCTAssertFalse(saved)
        draft.choose(true); draft.received(false, requestedAt: requestRevision)
        XCTAssertTrue(draft.enabled, "in-flight acknowledgement overwrote user's newer choice")
    }
    func testSharedContinueWatchingKeepsFullSourceIdentityOrderAndRefusesOfflineMetadata() async throws {
        func group(_ source: SharedLibraryIdentity) -> [String: Any] {
            ["import_id": source.importId, "server_id": source.serverId, "catalogue_epoch": source.catalogueEpoch, "source_name": "Configured Source", "count": 2]
        }
        var offline = false
        SharedBrowseHTTP.respond = { request in
            if request.url!.path == "/api/v1/shared/libraries" { return (200, ["libraries": [try self.assignment(self.first), try self.assignment(self.other)]]) }
            if request.url!.path == "/api/v1/shared/continue-watching" { return (200, ["groups": [group(self.first), group(self.other)]]) }
            let source = request.url!.path.contains(self.first.importId) ? self.first : self.other
            var reply = group(source)
            reply["availability"] = offline ? "unavailable" : "online"
            reply["items"] = try [self.item, "9007199254740993"].map { id in
                ["item": try self.wireItem(source, id: id), "watch": ["position_ms": 12000, "duration_ms": 90000, "watched": false, "sequence": 7, "updated_at_ms": 8]] as [String: Any]
            }
            return (200, reply)
        }
        let api = try client(), assigned = try await api.assignments(), groups = try await api.continueGroups()
        let a = try await api.continueItems(groups[0], assigned: assigned)
        let c = try await api.continueItems(groups[1], assigned: assigned)
        XCTAssertEqual(a.items.map { $0.item.reference.itemId }, [item, "9007199254740993"])
        XCTAssertNotEqual(a.items[0].id, c.items[0].id)
        do { _ = try await api.continueItems(groups[0], assigned: [assigned[1]]); XCTFail("foreign assignment accepted") } catch {}
        offline = true
        do { _ = try await api.continueItems(groups[0], assigned: assigned); XCTFail("offline stale metadata accepted") } catch {}
    }

}
