import Foundation
import XCTest
@testable import plurx

private final class FixtureHTTP: URLProtocol {
    static var body: Data?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: 200, httpVersion: nil, headerFields: [:])!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.body ?? Data()); client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

/// Drives the Swift Shared parsers from `tests/sharing/protocol-cases.json`,
/// the rows Rust and the web already read. Every group a native client parses
/// is asserted row by row; B-only groups (`asset_session_query`,
/// `resource_unsupported`, `playback_ids`, `receiver_recovery`) and `layer: b`
/// mutations are B's and are not read here.
@MainActor
final class SharedProtocolFixtureTests: XCTestCase {
    private var fixture: [String: Any] = [:]
    private var bSession = ""
    override func setUp() async throws {
        Session.shared.setCredentials(origin: "https://b.test", token: "fixture-bearer")
        let url = try XCTUnwrap(Bundle(for: SharedProtocolFixtureTests.self).url(forResource: "protocol-cases", withExtension: "json"))
        fixture = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
        XCTAssertEqual(fixture["version"] as? Int, 2)
        bSession = try XCTUnwrap((fixture["hls_start"] as? [String: Any])?["receiver"] as? [String: Any])["session_id"] as! String
    }
    override func tearDown() async throws { FixtureHTTP.body = nil; Session.shared.setCredentials(origin: "", token: nil) }

    private func group(_ name: String) throws -> Any { try XCTUnwrap(fixture[name], name) }
    private func rows(_ name: String) throws -> [[String: Any]] { try XCTUnwrap(group(name) as? [[String: Any]], name) }
    private func object(_ name: String) throws -> [String: Any] { try XCTUnwrap(group(name) as? [String: Any], name) }
    private func clientLayer(_ row: [String: Any]) -> Bool { (row["layer"] as? String).map { $0 == "client" || $0 == "both" } ?? true }

    /// The fixture's own signed context, admitted through the ordinary
    /// authenticated detail read (no raw locator constructor exists).
    private func context() async throws -> PlaybackFileContext {
        let wire = try object("context")
        let reference = try XCTUnwrap(wire["reference"] as? [String: Any])
        let file = try XCTUnwrap(wire["file_id"] as? String), revision = try XCTUnwrap(wire["revision"] as? String)
        let lifecycle = try XCTUnwrap(wire["lifecycle_generation"] as? Int)
        let binding: [String: Any] = ["item": reference, "file_id": file, "revision": revision, "lifecycle_generation": lifecycle]
        FixtureHTTP.body = try JSONSerialization.data(withJSONObject: ["lifecycle_generation": lifecycle,
            "files": [["file_id": file, "revision": revision, "file_base": try XCTUnwrap(wire["file_base"] as? String), "reference": binding]]])
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let ref = try decoder.decode(SharedPlaybackReference.self, from: JSONSerialization.data(withJSONObject: reference))
        let configuration = URLSessionConfiguration.ephemeral; configuration.protocolClasses = [FixtureHTTP.self]
        return try await PlaybackFileContext.authenticatedDetail(reference: ref, fileId: file, testTransport: URLSession(configuration: configuration))
    }

    private func mutate(_ value: Any, path: [String], row: [String: Any]) -> Any {
        guard let key = path.first, var dictionary = value as? [String: Any] else { return row["value"] ?? NSNull() }
        if path.count == 1 {
            if row["op"] as? String == "remove" { dictionary.removeValue(forKey: key) } else { dictionary[key] = row["value"] ?? NSNull() }
        } else {
            dictionary[key] = mutate(dictionary[key] ?? [String: Any](), path: Array(path.dropFirst()), row: row)
        }
        return dictionary
    }
    private func substituted(_ value: Any) -> Any {
        if let text = value as? String { return text.replacingOccurrences(of: "{session}", with: bSession) }
        if let dictionary = value as? [String: Any] { return dictionary.mapValues(substituted) }
        if let array = value as? [Any] { return array.map(substituted) }
        return value
    }
    private func request() -> CreateSessionRequest {
        CreateSessionRequest(playbackId: "player-fixture", requestId: "e0e0e0e0-e0e0-40e0-80e0-e0e0e0e0e0e0", start: 30.5, copy: true, caps: Caps.snapshot().document)
    }
    private func started(_ context: PlaybackFileContext) throws -> SharedStartedPlayback {
        let wire = try XCTUnwrap(object("hls_start")["public"])
        return try SharedStart.decode(JSONSerialization.data(withJSONObject: wire)).bindInitial(context, request: request())
    }

    func testSourceIdCasesUseTheCanonicalDecimalGrammar() throws {
        let cases = try rows("cases")
        XCTAssertEqual(cases.count, 12)
        for row in cases {
            let id = try XCTUnwrap((row["input"] as? [String: Any])?["source_id"] as? String)
            XCTAssertEqual(PlaybackFileContext.canonicalID(id), row["expected"] as? String == "accepted", row["id"] as? String ?? id)
        }
    }

    func testStatusTokensAndEveryWordFieldFollowTheTokenGrammar() async throws {
        let tokens = try rows("status_tokens")
        for row in tokens {
            XCTAssertEqual(SharedPlaybackStatus.isToken(row["token"] as! String), row["expected"] as? String == "accepted", row["id"] as! String)
        }
        let status = try object("shared_status")
        let fields = try XCTUnwrap(status["word_fields"] as? [String])
        XCTAssertEqual(Set(fields), SharedPlaybackStatus.requiredWords.union(SharedPlaybackStatus.optionalWords))
        let playback = try started(try await context())
        let accepted = try XCTUnwrap(status["accepted"] as? [String: Any])
        for field in fields {
            for row in tokens {
                let wire = mutate(accepted, path: ["status", field], row: ["value": row["token"]!])
                let decoded = try? SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback)
                XCTAssertEqual(decoded != nil, row["expected"] as? String == "accepted", "\(field) = \(row["id"]!)")
            }
        }
    }

    func testSharedStatusAcceptedEnvelopeAndClientMutations() async throws {
        let status = try object("shared_status")
        let playback = try started(try await context())
        let accepted = try XCTUnwrap(status["accepted"] as? [String: Any])
        let decoded = try SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: accepted), playback: playback)
        XCTAssertEqual(decoded.summary, "Shared HLS · 720p · h264_videotoolbox · running · 42 s ahead")
        let mutations = try XCTUnwrap(status["mutations"] as? [[String: Any]]).filter(clientLayer)
        XCTAssertEqual(mutations.count, 13)
        for row in mutations {
            let wire = mutate(accepted, path: row["path"] as! [String], row: row)
            let result = try? SharedPlaybackStatus.decode(JSONSerialization.data(withJSONObject: wire), playback: playback)
            XCTAssertEqual(result != nil, row["expected"] as? String == "accepted", row["id"] as! String)
        }
    }

    func testControlRefusalsMapToTheClientOutcome() throws {
        let refusals = try object("control_refusals")
        var checked = 0
        for row in (refusals["source"] as! [[String: Any]]) + (refusals["b_precheck"] as! [[String: Any]]) {
            guard let b = row["b"] as? [String: Any], let expected = row["client"] as? String else { continue }
            let failure = ControlTransportError(status: b["status"] as? Int, code: b["code"] as? String, retryAfterMs: b["retry_after_ms"] as? Int)
            let outcome = SharedControlChannel.classify(failure)
            switch expected {
            case "retry":
                guard case .retry(let delay) = outcome else { XCTFail("\(row["id"]!) did not retry"); continue }
                XCTAssertEqual(delay, max(250, min((b["retry_after_ms"] as? Int) ?? 500, 5_000)), row["id"] as! String)
            case "stop": XCTAssertTrue(outcome == .ended || outcome == .refused, row["id"] as! String)
            default: XCTFail("unknown client outcome \(expected)")
            }
            checked += 1
        }
        XCTAssertEqual(checked, 10)
    }

    func testRelayedPreparationNoneDeclinesAndAbsenceStaysArmed() throws {
        for row in try rows("control_preparation") {
            let b = row["b"] as? String
            let step = SharedControlStep.afterChange(.accepted(preparation: b), playing: true)
            switch row["client"] as? String {
            case "declined": XCTAssertEqual(step, .reopen(play: true), row["id"] as! String)
            case "armed": XCTAssertEqual(step, .refused, row["id"] as! String)
            default: XCTFail("unknown client outcome")
            }
            // The Local wait the prepared handoff reuses reads the same value:
            // a later `none` declines, absence never does.
            var wait = PreparedOfferWait(tappedAtMs: 0, floorSequence: 1)
            _ = wait.observe(answer: PlaybackControlAnswer(requestSequence: 1, action: nil, preparation: b), nowMs: 1)
            let later = wait.observe(answer: PlaybackControlAnswer(requestSequence: 2, action: nil, preparation: b), nowMs: 2)
            XCTAssertEqual(later == .reopen(reason: "declined"), row["client"] as? String == "declined", row["id"] as! String)
        }
    }

    func testHlsStartPublicProjectionAndClientMutations() async throws {
        let context = try await context()
        let start = try object("hls_start")
        let playback = try started(context)
        XCTAssertEqual(playback.context.sessionId, bSession)
        XCTAssertEqual(playback.start.response.control?.generation, (start["receiver"] as! [String: Any])["incarnation_id"] as? String)
        let base = try XCTUnwrap(start["public"] as? [String: Any])
        let mutations = try XCTUnwrap(start["mutations"] as? [[String: Any]]).filter(clientLayer)
        XCTAssertEqual(mutations.count, 16)
        for row in mutations {
            let wire = mutate(base, path: row["path"] as! [String], row: substituted(row) as! [String: Any])
            let result = try? SharedStart.decode(JSONSerialization.data(withJSONObject: wire)).bindInitial(context, request: request())
            XCTAssertEqual(result != nil, row["expected"] as? String == "accepted", row["id"] as! String)
        }
    }

    func testDirectStartPublicReplyMimesAndClientMutations() async throws {
        let context = try await context()
        let direct = try object("direct")
        XCTAssertEqual(Set(try XCTUnwrap(direct["mimes"] as? [String])), SharedDirectStart.mimes)
        let base = try XCTUnwrap(direct["public"] as? [String: Any])
        let decoded = try SharedDirectStart.decode(JSONSerialization.data(withJSONObject: base), context: context)
        XCTAssertEqual(decoded.start.sessionId, direct["b_session"] as? String)
        let mutations = try XCTUnwrap(direct["mutations"] as? [[String: Any]]).filter(clientLayer)
        XCTAssertEqual(mutations.count, 13)
        for row in mutations {
            let wire = mutate(base, path: row["path"] as! [String], row: row)
            let result = try? SharedDirectStart.decode(JSONSerialization.data(withJSONObject: wire), context: context)
            XCTAssertEqual(result != nil, row["expected"] as? String == "accepted", row["id"] as! String)
        }
        let fileBase = try XCTUnwrap(object("context")["file_base"] as? String)
        for row in try rows("direct_session_query") {
            var wire = base; wire["url"] = fileBase + "/direct?" + (row["query"] as! String)
            let result = try? SharedDirectStart.decode(JSONSerialization.data(withJSONObject: wire), context: context)
            XCTAssertEqual(result != nil, row["expected"] as? String == "accepted", row["id"] as! String)
        }
    }

    func testFileSuffixGrammarAndPresessionAssetPaths() async throws {
        let unbound = try await context()
        let bound = try unbound.withSession(bSession)
        let fileBase = try XCTUnwrap(object("context")["file_base"] as? String)
        let suffixes = try rows("file_suffixes")
        XCTAssertEqual(suffixes.count, 25)
        for row in suffixes {
            let suffix = row["suffix"] as! String
            XCTAssertEqual((try? bound.path(suffix)) != nil, row["expected"] as? String == "accepted", "suffix \(suffix)")
        }
        let presession = try object("presession_assets")
        let query = try XCTUnwrap(presession["bound_query"] as? String)
        for suffix in try XCTUnwrap(presession["suffixes"] as? [String]) {
            XCTAssertEqual(try unbound.path(suffix), fileBase + "/" + suffix, suffix)
            XCTAssertEqual(try bound.path(suffix), fileBase + "/" + suffix + "?" + query, suffix)
        }
    }
}
