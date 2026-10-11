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
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/\(String(repeating: "L", count: 236))" }
    override func tearDown() { FileContextHTTP.beforeResponse = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func fetch(_ base: String?, mutate: (inout [String: Any]) -> Void = { _ in },
                       detailBody: (Data) throws -> Data = { $0 }) async throws -> PlaybackFileContext {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let reference = try JSONSerialization.jsonObject(with: encoder.encode(ref))
        var row: [String: Any] = ["file_id": file, "revision": revision,
                                  "reference": ["item": reference, "file_id": file, "revision": revision, "lifecycle_generation": 1]]
        row["file_base"] = base
        mutate(&row)
        FileContextHTTP.body = try detailBody(JSONSerialization.data(withJSONObject: ["files": [row], "lifecycle_generation": 1]))
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
    func testSharedFullQueryVocabularyAndTranslatedMediaCannotAcquireAuthority() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        XCTAssertThrowsError(try PlaybackFileContext.localCall(7, context: context))
        let bound = try context.withSession("44444444-4444-4444-8444-444444444444")
        let fields = ["client": "android", "device": "Native client", "profile": "android-directplay-any",
                      "vcodec": "hevc,h264", "vmaxheight": "hevc:2160,h264:1080", "acodec": "aac,eac3",
                      "container": "mkv,mp4", "maxheight": "2160", "hdr": "1", "dv": "1", "dvhls": "1",
                      "hdr10t": "1", "dvprofile": "5,8", "start": "12.5", "audio": "2", "audio_offset_ms": "-100"]
        let path = try bound.path("stream.mp4", query: fields.map { URLQueryItem(name: $0.key, value: $0.value) })
        let parameters = URLComponents(string: path)!.queryItems!
        for (key, value) in fields { XCTAssertEqual(parameters.first { $0.name == key }?.value, value) }
        XCTAssertEqual(try bound.translatedDeliveryPath(path), path)
        for value in ["https://a.test" + path, "/api/v1/files/7/direct", base + "/direct?token=x", base + "/direct?session=55555555-5555-4555-8555-555555555555", base + "/stream.mp4?achannels=6", base + "/stream.mp4?audio=1&audio=2"] {
            XCTAssertThrowsError(try bound.translatedDeliveryPath(value))
        }
        XCTAssertThrowsError(try bound.path("decision", query: [URLQueryItem(name: "upstreamURL", value: "https://a.test")]))
    }
    func testActualLocalAPICallersPreserveExactIDsAndRejectSharedFallback() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let shared = try await fetch(base)
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [FileContextHTTP.self]
        let api = PlurxAPI(origin: "https://b.test", testTransport: URLSession(configuration: config))
        let id = 9_007_199_254_740_993
        let context = try PlaybackFileContext.local(id)
        do { _ = try await api.pgsOverlayManifest(fileId: id, trackIndex: 2, fileContext: context) } catch {}
        XCTAssertEqual(FileContextHTTP.lastRequest?.url?.path, "/api/v1/files/9007199254740993/subs/2/overlay.json")
        do { _ = try await api.createHlsSession(fileId: id, body: CreateSessionRequest(playbackId: "44444444-4444-4444-8444-444444444444"), fileContext: context) } catch {}
        XCTAssertEqual(FileContextHTTP.lastRequest?.url?.path, "/api/v1/files/9007199254740993/hls/sessions")
        FileContextHTTP.lastRequest = nil
        do { _ = try await api.pgsOverlayManifest(fileId: id, trackIndex: 2, fileContext: shared); XCTFail("Shared became Local") } catch {}
        XCTAssertNil(FileContextHTTP.lastRequest)
    }
    func testAuthenticatedDetailHonorsFourMiBBoundWithoutGrantingMissingLocator() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        // Three valid file inventories with the Source's bounded chapter vocabulary exceed 1 MiB.
        let largeDetail: (Data) throws -> Data = { data in
            let original = try JSONSerialization.jsonObject(with: data) as! [String: Any]
            let template = (original["files"] as! [[String: Any]])[0]
            let chapters: [[String: Any]] = (0..<1024).map {
                ["index": $0, "title": String(repeating: "c", count: 512), "start_ms": $0 * 1000, "end_ms": ($0 + 1) * 1000]
            }
            let rows = (0..<3).map { index -> [String: Any] in
                var row = template
                let id = index == 0 ? self.file : String(index)
                row["file_id"] = id; row["size"] = "1048576"; row["chapters"] = chapters
                row["audio_streams"] = []; row["subtitle_streams"] = []; row["skip_regions"] = []
                row["dolby_vision"] = [:]; row["audio_offset_ms"] = 0; row["probed"] = true
                var binding = row["reference"] as! [String: Any]; binding["file_id"] = id; row["reference"] = binding
                return row
            }
            var result = try JSONSerialization.data(withJSONObject: ["files": rows, "lifecycle_generation": 1])
            XCTAssertGreaterThan(result.count, 1_048_576); XCTAssertLessThan(result.count, 4_194_304)
            // JSON whitespace makes the transport byte boundary exact without inventing wire fields.
            result.append(Data(repeating: 32, count: 4_194_304 - result.count))
            return result
        }
        let context = try await fetch(base, detailBody: largeDetail)
        XCTAssertEqual(context.reference, ref); XCTAssertEqual(context.sourceFileId, file)
        XCTAssertNil(context.sessionId); XCTAssertThrowsError(try context.path("direct"))
        do { _ = try await fetch(base, detailBody: { data in var result = try largeDetail(data); result.append(32); return result }); XCTFail("accepted detail over 4 MiB") } catch {}
        do { _ = try await fetch(nil, detailBody: largeDetail); XCTFail("large detail supplied missing delivery authority") } catch {}
    }
    func testLocalExactIDsAndClosedResources() throws {
        XCTAssertEqual(try PlaybackFileContext.local(Int64.max.description).path("direct"), "/api/v1/files/9223372036854775807/direct")
        for id in ["01", "-1", "9223372036854775808", "1.0", "1/2"] { XCTAssertThrowsError(try PlaybackFileContext.local(id)) }
        let decoded = try JSONDecoder().decode(MediaFile.self, from: Data("{\"id\":9007199254740993}".utf8))
        XCTAssertEqual(try PlaybackFileContext.local(decoded.id).path("direct"), "/api/v1/files/9007199254740993/direct")
        XCTAssertThrowsError(try PlaybackFileContext.localCall(7, context: PlaybackFileContext.local(decoded.id)))
        let context = try PlaybackFileContext.local(7)
        for resource in ["../direct", "direct?token=x", "//a.test", "subs/1/../2", "hls/foreign/status"] {
            XCTAssertThrowsError(try context.path(resource))
        }
    }
}
