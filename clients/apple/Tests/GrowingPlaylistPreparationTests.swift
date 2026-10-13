import Foundation
import XCTest
@testable import plurx

private final class PreparationHTTP: URLProtocol {
    static var responseStatus = 200
    static var responseBody = Data()
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        client?.urlProtocol(self, didReceive: HTTPURLResponse(url: request.url!, statusCode: Self.responseStatus,
                            httpVersion: nil, headerFields: nil)!, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Self.responseBody)
        client?.urlProtocolDidFinishLoading(self)
    }
    override func stopLoading() {}
}

final class GrowingPlaylistPreparationTests: XCTestCase {
    func testOnlySuccessfulPlaylistResponseAllowsNativePreparation() async throws {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [PreparationHTTP.self]
        let transport = URLSession(configuration: configuration)
        defer { transport.invalidateAndCancel() }
        let url = URL(string: "http://media.test/hls/session/master.m3u8")!
        for (status, body, accepts) in [
            (200, "#EXTM3U\n#EXT-X-TARGETDURATION:16\n", true),
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
