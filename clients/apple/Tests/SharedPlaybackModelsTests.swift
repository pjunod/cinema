import Foundation
import XCTest
@testable import plurx

private final class SharedWireHTTP: URLProtocol {
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

final class SharedPlaybackModelsTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111",
        serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333",
        libraryId: "9007199254740993", itemId: "9223372036854775807")
    private let revision = String(repeating: "a", count: 64)
    private let file = "9007199254740993"
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/\(String(repeating: "L", count: 236))" }
    override func tearDown() { SharedWireHTTP.beforeResponse = nil; Session.shared.setCredentials(origin: "", token: nil) }
    private func fetch(_ base: String?, mutate: (inout [String: Any]) -> Void = { _ in }) async throws -> PlaybackFileContext {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        let reference = try JSONSerialization.jsonObject(with: encoder.encode(ref))
        var row: [String: Any] = ["file_id": file, "revision": revision,
                                  "reference": ["item": reference, "file_id": file, "revision": revision, "lifecycle_generation": 1]]
        row["file_base"] = base
        mutate(&row)
        SharedWireHTTP.body = try JSONSerialization.data(withJSONObject: ["files": [row], "lifecycle_generation": 1])
        let config = URLSessionConfiguration.ephemeral; config.protocolClasses = [SharedWireHTTP.self]
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: file,
                                                                 testTransport: URLSession(configuration: config))
    }
    private func binding() throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return ["item": try JSONSerialization.jsonObject(with: encoder.encode(ref)), "file_id": file, "revision": revision, "lifecycle_generation": 1]
    }
    private func decision(_ change: (inout [String: Any]) -> Void = { _ in }) throws -> SharedDecision {
        var object: [String: Any] = ["file_id": file, "reference": try binding(), "method": "remux",
            "play_url": base + "/stream.mp4?audio=4095",
            "delivery": ["mode": "remux", "url": base + "/stream.mp4?audio=4095", "sessions_url": base + "/hls/sessions"],
            "vod_indexed": true, "delivered_audio": ["codec": "aac", "channels": 6],
            "convert_dolby_vision": true, "container": "mp4", "prior_kbps": 4294967295 as Int64,
            "prefer_segmented": "high-bitrate", "source": ["dv_profile": 7, "dv_el_present": true, "frame_rate": "24000/1001"],
            "future": ["exact_integer": 9223372036854775807 as Int64]]
        change(&object)
        return try SharedDecision.decode(JSONSerialization.data(withJSONObject: object))
    }
    func testDistinctDecisionPreservesWireAndCannotAcquireMediaAuthority() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        let result = try decision().validated(context)
        XCTAssertEqual(result.fileId, file)
        XCTAssertEqual(result.wire["prior_kbps"], .integer(4294967295))
        XCTAssertEqual(result.wire["future"]?.object?["exact_integer"], .integer(Int64.max))
        XCTAssertEqual(result.wire["source"]?.object?["frame_rate"], .string("24000/1001"))
        XCTAssertEqual(result.wire["convert_dolby_vision"], .bool(true))
        XCTAssertThrowsError(try context.translatedDeliveryPath(result.playUrl))
        XCTAssertNoThrow(try PlaybackSubject.shared(context: context).validated())
        XCTAssertThrowsError(try PlaybackSubject.local(itemId: ref.itemId, context: context).validated())
        for bad in ["https://a.test" + base + "/direct", "/api/v1/files/7/direct", base + "/stream.mp4?audio=4096", base + "/stream.mp4?audio=01", base + "/stream.mp4?audio=1&audio=2", base + "/direct?session=44444444-4444-4444-8444-444444444444", base + "/direct?token=x", base + "/direct?"] {
            XCTAssertThrowsError(try decision { $0["play_url"] = bad }.validated(context))
        }
    }
    func testExactReferenceRejectsCollisionAndRetiredAccount() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        for key in ["import_id", "server_id", "catalogue_epoch", "library_id", "item_id"] {
            XCTAssertThrowsError(try decision { object in
                var binding = object["reference"] as! [String: Any]
                var item = binding["item"] as! [String: Any]
                item[key] = key.hasSuffix("id") && ["library_id", "item_id"].contains(key) ? "7" : "55555555-5555-4555-8555-555555555555"
                binding["item"] = item; object["reference"] = binding
            }.validated(context))
        }
        XCTAssertThrowsError(try decision { $0["file_id"] = 9007199254740993 as Int64 })
        XCTAssertThrowsError(try decision { var b = $0["reference"] as! [String: Any]; b["revision"] = String(repeating: "b", count: 64); $0["reference"] = b }.validated(context))
        XCTAssertThrowsError(try decision { var b = $0["reference"] as! [String: Any]; b["file_id"] = "7"; $0["reference"] = b }.validated(context))
        let result = try decision()
        Session.shared.setCredentials(origin: "https://b.test", token: nil)
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        XCTAssertThrowsError(try result.validated(context))
    }
    private func manifest(_ change: (inout [String: Any]) -> Void = { _ in }) throws -> SharedPGSManifest {
        var object: [String: Any] = ["schema": 1, "generation": revision, "file_id": file, "reference": try binding(),
            "track_index": 2, "kind": "pgs", "timebase": "source_ms", "duration_ms": 1000,
            "cues": [["id": "cue", "start_ms": 0, "end_ms": 500, "canvas_width": 1920, "canvas_height": 1080,
                "objects": [["image": "overlay/" + revision + "/objects/" + String(repeating: "b", count: 64) + ".png", "x": 0, "y": 0, "width": 100, "height": 50]]]]]
        change(&object)
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(SharedPGSManifest.self, from: JSONSerialization.data(withJSONObject: object))
    }
    func testSharedPGSIdentityTimingAndObjectContainment() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        XCTAssertEqual(try manifest().validated(context: context, trackIndex: 2).fileId, file)
        XCTAssertThrowsError(try manifest { $0["file_id"] = 9007199254740993 as Int64 })
        XCTAssertThrowsError(try manifest().validated(context: context, trackIndex: 3))
        for image in ["https://a.test/object.png", "overlay/../objects/" + revision + ".png", "overlay/" + String(repeating: "c", count: 64) + "/objects/" + revision + ".png"] {
            XCTAssertThrowsError(try manifest { object in
                var cues = object["cues"] as! [[String: Any]]; var objects = cues[0]["objects"] as! [[String: Any]]
                objects[0]["image"] = image; cues[0]["objects"] = objects; object["cues"] = cues
            }.validated(context: context, trackIndex: 2))
        }
        XCTAssertThrowsError(try manifest { object in var cues = object["cues"] as! [[String: Any]]; cues[0]["end_ms"] = 1001; object["cues"] = cues }.validated(context: context, trackIndex: 2))
        XCTAssertThrowsError(try manifest { object in var cues = object["cues"] as! [[String: Any]]; var objects = cues[0]["objects"] as! [[String: Any]]; objects[0]["width"] = Int.max; cues[0]["objects"] = objects; object["cues"] = cues }.validated(context: context, trackIndex: 2))
    }
    func testStartRequiresAlreadyAdmittedExactBSessionAndClosedControlPaths() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let context = try await fetch(base)
        let session = "44444444-4444-4444-8444-444444444444"
        let bound = try context.withSession(session)
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let data = try JSONSerialization.data(withJSONObject: ["prior_kbps": 4294967295 as Int64, "quality_catalog_status": ["state": "complete"], "plan_notes": ["retained"], "session_id": session, "playlist_url": "/api/v1/hls/" + session + "/master.m3u8?native=1&subtitle=2&diagnostic=video-only", "control": ["protocol": "plurx-playback-control-v1", "url": "/api/v1/hls/" + session + "/control", "generation": "55555555-5555-4555-8555-555555555555", "control_epoch": 1, "next_exchange_ms": 1000, "lease_timeout_ms": 5000]])
        let sharedStart = try SharedStart.decode(data).validated(bound)
        XCTAssertEqual(sharedStart.wire["prior_kbps"], .integer(4294967295))
        XCTAssertEqual(sharedStart.wire["plan_notes"], .array([.string("retained")]))
        let start = sharedStart.response
        XCTAssertNoThrow(try SharedStartValidation.validated(start, context: bound))
        XCTAssertThrowsError(try SharedStartValidation.validated(start, context: context))
        for query in ["?token=x", "?native=1&native=0", "?subtitle=4096", "?diagnostic=upstream", "?native=%31", "?"] {
            var object = try JSONSerialization.jsonObject(with: data) as! [String: Any]
            object["playlist_url"] = "/api/v1/hls/" + session + "/index.m3u8" + query
            let invalid = try decoder.decode(HlsStart.self, from: JSONSerialization.data(withJSONObject: object))
            XCTAssertThrowsError(try SharedStartValidation.validated(invalid, context: bound))
        }
        var wrong = start
        wrong.control?.url = "/api/v1/hls/55555555-5555-4555-8555-555555555555/control"
        XCTAssertThrowsError(try SharedStartValidation.validated(wrong, context: bound))
    }
}
