import XCTest
@testable import plurx

/// Auto on an original or remux incumbent whose title has never been encoded
/// whole. Each test pins one decision point of the 2026-10-17 Agent Zero
/// finding (shipping build 252, 28.8 Mb/s H.264 remux, 10 / 25 Mb/s links):
/// the catalog's declared costs were discarded, the original was costed only
/// by a body less than ten seconds old, that body's verdict expired long
/// before the next 40 MB body could arrive, and the surface presenter kept
/// reading a stationary clock as presenting.
final class AutoDeclaredCostTests: XCTestCase {
    private func candidate(_ height: Int, route: String = "encode", peak: UInt64? = 3_000_000,
                           average: UInt64? = 2_000_000, grade: String = "sdr") -> QualityCandidate {
        QualityCandidate(id: "0a7ba9bab6fbdd31bab5e5e362a3fac7", recipeDigest: Array(repeating: 0, count: 32),
            route: route, width: height * 16 / 9, height: height, targetHeight: height,
            averageBps: average, peakBps: peak, grade: grade, decoderCompatible: true,
            completeCache: false, sustainable: false)
    }

    private func measured(_ candidate: QualityCandidate, average: UInt64, peak: UInt64) -> MeasuredCandidateOutput {
        MeasuredCandidateOutput(candidateId: candidate.id, recipeDigest: candidate.recipeDigest,
            route: candidate.route, artifactId: "00000000-0000-0000-0000-000000000001",
            outputIdentity: String(repeating: "0", count: 64), qualification: "complete_full_mux_rfc8216_v1",
            averageBps: average, peakBps: peak)
    }

    private func body(bodySeconds: Double, completedAt: Int = 0, bytes: Int64 = 40_000_000,
                      mediaSeconds: Double? = 11.5, receipt: String? = "00000000-0000-0000-0000-000000000007",
                      session: String? = "s", candidate: String? = "c") -> PlayerController.AutoCompletedTransfer {
        PlayerController.AutoCompletedTransfer(bodyBytes: bytes, bodyDurationSeconds: bodySeconds,
            completedAtMs: completedAt, origin: "https://node", networkLoad: true,
            fromLocalCache: false, producerPaced: false, statusCode: 200,
            segmentId: "https://node/api/v1/hls/s/seg00017.m4s", mediaDurationSeconds: mediaSeconds,
            receipt: receipt, etag: "object-17", installedSessionId: session, installedCandidateId: candidate)
    }

    private func playerControllerSource() throws -> String {
        let url = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../Sources/PlayerController.swift").standardizedFileURL
        return try String(contentsOf: url, encoding: .utf8)
    }

    private func functionBody(_ source: String, _ signature: String) throws -> String {
        let start = try XCTUnwrap(source.range(of: signature), signature)
        let end = try XCTUnwrap(source.range(of: "\n    }\n", range: start.upperBound..<source.endIndex))
        return String(source[start.upperBound..<end.lowerBound])
    }

    // MARK: Fix 1 — declared catalog costs stand until a measured output replaces them

    func testDeclaredCatalogCostsStandUntilAMeasuredOutputReplacesThem() {
        let rung = candidate(720, peak: 7_500_000, average: 5_000_000)
        // No complete full-mux measurement: the declared VBV ceiling plus
        // audio is the rung's cost. The old catalog overwrote it with nil.
        let declared = autoCostedCatalog([rung], outputs: nil)
        XCTAssertEqual(declared.first?.peakBps, 7_500_000)
        XCTAssertEqual(declared.first?.averageBps, 5_000_000)
        XCTAssertEqual(autoCostedCatalog([rung], outputs: []).first?.peakBps, 7_500_000)
        // A measured output is the exact cost and replaces both figures.
        let exact = autoCostedCatalog([rung], outputs: [measured(rung, average: 3_900_000, peak: 5_100_000)])
        XCTAssertEqual(exact.first?.peakBps, 5_100_000)
        XCTAssertEqual(exact.first?.averageBps, 3_900_000)
        // Two conflicting proofs are no proof; the declared cost stands.
        let conflicting = [measured(rung, average: 1, peak: 2), measured(rung, average: 3, peak: 4)]
        XCTAssertEqual(autoCostedCatalog([rung], outputs: conflicting).first?.peakBps, 7_500_000)
        // An original keeps its declared source rate and no peak.
        let original = candidate(1_080, route: "remux", peak: nil, average: 28_814_703)
        let costed = autoCostedCatalog([original], outputs: nil).first
        XCTAssertNil(costed?.peakBps)
        XCTAssertEqual(costed?.averageBps, 28_814_703)
    }

    func testARemuxUnderLinkPressureNowHasACostedLowerRungToRecoverTo() {
        // The Agent Zero ladder as the server declared it: a 28.8 Mb/s remux
        // incumbent and encode rungs costed by VBV ceiling plus audio, none
        // of them ever encoded whole. The link measured 8.69 Mb/s.
        let original = candidate(1_080, route: "remux", peak: nil, average: 28_814_703)
        let ladder = autoCostedCatalog([
            original, candidate(720, peak: 7_500_000), candidate(480, peak: 3_500_000),
            candidate(360, peak: 2_000_000),
        ], outputs: nil)
        XCTAssertEqual(autoRecoveryCandidate(ladder, current: original, rejected: [],
                                             linkCeiling: 8_690_000, currentCost: 28_814_703)?.height, 720)
        XCTAssertEqual(autoRecoveryCandidate(ladder, current: original, rejected: [],
                                             linkCeiling: 5_000_000, currentCost: 28_814_703)?.height, 480)
    }
}
