import Foundation
import XCTest
@testable import plurx

/// The transport's whole job is turning an HTTP response into the fields the
/// reporter classifies on. Getting these wrong makes a retryable refusal look
/// terminal — the reporter would stop reporting for the rest of a film — or a
/// terminal one look retryable, which is a client hammering a server that has
/// already said no.
final class PlaybackControlTransportTests: XCTestCase {
    private func failure(_ status: Int, _ json: String) -> ControlTransportError {
        PlaybackControlTransport.failure(status: status, body: Data(json.utf8))
    }

    func testAStatusSurvivesABodyThatSaysNothing() {
        for body in ["", "not json at all", "[]", "null", "{}"] {
            let value = PlaybackControlTransport.failure(status: 503, body: Data(body.utf8))
            XCTAssertEqual(value.status, 503, "body was \(body)")
            XCTAssertNil(value.code)
        }
    }

    func testATypedRefusalCarriesItsCode() {
        let value = failure(429, #"{"code":"control_rate_limited","message":"slow down"}"#)
        XCTAssertEqual(value.status, 429)
        XCTAssertEqual(value.code, "control_rate_limited")
        XCTAssertNil(value.retryAfterMs)
    }

    func testARetryAfterIsCarriedThrough() {
        let value = failure(503, #"{"code":"control_unavailable","retry_after_ms":4000}"#)
        XCTAssertEqual(value.retryAfterMs, 4_000)
    }

    func testAnOwnerChangeCarriesTheNewOwner() {
        let value = failure(409, """
        {"code":"owner_changed","message":"moved",
         "generation":"44444444-4444-4444-8444-444444444444","control_epoch":9}
        """)
        XCTAssertEqual(value.status, 409)
        XCTAssertEqual(value.code, "owner_changed")
        XCTAssertEqual(value.generation, "44444444-4444-4444-8444-444444444444")
        XCTAssertEqual(value.controlEpoch, 9)
    }

    func testAFieldOfTheWrongTypeIsIgnoredRatherThanCrashingTheExchange() {
        let value = failure(409, """
        {"code":"owner_changed","generation":42,"control_epoch":"nine","retry_after_ms":"soon"}
        """)
        XCTAssertEqual(value.code, "owner_changed")
        XCTAssertNil(value.generation, "a numeric generation is not a generation")
        XCTAssertNil(value.controlEpoch)
        XCTAssertNil(value.retryAfterMs)
    }

    func testTheClassificationsTheReporterDependsOnRoundTrip() {
        // These four are the whole retryable set. If a rename on the server
        // ever breaks one, this is where it shows up rather than in a client
        // that quietly stopped reporting.
        XCTAssertEqual(failure(425, #"{"code":"owner_transition"}"#).code, "owner_transition")
        XCTAssertEqual(failure(429, #"{"code":"control_rate_limited"}"#).code, "control_rate_limited")
        XCTAssertEqual(failure(503, #"{"code":"control_unavailable"}"#).code, "control_unavailable")
        XCTAssertEqual(failure(409, #"{"code":"owner_changed"}"#).code, "owner_changed")
    }

    func testAControlPathIsRequiredBeforeAnyRequestIsBuilt() async {
        let transport = PlaybackControlTransport(
            origin: "https://media.example",
            authorize: { _ in },
            session: .shared
        )
        for path in [
            "/api/v1/hls/session-1/status",
            "https://elsewhere.example/api/v1/hls/session-1/control",
            "/api/v1/hls/session-1/control/extra",
        ] {
            do {
                _ = try await transport.send(path, sampleRequest())
                XCTFail("sent to \(path)")
            } catch let error as ControlProtocolError {
                XCTAssertEqual(error.reason, "url")
            } catch {
                XCTFail("wrong error for \(path): \(error)")
            }
        }
    }

    private func sampleRequest() -> ControlRequest {
        ControlRequest(
            proto: PlaybackControl.protocolName,
            generation: "11111111-1111-4111-8111-111111111111",
            controlEpoch: 1,
            clientInstanceId: "22222222-2222-4222-8222-222222222222",
            sequence: 1,
            demand: .active,
            positionMs: 0,
            bufferedFromMs: nil,
            bufferedThroughMs: 0,
            playbackRate: 1,
            renderState: .starting,
            seekTargetMs: nil,
            observedDownloadBps: nil,
            selection: ClientSelection(
                quality: .auto,
                audioTrack: nil,
                subtitle: SubtitleSelection(mode: .off, track: nil),
                audioOffsetMs: 0,
                codec: .auto,
                dynamicRange: .auto
            ),
            capabilities: DynamicCapabilities(
                platform: "apple",
                maxHeight: 1_080,
                codecs: [.h264],
                dynamicRanges: [.sdr],
                dualPlayerPreparation: false
            ),
            observation: nil
        )
    }
}
