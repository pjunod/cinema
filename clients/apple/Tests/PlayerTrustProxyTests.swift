import Foundation
import Network
import XCTest
@testable import plurx

private final class ProxyRangeFixture: @unchecked Sendable {
    private let queue = DispatchQueue(label: "cinema.tests.proxy-upstream")
    private let listener: NWListener
    private let lock = NSLock()
    private var requestHeader: String?
    private var boundPort: UInt16?

    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: parameters)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { [weak self] state in
            guard let self else { return }
            if case .ready = state {
                self.lock.lock(); self.boundPort = self.listener.port?.rawValue; self.lock.unlock()
                ready.signal()
            }
            if case .failed = state { ready.signal() }
        }
        listener.newConnectionHandler = { [weak self] peer in
            guard let self else { peer.cancel(); return }
            peer.start(queue: self.queue)
            self.receive(peer, header: Data())
        }
        listener.start(queue: queue)
        guard ready.wait(timeout: .now() + 3) == .success, port != nil else {
            listener.cancel()
            throw URLError(.cannotConnectToHost)
        }
    }

    var port: UInt16? { lock.lock(); defer { lock.unlock() }; return boundPort }
    var header: String? { lock.lock(); defer { lock.unlock() }; return requestHeader }
    func stop() { listener.cancel() }

    private func receive(_ peer: NWConnection, header: Data) {
        peer.receive(minimumIncompleteLength: 1, maximumLength: 16_384) { [weak self] bytes, _, done, error in
            guard let self, error == nil else { peer.cancel(); return }
            var next = header; if let bytes { next.append(bytes) }
            guard next.count < 16_384 else { peer.cancel(); return }
            guard next.range(of: Data("\r\n\r\n".utf8)) != nil else {
                if done { peer.cancel() } else { self.receive(peer, header: next) }
                return
            }
            self.lock.lock(); self.requestHeader = String(decoding: next, as: UTF8.self); self.lock.unlock()
            let response = "HTTP/1.1 206 Partial Content\r\nContent-Type: video/mp4\r\nContent-Length: 4\r\nContent-Range: bytes 2-5/8\r\nAccept-Ranges: bytes\r\nETag: \"fixture-v1\"\r\nConnection: close\r\n\r\ncdef"
            peer.send(content: Data(response.utf8), completion: .contentProcessed { _ in peer.cancel() })
        }
    }
}

private final class ProxyHTTPResult: @unchecked Sendable {
    private let lock = NSLock()
    private var result: Result<(Data, URLResponse), Error>?
    func store(_ value: Result<(Data, URLResponse), Error>) { lock.lock(); result = value; lock.unlock() }
    func load() -> Result<(Data, URLResponse), Error>? { lock.lock(); defer { lock.unlock() }; return result }
}

final class PlayerTrustProxyTests: XCTestCase {
    private let base = URL(string: "https://media.test:32400/api/v1/hls/session/master.m3u8?token=parent")!

    private func rewrite(_ source: String, relativeTo base: URL? = nil) throws -> (String, [URL]) {
        var mapped: [URL] = []
        let rewritten = try PlayerTrustProxy.rewrite(source, relativeTo: base ?? self.base) { remote in
            mapped.append(remote)
            return URL(string: "http://127.0.0.1:12345/capability/\(mapped.count)")!
        }
        return (rewritten, mapped)
    }

    private func fetch(_ request: URLRequest) throws -> (Data, HTTPURLResponse) {
        let completed = expectation(description: "loopback HTTP response")
        let result = ProxyHTTPResult()
        let session = URLSession(configuration: .ephemeral)
        defer { session.invalidateAndCancel() }
        session.dataTask(with: request) { data, response, error in
            if let error { result.store(.failure(error)) }
            else if let data, let response { result.store(.success((data, response))) }
            else { result.store(.failure(URLError(.badServerResponse))) }
            completed.fulfill()
        }.resume()
        wait(for: [completed], timeout: 8)
        let (data, response) = try XCTUnwrap(result.load()).get()
        return (data, try XCTUnwrap(response as? HTTPURLResponse))
    }

    func testMasterVariantsAndAlternateAudioAndSubtitlesResolveThroughProxy() throws {
        let source = """
        #EXTM3U
        #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",URI="audio/en.m3u8"
        #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID="subs",URI="../subtitles/en.m3u8?token=sub"
        #EXT-X-STREAM-INF:BANDWIDTH=4000000,AUDIO="audio",SUBTITLES="subs"
        video/2160p.m3u8?token=variant
        """
        let (rewritten, mapped) = try rewrite(source)
        XCTAssertEqual(Set(mapped.map(\.absoluteString)), Set([
            "https://media.test:32400/api/v1/hls/session/audio/en.m3u8",
            "https://media.test:32400/api/v1/hls/subtitles/en.m3u8?token=sub",
            "https://media.test:32400/api/v1/hls/session/video/2160p.m3u8?token=variant"
        ]))
        XCTAssertTrue(rewritten.contains("#EXT-X-STREAM-INF:BANDWIDTH=4000000,AUDIO=\"audio\",SUBTITLES=\"subs\""))
        XCTAssertEqual(rewritten.components(separatedBy: "http://127.0.0.1:").count - 1, 3)
    }

    func testInitializationAndEncryptionURIsAndByteRangeMetadataAreRewritten() throws {
        let source = """
        #EXTM3U
        #EXT-X-MAP:URI="init.mp4?token=init",BYTERANGE="128@0"
        #EXT-X-KEY:METHOD=AES-128,URI="https://keys.test/key.bin?token=key",IV=0x0001
        #EXT-X-BYTERANGE:32@128
        segment.mp4
        """
        let (rewritten, mapped) = try rewrite(source)
        XCTAssertEqual(mapped.count, 3)
        XCTAssertTrue(mapped.contains(URL(string: "https://keys.test/key.bin?token=key")!))
        XCTAssertTrue(mapped.contains(URL(string: "https://media.test:32400/api/v1/hls/session/init.mp4?token=init")!))
        XCTAssertTrue(rewritten.contains("BYTERANGE=\"128@0\""))
        XCTAssertTrue(rewritten.contains("IV=0x0001"))
        XCTAssertTrue(rewritten.contains("#EXT-X-BYTERANGE:32@128"))
    }

    func testQueryOnlyParentRelativeAndAbsoluteReferencesPreserveTheirTargets() throws {
        let (_, mapped) = try rewrite("#EXTM3U\n?token=child\n../other/seg.m4s\nhttps://cdn.test/movie/seg.m4s?token=cdn")
        XCTAssertEqual(mapped.map(\.absoluteString), [
            "https://media.test:32400/api/v1/hls/session/master.m3u8?token=child",
            "https://media.test:32400/api/v1/hls/other/seg.m4s",
            "https://cdn.test/movie/seg.m4s?token=cdn"
        ])
    }

    func testUnicodeAndPercentEscapedPathsKeepTheirURLIdentity() throws {
        let (_, mapped) = try rewrite("#EXTM3U\n字幕/日本語.m3u8?name=caf%C3%A9\npart%20one%22.m4s?token=a%2Fb%2Bc%3D")
        XCTAssertEqual(mapped.map(\.absoluteString), [
            URL(string: "字幕/日本語.m3u8?name=caf%C3%A9", relativeTo: base)!.absoluteURL.absoluteString,
            "https://media.test:32400/api/v1/hls/session/part%20one%22.m4s?token=a%2Fb%2Bc%3D"
        ])
    }

    func testMultipleURIAttributesAreReplacedWithoutInvalidatingEarlierRanges() throws {
        let (rewritten, mapped) = try rewrite("#EXTM3U\n#EXT-X-TEST:URI=\"a.m4s\",SECOND-URI=\"longer-b.m4s\"")
        XCTAssertEqual(Set(mapped.map(\.lastPathComponent)), Set(["a.m4s", "longer-b.m4s"]))
        XCTAssertTrue(rewritten.contains("URI=\"http://127.0.0.1:12345/capability/2\",SECOND-URI=\"http://127.0.0.1:12345/capability/1\""))
    }

    func testFairPlaySKDKeyURIRemainsAnAuthenticationReference() throws {
        let source = "#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"skd://content-id\",KEYFORMAT=\"com.apple.streamingkeydelivery\"\nsegment.m4s"
        let (rewritten, mapped) = try rewrite(source)
        XCTAssertEqual(mapped.count, 1)
        XCTAssertTrue(rewritten.contains("URI=\"skd://content-id\""))
        XCTAssertTrue(rewritten.contains("KEYFORMAT=\"com.apple.streamingkeydelivery\""))
    }

    func testHTTPSPlaylistRejectsPlaintextChildrenBeforeMapping() {
        for child in ["http://media.test/segment.m4s", "#EXT-X-MAP:URI=\"http://media.test/init.mp4\"", "#EXT-X-KEY:METHOD=AES-128,URI=\"http://keys.test/key\""] {
            var mapped = false
            XCTAssertThrowsError(try PlayerTrustProxy.rewrite("#EXTM3U\n" + child, relativeTo: base) { remote in mapped = true; return remote })
            XCTAssertFalse(mapped)
        }
    }

    func testUnsafeSchemesCannotEscapeThroughPlainLinesOrURIAttributes() {
        for uri in ["file:///private/movie.m4s", "data:video/mp4;base64,AA==", "javascript:alert(1)", "ftp://media.test/segment"] {
            XCTAssertThrowsError(try rewrite("#EXTM3U\n" + uri))
            XCTAssertThrowsError(try rewrite("#EXTM3U\n#EXT-X-MAP:URI=\"\(uri)\""))
        }
    }

    func testHTTPSourceRemainsPlayableAndCommentAndBlankLinesRemainMetadata() throws {
        let source = "#EXTM3U\n\n# a comment without media\nhttp://media.test/seg.m4s"
        let (rewritten, mapped) = try rewrite(source, relativeTo: URL(string: "http://media.test/master.m3u8")!)
        XCTAssertEqual(mapped.map(\.absoluteString), ["http://media.test/seg.m4s"])
        XCTAssertTrue(rewritten.hasPrefix("#EXTM3U\n\n# a comment without media\n"))
    }

    func testCapabilityURLIsLoopbackOnlyAndEncryptsCompleteUpstreamIdentity() throws {
        let upstream = URL(string: "https://media.test:32400/a%20b/movie.m3u8?token=a%2Fb%2Bc%3D&subtitle=ja")!
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: upstream))
        XCTAssertEqual(local.host, "127.0.0.1")
        XCTAssertEqual(local.scheme, "http")
        XCTAssertNotNil(local.port)
        XCTAssertNil(local.query)
        let components = local.path.split(separator: "/")
        XCTAssertEqual(components.count, 2)
        XCTAssertGreaterThan(components[0].count, 64)
        var encoded = String(components[1]).replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        encoded += String(repeating: "=", count: (4 - encoded.count % 4) % 4)
        let ciphertext = try XCTUnwrap(Data(base64Encoded: encoded))
        XCTAssertNil(ciphertext.range(of: Data(upstream.absoluteString.utf8)))
        XCTAssertNil(ciphertext.range(of: Data("token=a%2Fb%2Bc%3D".utf8)))
        XCTAssertEqual(PlayerTrustProxy.shared.originalURL(for: local), upstream)
        XCTAssertEqual(PlayerTrustProxy.shared.url(for: upstream), local, "Live playlist segment URLs stay stable")
        XCTAssertEqual(PlayerTrustProxy.shared.url(for: local), local)
        XCTAssertNil(PlayerTrustProxy.shared.originalURL(for: upstream))
        for invalid in ["file:///tmp/movie.mp4", "skd://content-id", "https://user:password@media.test/movie.mp4"] {
            XCTAssertNil(PlayerTrustProxy.shared.url(for: URL(string: invalid)!))
        }
    }

    func testInvalidCapabilityMalformedPayloadAndUnsupportedMethodsReturn404() throws {
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: URL(string: "https://upstream.invalid/movie.mp4")!))
        let pieces = local.path.split(separator: "/")
        var components = try XCTUnwrap(URLComponents(url: local, resolvingAgainstBaseURL: false))
        let invalidPaths = ["/wrong-capability/\(pieces[1])", "/\(pieces[0])/not+valid-base64!", "/\(pieces[0])/ZmlsZTovLy90bXAvbW92aWUubXA0"]
        for path in invalidPaths {
            components.path = path
            let (body, response) = try fetch(URLRequest(url: try XCTUnwrap(components.url)))
            XCTAssertEqual(response.statusCode, 404)
            XCTAssertTrue(body.isEmpty)
        }
        for method in ["POST", "PUT", "DELETE", "OPTIONS"] {
            var request = URLRequest(url: local); request.httpMethod = method
            XCTAssertEqual(try fetch(request).1.statusCode, 404)
        }
        components = try XCTUnwrap(URLComponents(url: local, resolvingAgainstBaseURL: false)); components.query = "extra=1"
        XCTAssertEqual(try fetch(URLRequest(url: try XCTUnwrap(components.url))).1.statusCode, 404)
    }

    func testCiphertextTamperingCannotRecoverOrFetchTheUpstream() throws {
        let upstream = URL(string: "https://upstream.invalid/movie.m3u8?token=private-fixture")!
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: upstream))
        let pieces = local.path.split(separator: "/")
        var encoded = String(pieces[1]).replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        encoded += String(repeating: "=", count: (4 - encoded.count % 4) % 4)
        var ciphertext = try XCTUnwrap(Data(base64Encoded: encoded))
        ciphertext[ciphertext.count - 1] ^= 1
        let damaged = ciphertext.base64EncodedString().replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        var components = try XCTUnwrap(URLComponents(url: local, resolvingAgainstBaseURL: false))
        components.path = "/\(pieces[0])/\(damaged)"
        let tampered = try XCTUnwrap(components.url)
        XCTAssertNil(PlayerTrustProxy.shared.originalURL(for: tampered))
        let (body, response) = try fetch(URLRequest(url: tampered))
        XCTAssertEqual(response.statusCode, 404)
        XCTAssertTrue(body.isEmpty)
    }

    func testCapabilityCannotBeUsedWithAnOffLoopbackHostHeader() throws {
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: URL(string: "https://upstream.invalid/movie.mp4")!))
        for host in ["off-loopback.test", "localhost:\(try XCTUnwrap(local.port))", "127.0.0.1:1"] {
            var request = URLRequest(url: local)
            request.setValue(host, forHTTPHeaderField: "Host")
            XCTAssertEqual(try fetch(request).1.statusCode, 404)
        }
    }

    func testHeadPreservesResponseMetadataWithoutSendingMediaBody() throws {
        let upstream = try ProxyRangeFixture()
        defer { upstream.stop() }
        let target = URL(string: "http://127.0.0.1:\(try XCTUnwrap(upstream.port))/segment.mp4")!
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: target))
        var request = URLRequest(url: local); request.httpMethod = "HEAD"
        let (body, response) = try fetch(request)
        XCTAssertEqual(response.statusCode, 206)
        XCTAssertTrue(body.isEmpty)
        XCTAssertEqual(response.value(forHTTPHeaderField: "Content-Length"), "4")
        XCTAssertEqual(response.value(forHTTPHeaderField: "Content-Range"), "bytes 2-5/8")
        XCTAssertTrue(try XCTUnwrap(upstream.header).hasPrefix("HEAD /segment.mp4 "))
    }

    func testByteRangeAndValidatorsPassThroughRealLoopbackUpstream() throws {
        let upstream = try ProxyRangeFixture()
        defer { upstream.stop() }
        let target = URL(string: "http://127.0.0.1:\(try XCTUnwrap(upstream.port))/segment.mp4?token=fixture")!
        let local = try XCTUnwrap(PlayerTrustProxy.shared.url(for: target))
        var request = URLRequest(url: local)
        request.setValue("bytes=2-5", forHTTPHeaderField: "Range")
        request.setValue("\"fixture-v1\"", forHTTPHeaderField: "If-Range")
        request.setValue("never-forward-this", forHTTPHeaderField: "Authorization")
        let (body, response) = try fetch(request)
        XCTAssertEqual(response.statusCode, 206)
        XCTAssertEqual(body, Data("cdef".utf8))
        XCTAssertEqual(response.value(forHTTPHeaderField: "Content-Range"), "bytes 2-5/8")
        XCTAssertEqual(response.value(forHTTPHeaderField: "Accept-Ranges"), "bytes")
        XCTAssertEqual(response.value(forHTTPHeaderField: "ETag"), "\"fixture-v1\"")
        let header = try XCTUnwrap(upstream.header).lowercased()
        XCTAssertTrue(header.contains("range: bytes=2-5\r\n"))
        XCTAssertTrue(header.contains("if-range: \"fixture-v1\"\r\n"))
        XCTAssertTrue(header.contains("accept-encoding: identity\r\n"))
        XCTAssertFalse(header.contains("authorization:"))
    }
}
