import Foundation
import XCTest
@testable import plurx

private final class PreparationHTTP: URLProtocol {
    static var responseStatus = 200
    static var responseBody = Data()
    static var onRequest: ((PreparationHTTP) -> Void)?
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        if let onRequest = Self.onRequest { onRequest(self); return }
        finish(status: Self.responseStatus, body: Self.responseBody)
    }
    func finish(status: Int = 200, body: Data) {
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: status,
                            httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class GrowingPlaylistPreparationTests: XCTestCase {
    func testImmediateMasterWaitsForPublishedChildAndCancelsItsRequest() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [PreparationHTTP.self]
        let transport = URLSession(configuration: configuration)
        defer { PreparationHTTP.onRequest = nil; transport.invalidateAndCancel() }
        let url = URL(string: "http://media.test/hls/session/master.m3u8")!
        for cancel in [false, true] {
            let childEntered = expectation(description: "child request pending")
            var child: PreparationHTTP?
            PreparationHTTP.onRequest = { request in
                if request.request.url == url {
                    request.finish(body: Data("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nindex.m3u8\n".utf8))
                } else {
                    XCTAssertEqual(request.request.url?.path, "/hls/session/index.m3u8")
                    child = request
                    childEntered.fulfill()
                }
            }
            let preparation = Task { try await GrowingPlaylistPreparation.wait(for: url, using: transport) }
            await fulfillment(of: [childEntered], timeout: 3)
            if cancel {
                preparation.cancel()
                do { try await preparation.value; XCTFail("cancelled child cannot authorize attachment") }
                catch { /* URLSession cancellation retires the pending child. */ }
            } else {
                child?.finish(body: Data("#EXTM3U\n#EXTINF:6,\nsegment.ts\n".utf8))
                try await preparation.value
            }
        }
    }

    func testSplitMasterWaitsForTheDefaultAudioAndVideo() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [PreparationHTTP.self]
        let transport = URLSession(configuration: configuration)
        defer { PreparationHTTP.onRequest = nil; transport.invalidateAndCancel() }
        let url = try XCTUnwrap(URL(string: "http://media.test/hls/session/master.m3u8"))
        let audio = String(repeating: "a", count: 64)
        let video = String(repeating: "b", count: 64)
        let waitingForAudio = expectation(description: "default audio requested")
        let waitingForVideo = expectation(description: "video requested")
        PreparationHTTP.onRequest = { request in
            switch request.request.url?.path {
            case "/hls/session/master.m3u8":
                let master = """
                #EXTM3U
                #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID="audio",NAME="Default",DEFAULT=YES,STABLE-RENDITION-ID="a0",URI="audio/\(audio)/index.m3u8"
                #EXT-X-STREAM-INF:BANDWIDTH=1000000,AUDIO="audio"
                video/\(video)/index.m3u8
                """
                request.finish(body: Data(master.utf8))
            case "/hls/session/audio/\(audio)/index.m3u8":
                waitingForAudio.fulfill()
                request.finish(body: Data("#EXTM3U\n#EXTINF:1,\nsegment/0.m4s\n".utf8))
            case "/hls/session/video/\(video)/index.m3u8":
                waitingForVideo.fulfill()
                request.finish(body: Data("#EXTM3U\n#EXTINF:1,\nsegment/0.m4s\n".utf8))
            default:
                XCTFail("unexpected playlist")
                request.finish(status: 404, body: Data())
            }
        }
        try await GrowingPlaylistPreparation.wait(for: url, using: transport)
        await fulfillment(of: [waitingForAudio, waitingForVideo], timeout: 3)
    }

    func testOnlySuccessfulPlaylistResponseAllowsNativePreparation() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [PreparationHTTP.self]
        let transport = URLSession(configuration: configuration)
        defer { transport.invalidateAndCancel() }
        let url = URL(string: "http://media.test/hls/session/master.m3u8")!
        for (status, body, accepts) in [
            (200, "#EXTM3U\n#EXT-X-TARGETDURATION:16\n#EXTINF:6,\nsegment.ts\n", true),
            (200, "#EXTM3U\n#EXT-X-TARGETDURATION:16\n", false),
            (503, "{\"code\":\"startup_timeout\"}", false),
            (200, "<html>Server unavailable</html>", false)
        ] {
            PreparationHTTP.responseStatus = status
            PreparationHTTP.responseBody = Data(body.utf8)
            do {
                try await GrowingPlaylistPreparation.wait(for: url, using: transport)
                XCTAssertTrue(accepts, "failed preparation must not enter AVPlayer")
            } catch {
                XCTAssertFalse(accepts, "valid published playlist was refused: \(error)")
            }
        }
    }
}
