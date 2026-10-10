import XCTest
@testable import plurx

final class EffectiveProcessingPresentationTests: XCTestCase {
    let generation = "11111111-1111-4111-8111-111111111111"
    func report() -> EffectiveProcessingReport {
        // Synthetic wire data exercises presentation, never production admission.
        EffectiveProcessingReport(generation: generation, hdr10Enhanced: true,
            felContributed: true, appliedOperations: ["PolynomialReshape", "RpuColorConversion"])
    }
    func testEnhancementRequiresActiveReceiptGenerationAndKeepsFelSeparate() {
        let accepted = report()
        let badge = PlayerView.dynamicRangeBadge(hdr: "dolby_vision", hdrFormat: "Dolby Vision Profile 7",
            delivered: "hdr10", displayHDR: true, effectiveProcessing: accepted, activeGeneration: generation)
        XCTAssertEqual(badge?.renderedMark, "HDR10-E")
        XCTAssertTrue(badge?.accessibilityLabel.contains("FEL used: yes") == true)
        var base = accepted; base.felContributed = false
        XCTAssertTrue(base.matches(generation: generation, delivered: "hdr10"))
        XCTAssertTrue(base.detail.contains("FEL used: no"))
    }
    func testAbsentStaleFallbackAndNativeDvNeverRetainEnhancement() {
        for (receipt, current, delivered) in [
            (nil, generation, "hdr10"),
            (report(), "22222222-2222-4222-8222-222222222222", "hdr10"),
            (report(), generation, "dolby_vision"), (report(), generation, "sdr")
        ] as [(EffectiveProcessingReport?, String, String)] {
            let badge = PlayerView.dynamicRangeBadge(hdr: "dolby_vision", hdrFormat: "Dolby Vision Profile 7",
                delivered: delivered, displayHDR: true, effectiveProcessing: receipt, activeGeneration: current)
            XCTAssertNotEqual(badge?.renderedMark, "HDR10-E")
        }
    }
    func testMalformedAdditiveReportDoesNotRefuseLegacyPlaybackOrAwardEnhancement() throws {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        for value in ["true", "{\"generation\":\"\(generation)\",\"hdr10_enhanced\":true,\"applied_operations\":[\"RpuColorConversion\"]}"] {
            let hls = try decoder.decode(HlsStart.self, from: Data("{\"session_id\":\"s\",\"playlist_url\":\"/p\",\"effective_processing\":\(value)}".utf8))
            XCTAssertFalse(hls.effectiveProcessing?.matches(generation: generation, delivered: "hdr10") ?? false)
        }
    }

    @MainActor
    func testRelativeAndMarkerSeeksClearTheActualControllerReport() async throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let decision = try decoder.decode(Decision.self, from: Data("""
        {"file_id":1,"method":"transcode","play_url":"/unused",
         "markers":[{"kind":"intro","label":"Intro","start_ms":0,"end_ms":90000,
                     "provenance":"authored","confidence":100}]}
        """.utf8))
        let hls = try decoder.decode(HlsStart.self, from: Data("""
        {"session_id":"s","playlist_url":"/unused","delivered_dynamic_range":"hdr10",
         "control":{"protocol":"plurx-playback-control-v1","url":"/api/v1/hls/s/control",
                    "generation":"\(generation)","control_epoch":1,"next_exchange_ms":5000,
                    "lease_timeout_ms":300000},
         "effective_processing":{"generation":"\(generation)","hdr10_enhanced":true,
                                 "fel_contributed":true,"applied_operations":["RpuColorConversion"]}}
        """.utf8))
        for action in ["relative", "marker", "automatic"] {
            let model = AppModel(startServices: false)
            model.autoSkip = true
            let controller = PlayerController(requestPlaybackDecision: { _, _, _, _ in
                (decision, model.caps())
            }, requestHlsSession: { _, _, _ in
                throw APIError.transport("fixture stops before network attachment")
            })
            controller.start(model: model, itemId: 1, fileId: 1, startMs: 0,
                             durationMs: 600000, title: "Seek report reset")
            await controller.loadingTask?.value
            controller.adoptEffectiveProcessing(hls)
            XCTAssertNotNil(controller.effectiveProcessing, action)
            XCTAssertNotNil(controller.activeMarker, action)
            switch action {
            case "relative": controller.skip(seconds: 10)
            case "marker": controller.skipActiveMarker()
            default: controller.autoSkipActiveMarkerIfNeeded()
            }
            XCTAssertNil(controller.effectiveProcessing, action)
            XCTAssertNil(controller.effectiveProcessingGeneration, action)
            controller.stop()
        }
    }

}
