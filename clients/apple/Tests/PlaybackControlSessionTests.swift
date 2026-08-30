import Foundation
import XCTest
@testable import plurx

// MARK: - Fixtures

/// Every control request the stub server saw.
///
/// A class with a lock rather than a static var because `startLoading()` runs
/// on URLSession's own queue while the test reads from the main actor — which
/// is the whole point of these tests.
private final class ControlExchangeLog: @unchecked Sendable {
    private let lock = NSLock()
    private var requests: [ControlRequest] = []

    func append(_ request: ControlRequest) {
        lock.lock()
        defer { lock.unlock() }
        requests.append(request)
    }

    func all() -> [ControlRequest] {
        lock.lock()
        defer { lock.unlock() }
        return requests
    }

    func reset() {
        lock.lock()
        defer { lock.unlock() }
        requests = []
    }
}

private let controlExchanges = ControlExchangeLog()

/// Accepts every exchange the way the server does, and records what it carried.
private final class ControlExchangeURLProtocol: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        guard let url = request.url,
              let decoded = try? PlaybackControl.decoder.decode(
                  ControlRequest.self,
                  from: Self.body(of: request)
              ),
              let http = HTTPURLResponse(
                  url: url,
                  statusCode: 200,
                  httpVersion: "HTTP/1.1",
                  headerFields: ["Content-Type": "application/json"]
              )
        else {
            client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse))
            return
        }
        controlExchanges.append(decoded)
        let response = ControlResponse(
            proto: PlaybackControl.protocolName,
            generation: decoded.generation,
            controlEpoch: decoded.controlEpoch,
            acceptedSequence: decoded.sequence,
            action: ControlAction(type: "none")
        )
        let body = (try? PlaybackControl.encoder.encode(response)) ?? Data()
        client?.urlProtocol(self, didReceive: http, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: body)
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    /// URLSession hands a body to a protocol as a stream, not as `httpBody`.
    private static func body(of request: URLRequest) -> Data {
        if let body = request.httpBody { return body }
        guard let stream = request.httpBodyStream else { return Data() }
        stream.open()
        defer { stream.close() }
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 4_096)
        while stream.hasBytesAvailable {
            let read = stream.read(&buffer, maxLength: buffer.count)
            if read <= 0 { break }
            data.append(buffer, count: read)
        }
        return data
    }
}

/// What the player would report. Held in a class so a test can move the
/// playhead between exchanges the way the player does.
@MainActor
private final class PlayerStub {
    var positionMs = 4_000
    var reads = 0

    func observation() -> PlayerControlObservation? {
        reads += 1
        return PlayerControlObservation(
            positionMs: positionMs,
            durationMs: 7_200_000,
            bufferedFromMs: positionMs,
            bufferedThroughMs: positionMs + 30_000,
            rate: 1,
            isPaused: false,
            isEnded: false,
            isSeeking: false,
            hasStarted: true,
            waitingForMs: nil,
            isLikelyToKeepUp: true,
            errorCode: nil,
            errorDetail: nil,
            droppedFrames: nil,
            observedDownloadBps: nil,
            observationOverride: nil,
            renderOverride: nil,
            selection: ClientSelection(
                quality: .auto,
                audioTrack: 0,
                subtitle: SubtitleSelection(mode: .off, track: nil),
                audioOffsetMs: 0,
                codec: .auto,
                dynamicRange: .auto
            ),
            capabilities: DynamicCapabilities(
                platform: "apple",
                maxHeight: 2_160,
                codecs: [.hevc, .h264],
                dynamicRanges: [.hdr10, .sdr],
                dualPlayerPreparation: false
            )
        )
    }
}

/// The exchange cadence is the protocol minimum so a second exchange is a
/// quarter of a second away rather than five seconds.
private struct ExchangeTimeout: Error {}

private func sessionBootstrap() -> ControlBootstrap {
    ControlBootstrap(
        proto: PlaybackControl.protocolName,
        url: "/api/v1/hls/session-1/control",
        generation: "11111111-1111-4111-8111-111111111111",
        controlEpoch: 7,
        nextExchangeMs: PlaybackControl.minimumExchangeMs,
        leaseTimeoutMs: 300_000
    )
}

// MARK: - Tests

/// The seam between `PlayerController` and the reporter, driven end to end.
///
/// These tests exist because everything either side of the seam was covered
/// and the seam itself was not: the reporter's own suite hands it a snapshot
/// closure of the test's making, so nothing ever ran the closure the session
/// actually installs. That closure asked to be on the main actor while running
/// on the reporter's, and `MainActor.assumeIsolated` traps when it is wrong —
/// so build 90 died the instant a controllable session opened. A test that
/// stubs the closure would leave that free again; these drive the real session
/// through the real actor.
@MainActor
final class PlaybackControlSessionTests: XCTestCase {
    private func makeTransport() -> (PlaybackControlTransport, URLSession) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.protocolClasses = [ControlExchangeURLProtocol.self]
        let urlSession = URLSession(configuration: configuration)
        return (
            PlaybackControlTransport(
                origin: "https://cinema.example",
                authorize: { _ in },
                session: urlSession
            ),
            urlSession
        )
    }

    private func waitForExchange(
        timeout: TimeInterval = 5,
        where predicate: (ControlRequest) -> Bool
    ) async throws -> ControlRequest {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if let match = controlExchanges.all().first(where: predicate) { return match }
            try await Task.sleep(nanoseconds: 20_000_000)
        }
        XCTFail("no matching control exchange arrived within \(timeout)s")
        throw ExchangeTimeout()
    }

    func testAStartedSessionExchangesWhatThePlayerReported() async throws {
        controlExchanges.reset()
        let player = PlayerStub()
        let (transport, urlSession) = makeTransport()
        defer { urlSession.invalidateAndCancel() }
        let session = PlaybackControlSession()

        session.begin(
            bootstrap: sessionBootstrap(),
            transport: transport,
            observe: { player.observation() }
        )
        let first = try await waitForExchange { $0.sequence == 1 }
        session.end()

        // The reporter takes this snapshot on its own executor. Reaching back
        // into the player from there is what crashed the app, so the position
        // arriving at all is the assertion.
        XCTAssertEqual(first.positionMs, 4_000)
        XCTAssertEqual(first.demand, .active)
        XCTAssertEqual(first.renderState, .rendering)
        XCTAssertEqual(first.generation, "11111111-1111-4111-8111-111111111111")
        XCTAssertEqual(first.controlEpoch, 7)
        XCTAssertNotNil(first.capabilities)
    }

    func testTheNextExchangeCarriesWhereThePlayerMovedTo() async throws {
        controlExchanges.reset()
        let player = PlayerStub()
        let (transport, urlSession) = makeTransport()
        defer { urlSession.invalidateAndCancel() }
        let session = PlaybackControlSession()

        session.begin(
            bootstrap: sessionBootstrap(),
            transport: transport,
            observe: { player.observation() }
        )
        _ = try await waitForExchange { $0.sequence == 1 }

        player.positionMs = 9_000
        session.playerChanged()
        let moved = try await waitForExchange { $0.positionMs == 9_000 }
        session.end()

        XCTAssertGreaterThan(moved.sequence, 1)
    }

    func testEndingTheSessionStopsReporting() async throws {
        controlExchanges.reset()
        let player = PlayerStub()
        let (transport, urlSession) = makeTransport()
        defer { urlSession.invalidateAndCancel() }
        let session = PlaybackControlSession()

        session.begin(
            bootstrap: sessionBootstrap(),
            transport: transport,
            observe: { player.observation() }
        )
        _ = try await waitForExchange { $0.sequence == 1 }
        XCTAssertTrue(session.isReporting)
        session.end()
        XCTAssertFalse(session.isReporting)

        let settled = controlExchanges.all().count
        try await Task.sleep(nanoseconds: 600_000_000)
        XCTAssertLessThanOrEqual(controlExchanges.all().count, settled + 1)
    }

    func testABootstrapTheClientCannotAddressLeavesTheSessionSilent() async throws {
        controlExchanges.reset()
        let player = PlayerStub()
        let (transport, urlSession) = makeTransport()
        defer { urlSession.invalidateAndCancel() }
        var unusable = sessionBootstrap()
        unusable.url = "/api/v1/hls/session-1/notcontrol"
        let session = PlaybackControlSession()

        session.begin(bootstrap: unusable, transport: transport, observe: { player.observation() })

        XCTAssertFalse(session.isReporting)
        try await Task.sleep(nanoseconds: 400_000_000)
        XCTAssertTrue(controlExchanges.all().isEmpty)
        session.end()
    }
}
