import XCTest
@testable import plurx

final class EffectiveProcessingPresentationTests: XCTestCase {
    let generation = "11111111-1111-4111-8111-111111111111"
    func report() -> EffectiveProcessingReport {
        // Synthetic wire data exercises presentation, never production admission.
        EffectiveProcessingReport(generation: generation, hdr10Enhanced: true,
            felContributed: true, appliedOperations: ["PolynomialReshape", "RpuColorConversion"])
    }
    func testDisplayTargetOperationObjectRetainsCurrentReport() throws {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let wire = """
        {"generation":"\(generation)","hdr10_enhanced":true,"fel_contributed":false,
         "applied_operations":["RpuColorConversion","TargetMapping",{"DisplayTargetMapping":{"nits":1600}}]}
        """
        let report = try decoder.decode(EffectiveProcessingReport.self, from: Data(wire.utf8))
        XCTAssertTrue(report.matches(generation: generation, delivered: "hdr10"))
        XCTAssertTrue(report.appliedOperations.contains("DisplayTargetMapping:1600"))
    }
    func testSdrReceiptRequiresItsGradeAndProducesSdrMark() throws {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        let wire = """
        {"generation":"\(generation)","hdr10_enhanced":false,"sdr_processed":true,"fel_contributed":false,
         "applied_operations":["RpuColorConversion","TargetMapping","Bt709Gamma24Conversion",{"DisplayTargetMapping":{"nits":100}}]}
        """
        let report = try decoder.decode(EffectiveProcessingReport.self, from: Data(wire.utf8))
        XCTAssertTrue(report.matches(generation: generation, delivered: "sdr"))
        XCTAssertFalse(report.matches(generation: generation, delivered: "hdr10"))
        let badge = PlayerView.dynamicRangeBadge(hdr: "dolby_vision", hdrFormat: "Dolby Vision Profile 7", delivered: "sdr", displayHDR: false, effectiveProcessing: report, activeGeneration: generation)
        XCTAssertEqual(badge?.renderedMark, "SDR-E")
        XCTAssertTrue(report.detail.contains("L1 only"))
        let trimmedWire = wire
            .replacingOccurrences(of: "\"sdr_processed\":true,", with: "\"sdr_processed\":true,\"creative_trims_applied\":true,")
            .replacingOccurrences(of: "\"TargetMapping\",", with: "\"TargetMapping\",\"CreativeTrimApplication\",")
        let trimmed = try decoder.decode(EffectiveProcessingReport.self, from: Data(trimmedWire.utf8))
        XCTAssertTrue(trimmed.matches(generation: generation, delivered: "sdr"))
        XCTAssertTrue(trimmed.detail.contains("BT.709 gamma 2.4, creative trims applied"))
        XCTAssertFalse(trimmed.detail.contains("L1 only"))
        let partialWire = wire.replacingOccurrences(of: "\"sdr_processed\":true,", with: "\"sdr_processed\":true,\"creative_trims_partial\":true,")
        let partial = try decoder.decode(EffectiveProcessingReport.self, from: Data(partialWire.utf8))
        XCTAssertTrue(partial.matches(generation: generation, delivered: "sdr"))
        XCTAssertTrue(partial.detail.contains("BT.709 gamma 2.4, creative trims on some shots"))
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
            }, prepareGrowingPlaylist: { _ in }, requestHlsSession: { _, _, _ in
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

    // MARK: - Dolby Vision enhancement (FEL / RPU / DYN)

    func decodeStart(_ extra: String) throws -> HlsStart {
        let decoder = JSONDecoder(); decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(HlsStart.self, from: Data(
            "{\"session_id\":\"s\",\"playlist_url\":\"/p\"\(extra)}".utf8))
    }

    let dvSource = SourceSummary(container: nil, videoCodec: "hevc", videoProfile: nil, width: 3840,
        height: 2160, bitDepth: 10, hdr: "dolby_vision", hdrFormat: "Dolby Vision Profile 7",
        bitrate: nil, durationMs: nil)

    func enhancement(_ active: [PlaybackEnhancement.Badge], requires: Bool = false) -> PlaybackEnhancement {
        PlaybackEnhancement(active: active, details: [
            .init(badge: "FEL", active: active.contains(.fel), text: "FEL: full enhancement layer reconstructed into the picture"),
            .init(badge: "RPU", active: active.contains(.rpu), text: "RPU: reshaping applied by the server"),
            .init(badge: "DYN", active: active.contains(.dyn), text: "DYN: L1/L2/L8 dynamic metadata carried to the display"),
        ], requiresEffectiveProcessing: requires)
    }

    func testEnhancementDecodesPresentAbsentUnknownAndMalformed() throws {
        let present = try decodeStart("""
        ,"enhancement":{"active":["FEL","XFEL","RPU","DYN","FEL"],
          "details":[{"badge":"FEL","active":true,"text":"FEL: in the picture"},
                     {"badge":"XFEL","active":true,"text":"XFEL: a future badge"},
                     {"badge":"DYN","active":true,"text":"  "}],
          "requires_effective_processing":true}
        """)
        let decoded = try XCTUnwrap(present.enhancement)
        // Unknown badges are dropped from the pills, duplicates collapse, order is kept.
        XCTAssertEqual(decoded.active, [.fel, .rpu, .dyn])
        XCTAssertTrue(decoded.requiresEffectiveProcessing)
        // The server's lines stay readable even under a badge this client does not know.
        XCTAssertEqual(decoded.detailLines, ["FEL: in the picture", "XFEL: a future badge"])

        let omitted = try decodeStart(",\"enhancement\":{\"active\":[\"DYN\"],\"details\":[]}")
        XCTAssertEqual(omitted.enhancement?.requiresEffectiveProcessing, false)

        XCTAssertNil(try decodeStart("").enhancement)
        XCTAssertNil(try decodeStart(",\"enhancement\":null").enhancement)

        // Additive: a malformed answer never refuses the session and shows nothing.
        for value in ["true", "\"FEL\"", "{\"active\":\"FEL\",\"details\":7}", "{\"details\":[3]}"] {
            let hls = try decodeStart(",\"enhancement\":\(value)")
            XCTAssertNil(hls.enhancement?.presented(effectiveProcessing: nil, activeGeneration: nil,
                                                    delivered: "dolby_vision"), value)
            XCTAssertEqual(PlayerView.enhancementBadges(hls.enhancement, effectiveProcessing: nil,
                activeGeneration: nil, delivered: "dolby_vision"), [], value)
        }
    }

    func testEnhancementPillsFollowTheRangeBadgeOnlyForActiveEnhancements() {
        let badges = PlayerView.playbackBadges(source: dvSource, audio: nil, delivered: "dolby_vision",
            displayHDR: true, enhancement: enhancement([.dyn]))
        XCTAssertEqual(badges.map(\.kind), [.resolution, .dynamicRange, .enhancement])
        XCTAssertEqual(badges.last?.mark, "DYN")
        XCTAssertEqual(badges.last?.tone, .enhancement)
        XCTAssertNil(badges.last?.renderedMark)
        XCTAssertFalse(badges.last?.dimmed ?? true)
        XCTAssertEqual(badges.last?.accessibilityLabel, "DYN: L1/L2/L8 dynamic metadata carried to the display")

        let all = PlayerView.enhancementBadges(enhancement([.fel, .rpu, .dyn]), effectiveProcessing: nil,
            activeGeneration: nil, delivered: "dolby_vision")
        XCTAssertEqual(all.map(\.mark), ["FEL", "RPU", "DYN"])
        XCTAssertEqual(Set(all.map(\.tone)), [.enhancement])
        XCTAssertEqual(Set(all.map(\.id)).count, 3, "each pill needs its own identity in the row")

        // Nothing active: no pill, no reason text in the row.
        let none = PlayerView.playbackBadges(source: dvSource, audio: nil, delivered: "dolby_vision",
            displayHDR: true, enhancement: enhancement([]))
        XCTAssertEqual(none.map(\.kind), [.resolution, .dynamicRange])
        // No answer at all leaves today's row unchanged.
        XCTAssertEqual(PlayerView.playbackBadges(source: dvSource, audio: nil, delivered: "dolby_vision").map(\.kind),
                       [.resolution, .dynamicRange])
    }

    func testReportGatedEnhancementShowsOnlyWithTheCurrentReport() {
        let gated = enhancement([.fel, .rpu, .dyn], requires: true)
        let stale = "22222222-2222-4222-8222-222222222222"
        for (receipt, current, delivered) in [
            (nil, generation, "hdr10"), (report(), stale, "hdr10"),
            (report(), nil, "hdr10"), (report(), generation, "sdr"),
        ] as [(EffectiveProcessingReport?, String?, String)] {
            XCTAssertEqual(PlayerView.enhancementBadges(gated, effectiveProcessing: receipt,
                activeGeneration: current, delivered: delivered), [])
            XCTAssertNil(PlayerView.enhancementSummary(gated, effectiveProcessing: receipt,
                activeGeneration: current, delivered: delivered), "no row without a current report")
        }
        let shown = PlayerView.playbackBadges(source: dvSource, audio: nil, delivered: "hdr10",
            displayHDR: true, effectiveProcessing: report(), activeGeneration: generation, enhancement: gated)
        XCTAssertEqual(shown.map(\.kind), [.resolution, .dynamicRange, .enhancement, .enhancement, .enhancement])
        XCTAssertEqual(shown.compactMap { $0.kind == .enhancement ? $0.mark : nil }, ["FEL", "RPU", "DYN"])
        XCTAssertEqual(shown[1].renderedMark, "HDR10-E")
    }

    func testEnhancementInfoRowListsEveryDetailAsItsOwnLine() throws {
        let row = try XCTUnwrap(PlayerView.enhancementSummary(enhancement([.fel, .dyn]),
            effectiveProcessing: nil, activeGeneration: nil, delivered: "dolby_vision"))
        XCTAssertEqual(row.value, "FEL · DYN")
        XCTAssertEqual(row.lines, [
            "FEL: full enhancement layer reconstructed into the picture",
            "RPU: reshaping applied by the server",
            "DYN: L1/L2/L8 dynamic metadata carried to the display",
        ])
        let idle = try XCTUnwrap(PlayerView.enhancementSummary(enhancement([]),
            effectiveProcessing: nil, activeGeneration: nil, delivered: "hdr10"))
        XCTAssertEqual(idle.value, "None active")
        XCTAssertEqual(idle.lines.count, 3)
        XCTAssertNil(PlayerView.enhancementSummary(nil, effectiveProcessing: nil,
            activeGeneration: nil, delivered: "dolby_vision"))
        XCTAssertNil(PlayerView.enhancementSummary(PlaybackEnhancement(active: [], details: []),
            effectiveProcessing: nil, activeGeneration: nil, delivered: "dolby_vision"))
        let field = try XCTUnwrap(applePlaybackInfoFields.first { $0.id == "enhancement" })
        XCTAssertEqual(field.label, "Enhancement")
        XCTAssertEqual(field.section, "NOW DECODING")
        let ids = applePlaybackInfoFields.map(\.id)
        XCTAssertEqual(ids.firstIndex(of: "enhancement"), ids.firstIndex(of: "dynamic_range").map { $0 + 1 })
    }

    @MainActor
    func testControllerHoldsEnhancementAcrossSeekAndClearsItOnStop() async throws {
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let decision = try decoder.decode(Decision.self, from: Data("""
        {"file_id":1,"method":"transcode","play_url":"/unused"}
        """.utf8))
        let hls = try decodeStart("""
        ,"delivered_dynamic_range":"hdr10",
         "control":{"protocol":"plurx-playback-control-v1","url":"/api/v1/hls/s/control",
                    "generation":"\(generation)","control_epoch":1,"next_exchange_ms":5000,
                    "lease_timeout_ms":300000},
         "effective_processing":{"generation":"\(generation)","hdr10_enhanced":true,
                                 "fel_contributed":true,"applied_operations":["RpuColorConversion"]},
         "enhancement":{"active":["FEL","RPU","DYN"],"details":[],"requires_effective_processing":true}
        """)
        let model = AppModel(startServices: false)
        let controller = PlayerController(requestPlaybackDecision: { _, _, _, _ in
            (decision, model.caps())
        }, prepareGrowingPlaylist: { _ in }, requestHlsSession: { _, _, _ in
            throw APIError.transport("fixture stops before network attachment")
        })
        controller.start(model: model, itemId: 1, fileId: 1, startMs: 0,
                         durationMs: 600000, title: "Enhancement lifetime")
        await controller.loadingTask?.value
        controller.adoptEffectiveProcessing(hls)
        XCTAssertEqual(controller.playbackEnhancement?.active, [.fel, .rpu, .dyn])
        XCTAssertNotNil(controller.playbackEnhancement?.presented(
            effectiveProcessing: controller.effectiveProcessing,
            activeGeneration: controller.effectiveProcessingGeneration, delivered: "hdr10"))
        controller.skip(seconds: 10)
        // The seek drops the report, which alone hides the report-gated pills.
        XCTAssertNotNil(controller.playbackEnhancement)
        XCTAssertNil(controller.playbackEnhancement?.presented(
            effectiveProcessing: controller.effectiveProcessing,
            activeGeneration: controller.effectiveProcessingGeneration, delivered: "hdr10"))
        controller.stop()
        XCTAssertNil(controller.playbackEnhancement)
    }

    private func decodePrepare(_ extra: String) throws -> ControlAction {
        let id = "6f1d2a44-2b7e-4a1c-9f3e-2c5a7b8d9e01", session = "0a9b8c7d-6e5f-4a3b-8c2d-1e0f9a8b7c6d"
        return try PlaybackControl.decoder.decode(ControlAction.self, from: Data("""
        {"type":"prepare","action_id":"\(id)","session_id":"\(session)",
         "playlist_url":"/api/v1/hls/\(session)/index.m3u8","media_origin_ms":0,
         "effective_selection":{"quality_auto":true,"height":1080,"audio_track":null,
           "subtitle_burn":null,"audio_offset_ms":0,"codec":"server_selected","dynamic_range":"hdr10"}\(extra)}
        """.utf8))
    }

    func testPreparedSuccessorCarriesItsOwnEnhancementAndFallsBackToNone() throws {
        let withEnhancement = try XCTUnwrap(PreparedReplacementAction(try decodePrepare("""
        ,"enhancement":{"active":["FEL","RPU"],"requires_effective_processing":true,
          "details":[{"badge":"FEL","active":true,"text":"FEL: in the picture"}]}
        """)))
        XCTAssertEqual(withEnhancement.enhancement?.active, [.fel, .rpu])
        XCTAssertEqual(withEnhancement.enhancement?.requiresEffectiveProcessing, true)
        // The report gate is unchanged: without a current report nothing is shown.
        XCTAssertNil(withEnhancement.enhancement?.presented(
            effectiveProcessing: nil, activeGeneration: generation, delivered: "hdr10"))
        XCTAssertNotNil(withEnhancement.enhancement?.presented(
            effectiveProcessing: report(), activeGeneration: generation, delivered: "hdr10"))

        let without = try XCTUnwrap(PreparedReplacementAction(try decodePrepare("")))
        XCTAssertNil(without.enhancement)
        // A malformed value is additive: the action stays valid and shows nothing.
        let malformed = try XCTUnwrap(PreparedReplacementAction(try decodePrepare(",\"enhancement\":\"FEL\"")))
        XCTAssertNil(malformed.enhancement?.presented(
            effectiveProcessing: nil, activeGeneration: nil, delivered: "hdr10"))
    }

    func testCommitAdoptsTheSuccessorsEnhancementInsteadOfClearingIt() throws {
        let source = try String(contentsOf: URL(fileURLWithPath: #filePath)
            .deletingLastPathComponent().appendingPathComponent("../Sources/PlayerController.swift")
            .standardizedFileURL, encoding: .utf8)
        let commit = try XCTUnwrap(source.range(of: "func commitPreparedSuccessor("))
        let body = source[commit.upperBound...]
        let adopt = try XCTUnwrap(body.range(of: "playbackEnhancement = action.enhancement"))
        let clear = try XCTUnwrap(body.range(of: "effectiveProcessingGeneration = nil"))
        XCTAssertLessThan(clear.lowerBound, adopt.lowerBound, "the report is cleared first")
    }

}
