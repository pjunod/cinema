import Foundation
import XCTest
@testable import plurx

private final class ControlHTTP: URLProtocol {
    static var answer: ((URLRequest) throws -> (Int, Data))!
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        do {
            let (status, body) = try Self.answer(request)
            client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: [:])!, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: body); client?.urlProtocolDidFinishLoading(self)
        } catch { client?.urlProtocol(self, didFailWithError: error) }
    }
    override func stopLoading() {}
}

/// Synthetic B answers through actual authenticated URLProtocol I/O. These are
/// client protocol contracts, not physical Source or device evidence.
@MainActor
final class SharedPlaybackControlTests: XCTestCase {
    private let ref = SharedPlaybackReference(importId: "11111111-1111-4111-8111-111111111111", serverId: "22222222-2222-4222-8222-222222222222", catalogueEpoch: "33333333-3333-4333-8333-333333333333", libraryId: "7", itemId: "9007199254740993")
    private let session = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
    private let generation = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"
    private var base: String { "/api/v1/shared/imports/\(ref.importId)/files/" + String(repeating: "L", count: 236) }
    private var configuration: URLSessionConfiguration { let value = URLSessionConfiguration.ephemeral; value.protocolClasses = [ControlHTTP.self]; return value }
    override func setUp() async throws { Session.shared.setCredentials(origin: "https://b.test", token: "control-bearer") }
    override func tearDown() async throws { ControlHTTP.answer = nil; Session.shared.setCredentials(origin: "", token: nil) }

    private func binding(_ file: String = "5") throws -> [String: Any] {
        let encoder = JSONEncoder(); encoder.keyEncodingStrategy = .convertToSnakeCase
        return ["item": try JSONSerialization.jsonObject(with: encoder.encode(ref)), "file_id": file, "revision": String(repeating: "a", count: 64), "lifecycle_generation": 3]
    }
    private func context() async throws -> PlaybackFileContext {
        let detail: [String: Any] = ["lifecycle_generation": 3, "files": [["file_id": "5", "revision": String(repeating: "a", count: 64), "file_base": base, "reference": try binding()]]]
        let data = try JSONSerialization.data(withJSONObject: detail)
        ControlHTTP.answer = { _ in (200, data) }
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: "5", testTransport: URLSession(configuration: configuration))
    }
    private func decision(method: String = "remux", change: (inout [String: Any]) -> Void = { _ in }) throws -> SharedDecision {
        let mode = method == "direct_play" ? "direct" : method
        var wire: [String: Any] = ["file_id": "5", "reference": try binding(), "method": method, "play_url": base + "/hls/sessions",
            "delivery": ["mode": mode, "sessions_url": base + "/hls/sessions"], "source": ["container": "mp4", "hdr": "sdr"],
            "audio": [["index": 1, "codec": "aac", "default": true], ["index": 2, "codec": "ac3", "default": false]],
            "subtitles": [["index": 3, "codec": "subrip", "default": false, "forced": false, "text": true, "native": true],
                          ["index": 4, "codec": "hdmv_pgs_subtitle", "default": false, "forced": false, "text": false]],
            "delivered_dynamic_range": "sdr"]
        change(&wire)
        return try SharedDecision.decode(JSONSerialization.data(withJSONObject: wire))
    }
    private func reply(_ id: String? = nil) throws -> Data {
        let id = id ?? session
        return try JSONSerialization.data(withJSONObject: ["session_id": id, "playlist_url": "/api/v1/hls/\(id)/master.m3u8", "vod": true,
            "start_seconds": 0.0, "duration_ms": 600_000, "control": ["protocol": "plurx-playback-control-v1", "url": "/api/v1/hls/\(id)/control",
            "generation": generation, "control_epoch": 4, "next_exchange_ms": 5_000, "lease_timeout_ms": 300_000]])
    }
    private func body(_ request: URLRequest) throws -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }; stream.open(); defer { stream.close() }
        var result = Data(), buffer = [UInt8](repeating: 0, count: 4096)
        while true { let count = stream.read(&buffer, maxLength: buffer.count); if count <= 0 { break }; result.append(contentsOf: buffer.prefix(count)) }
        return result
    }
    private func started(height: Int? = nil, auto: Bool? = nil, copy: Bool? = true) async throws -> (SharedStartedPlayback, SharedPlaybackPlan, SharedDecisionClient) {
        let context = try await context(), bytes = try reply()
        ControlHTTP.answer = { _ in (200, bytes) }
        let caps = Caps.snapshot().document
        let request = CreateSessionRequest(playbackId: "shared-player", requestId: "dddddddd-dddd-4ddd-8ddd-dddddddddddd", height: height, qualityAuto: auto, start: 0, copy: copy, caps: caps)
        let client = try SharedDecisionClient(testConfiguration: configuration)
        let playback = try await client.start(context: context, request: request)
        let subject = SharedPlaybackSubject(context: context, title: "Shared", resumeMs: 0, watchSequence: 7)
        let plan = try SharedPlaybackPlan(subject: subject, decision: try decision(method: copy == true ? "remux" : "transcode"), caps: caps, request: request)
        return (playback, plan, client)
    }
    func testStatusRendersOnlyValidatedTokensBoundToTheRetainedPlayback() async throws {
        let (playback, _, client) = try await started()
        var metrics: [String: Any] = Dictionary(uniqueKeysWithValues: SharedPlaybackStatus.requiredCounters.map { ($0, 0) })
        metrics["target_height"] = 720; metrics["encoder"] = "h264_videotoolbox"; metrics["playlist_shape"] = "vod"
        metrics["producer_state"] = "running"; metrics["server_ready_state"] = "ready"; metrics["ahead_seconds"] = 42
        metrics["admitted"] = true; metrics["suspended"] = false; metrics["final"] = false
        var wire: [String: Any] = ["subject": "shared", "reference": try binding(), "session_id": session, "incarnation_id": generation, "control_epoch": 4, "status": metrics]
        let bytes = try JSONSerialization.data(withJSONObject: wire)
        ControlHTTP.answer = { request in XCTAssertEqual(request.url?.path, "/api/v1/hls/\(self.session)/status"); return (200, bytes) }
        let status = try await client.status(playback: playback)
        XCTAssertEqual(status.summary, "Shared HLS · 720p · h264_videotoolbox · running · 42 s ahead")
        XCTAssertEqual(status.serverReadyState, "ready")
        for prose in ["copy encoder", "/srv/media/film.mkv", "x\u{1}", "café", String(repeating: "a", count: 33)] {
            var bad = metrics; bad["encoder"] = prose; wire["status"] = bad
            XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback), prose)
        }
        wire["status"] = metrics; wire["session_id"] = "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee"
        XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback))
        wire["session_id"] = session; wire["subject"] = "local"
        XCTAssertThrowsError(try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback))
    }
}
