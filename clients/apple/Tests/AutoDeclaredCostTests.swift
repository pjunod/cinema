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

    // MARK: Fix 2 — an original is costed by its declared source rate, and pressure does not wait for the runway

    func testAnOriginalIsCostedByItsDeclaredSourceRateWithoutAFreshBody() {
        let original = candidate(1_080, route: "remux", peak: nil, average: 28_814_703)
        // No body at all, a stale body, a body of another session: the
        // declared rate is the cost every time. The old rule answered nil to
        // all three and left the incumbent uncosted between bodies.
        XCTAssertEqual(autoDownsideCostBps(original, sample: nil, sessionId: "s", nowMs: 0), 28_814_703)
        XCTAssertEqual(autoDownsideCostBps(original, sample: body(bodySeconds: 1), sessionId: "s", nowMs: 60_000), 28_814_703)
        XCTAssertEqual(autoDownsideCostBps(original, sample: body(bodySeconds: 1), sessionId: "other", nowMs: 500), 28_814_703)
        // A fresh body does not raise the declared rate: one object's wire
        // rate is a peak, the catalog's figure is the title's sustained cost.
        XCTAssertEqual(autoDownsideCostBps(original, sample: body(bodySeconds: 1, mediaSeconds: 1), sessionId: "s", nowMs: 500), 28_814_703)
        // Only a title the server declared no rate for is costed by a body,
        // and then only a fresh unpaced one of its own session.
        let undeclared = candidate(1_080, route: "remux", peak: nil, average: nil)
        XCTAssertEqual(autoDownsideCostBps(undeclared, sample: body(bodySeconds: 1, bytes: 1_000_000, mediaSeconds: 1),
                                           sessionId: "s", nowMs: 500), 8_000_000)
        XCTAssertNil(autoDownsideCostBps(undeclared, sample: body(bodySeconds: 1), sessionId: "s", nowMs: 60_000))
        XCTAssertNil(autoDownsideCostBps(undeclared, sample: body(bodySeconds: 1), sessionId: "other", nowMs: 500))
        XCTAssertNil(autoDownsideCostBps(undeclared, sample: nil, sessionId: "s", nowMs: 0))
        // An encode rung is costed by its peak alone.
        XCTAssertEqual(autoDownsideCostBps(candidate(720, peak: 7_500_000), sample: nil, sessionId: "s", nowMs: 0), 7_500_000)
        XCTAssertNil(autoDownsideCostBps(candidate(720, peak: nil), sample: body(bodySeconds: 1), sessionId: "s", nowMs: 500))
    }

    func testLinkPressureOnACostedIncumbentDoesNotWaitForTheRunwayToDrain() throws {
        let tick = try functionBody(try playerControllerSource(),
                                    "private func tickDisplayAwareAuto(pressureOnly: Bool = false) {")
        XCTAssertTrue(
            tick.contains("let pressure = severe || (mild && autoMildSamples >= 2) || producerPressure"),
            "a severe link, or two mild bodies, is pressure on its own"
        )
        XCTAssertFalse(
            tick.contains("(current.peakBps != nil || (bufferedRunwaySeconds() ?? 0) < 10))"),
            "the runway gate held a 28.8 Mb/s remux on a 9 Mb/s link until the buffer was one body from empty"
        )
    }

    // MARK: Fix 3 — a completed body is evidence for as long as it took to arrive

    func testACompletedBodyStaysLinkEvidenceForAsLongAsItTookToArrive() {
        XCTAssertEqual(autoCompletedTransferValidityMs(body(bodySeconds: 37)), 37_000)
        XCTAssertEqual(autoCompletedTransferValidityMs(body(bodySeconds: 0.5)), 10_000)
        XCTAssertEqual(autoCompletedTransferValidityMs(body(bodySeconds: 12), maximumAgeMs: 15_000), 15_000)
        // A 40 MB object that took 37 s to cross the link is 37 s of
        // evidence; the old ten-second expiry discarded it at ten.
        XCTAssertNotNil(autoCompletedTransferBps(body(bodySeconds: 37), nowMs: 30_000))
        XCTAssertNotNil(autoCompletedTransferBps(body(bodySeconds: 37), nowMs: 37_000))
        XCTAssertNil(autoCompletedTransferBps(body(bodySeconds: 37), nowMs: 37_001))
        // A short body still earns the ordinary ten seconds and no more.
        XCTAssertNotNil(autoCompletedTransferBps(body(bodySeconds: 0.5), nowMs: 10_000))
        XCTAssertNil(autoCompletedTransferBps(body(bodySeconds: 0.5), nowMs: 10_001))
    }

    func testTheIncumbentLinkReadsTheSameBodyInTheTickAndInTheStallPath() {
        let transfer = body(bodySeconds: 37)
        XCTAssertEqual(autoIncumbentLinkBps(transfer, sessionId: "s", candidateId: "c", nowMs: 30_000),
                       Double(40_000_000) * 8 / 37)
        XCTAssertNil(autoIncumbentLinkBps(transfer, sessionId: "s", candidateId: "c", nowMs: 37_001))
        // Bound to the installed session and candidate, an actual 200 with
        // a receipt; nothing else is the incumbent's link.
        XCTAssertNil(autoIncumbentLinkBps(transfer, sessionId: "other", candidateId: "c", nowMs: 1_000))
        XCTAssertNil(autoIncumbentLinkBps(transfer, sessionId: "s", candidateId: "other", nowMs: 1_000))
        XCTAssertNil(autoIncumbentLinkBps(transfer, sessionId: nil, candidateId: "c", nowMs: 1_000))
        XCTAssertNil(autoIncumbentLinkBps(body(bodySeconds: 37, receipt: nil), sessionId: "s", candidateId: "c", nowMs: 1_000))
    }

    func testTheTickAndTheStallPathReadTheIncumbentLinkThroughTheOneValidityRule() throws {
        let source = try playerControllerSource()
        for signature in ["private func tickDisplayAwareAuto(pressureOnly: Bool = false) {",
                          "private func selectAutoStallRecoveryCandidate(attempt: Attempt, deadline: Double?) async {"] {
            let body = try functionBody(source, signature)
            XCTAssertTrue(
                body.contains("autoIncumbentLinkBps(transfer, sessionId: sessionId, candidateId: autoActiveCandidateId, nowMs: now)"),
                "\(signature) reads the incumbent's link through the helper that applies the body's validity"
            )
            XCTAssertFalse(
                body.contains("ageMs(nowMs: now) <= 10_000"),
                "\(signature) must not expire a body at ten seconds on its own"
            )
        }
    }

    func testTwoMildBodiesStayUrgentForAsLongAsTheLastOneTookToArrive() {
        let item = NSObject()
        let owner = AutoTransferPressureOwner(item: ObjectIdentifier(item), attempt: "attempt", viewerEpoch: 1,
                                              session: "s", candidate: "c", origin: "https://node")
        var evidence = AutoTransferPressureEvidence(); evidence.bind(owner)
        func mild(_ receipt: Int, completed: Int) -> PlayerController.AutoCompletedTransfer {
            body(bodySeconds: 13, completedAt: completed,
                 receipt: String(format: "00000000-0000-0000-0000-%012d", receipt))
        }
        // 40 MB in 13 s is 24.6 Mb/s against a 28.8 Mb/s title: mild twice.
        let first = evidence.observe(mild(1, completed: 1_000), owner: owner, canonicalOrigin: "https://node",
                                     downsideCost: 28_814_703, knownPeak: false, nowMs: 1_000)
        XCTAssertEqual(first?.urgent, false)
        let second = evidence.observe(mild(2, completed: 7_000), owner: owner, canonicalOrigin: "https://node",
                                      downsideCost: 28_814_703, knownPeak: false, nowMs: 7_000)
        XCTAssertEqual(second?.urgent, true)
        // Twelve seconds on, the second body is still the link's evidence;
        // the old ten-second window had already forgotten both.
        XCTAssertEqual(evidence.mildSamples(nowMs: 19_000), 2)
        XCTAssertEqual(evidence.mildSamples(nowMs: 20_000), 2)
        XCTAssertEqual(evidence.mildSamples(nowMs: 20_001), 0)
    }

    // MARK: Fix 4 — a stationary film clock is sampled as not presenting

    func testThePresenterSamplesAStoppedFilmClockOnItsOwnClock() throws {
        let source = try playerControllerSource()
        let clock = try functionBody(source, "private func startSurfaceClock() {")
        let sample = try XCTUnwrap(clock.range(of: "self.sampleSurfacePresentation("),
                                   "the presenter's clock takes the evidence sample once the observer is silent")
        let wait = try XCTUnwrap(clock.range(of: "self.noteMediaWaiting()"))
        XCTAssertLessThan(sample.lowerBound, wait.lowerBound,
                          "evidence first, then the wait, so the raise is weighed against the stopped clock")
        XCTAssertTrue(clock.contains("Self.surfaceClockSamplesPresentation("))
        // The contract's evidence stays the position delta; no transport status.
        XCTAssertFalse(clock.contains("timeControlStatus"))
        let sampler = try functionBody(source,
            "private func sampleSurfacePresentation(at observedPosition: Int, evidence: LocalVideoEvidence) {")
        XCTAssertTrue(sampler.contains("lastSurfaceSampleAtMs = PlaybackControlSession.monotonicMs()"),
                      "the sampler records when it ran, so the clock knows when the observer went quiet")
    }

    func testThePresenterClockSamplesOnlyOnceTheObserverHasBeenSilentForAnInterval() {
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: nil, nowMs: 0))
        XCTAssertFalse(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 10_499))
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 10_500))
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 22_000))
    }

    func testAWaitRaisedOverAStationaryClockIsKeptAndOneOverAStaleSampleIsNot() {
        // The model half of the defect, for the record: the observer's last
        // sample over a still-moving clock says presenting, and a wait
        // raised against it is retired in the same step. Once the stopped
        // clock has been sampled, the same raise stands.
        var model = PlaybackSurfaceModel()
        let origin = ContinuousClock.now
        _ = model.apply(.attach(1), now: origin)
        _ = model.apply(.presenting(true, attached: 1), now: origin.advanced(by: .milliseconds(500)))
        let retired = model.apply(
            PlaybackSurfaceModel.raise(source: "media_waiting", context: .attached, attached: 1,
                                       playerStopped: false, title: "Buffering…", detail: nil),
            now: origin.advanced(by: .milliseconds(1_000)))
        XCTAssertTrue(retired.contains { $0.event == .cleared && $0.source == "media_waiting" })
        XCTAssertFalse(model.faults.contains { $0.source == "media_waiting" })
        _ = model.apply(.presenting(false, attached: 1), now: origin.advanced(by: .milliseconds(1_500)))
        let kept = model.apply(
            PlaybackSurfaceModel.raise(source: "media_waiting", context: .attached, attached: 1,
                                       playerStopped: false, title: "Buffering…", detail: nil),
            now: origin.advanced(by: .milliseconds(1_500)))
        XCTAssertFalse(kept.contains { $0.event == .cleared })
        XCTAssertTrue(model.faults.contains { $0.source == "media_waiting" })
    }
}
