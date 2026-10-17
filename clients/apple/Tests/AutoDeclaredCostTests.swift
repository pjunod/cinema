import AVFoundation
import CryptoKit
import XCTest
@testable import plurx

/// Auto on an original or remux incumbent whose title has never been encoded
/// whole. Each test pins one decision point of the 2026-10-17 finding on the
/// shipping build 252 — a 1080p H.264 library title delivered as a 28.8 Mb/s
/// original, behind 10 / 25 Mb/s links: the catalog's declared costs were
/// discarded, the original was costed only by a body less than ten seconds
/// old, that body's verdict expired long before the next 40 MB body could
/// arrive, and the surface presenter kept reading a stationary clock as
/// presenting. The decisions are exercised on the functions the player
/// decides with, fed the evidence the player would feed them.
final class AutoDeclaredCostTests: XCTestCase {
    /// A distinct valid candidate identity per recipe byte.
    private func identity(_ byte: UInt8) -> (id: String, digest: [UInt8]) {
        let digest = Array(repeating: byte, count: 32)
        var bytes = Data("plurx:auto-quality-candidate:v1\0".utf8)
        bytes.append(contentsOf: digest)
        return (SHA256.hash(data: bytes).prefix(16).map { String(format: "%02x", $0) }.joined(), digest)
    }

    private func candidate(_ height: Int, route: String = "encode", peak: UInt64? = 3_000_000,
                           average: UInt64? = 2_000_000, grade: String = "sdr", recipe: UInt8 = 0) -> QualityCandidate {
        let who = identity(recipe)
        return QualityCandidate(id: who.id, recipeDigest: who.digest,
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

    /// The ladder the server declared for the title: a 28.8 Mb/s original
    /// and encode rungs costed by VBV cap plus audio, none encoded whole.
    private var original: QualityCandidate { candidate(1_080, route: "remux", peak: nil, average: 28_814_703, recipe: 1) }
    private var ladder: [QualityCandidate] {
        autoCostedCatalog([
            original,
            candidate(1_080, peak: 12_000_000, average: 8_000_000, recipe: 2),
            candidate(720, peak: 7_500_000, average: 5_000_000, recipe: 3),
            candidate(480, peak: 3_500_000, average: 2_300_000, recipe: 4),
            candidate(360, peak: 2_000_000, average: 1_300_000, recipe: 5),
        ], outputs: nil)
    }
    private var current: QualityCandidate { ladder[0] }

    private func pressureVerdict(transfer: PlayerController.AutoCompletedTransfer?, runway: Double?,
                                 mildSamples: Int = 0, producer: Bool = false, pressureOnly: Bool = true,
                                 nowMs: Int = 30_000) -> AutoLinkPressureVerdict? {
        autoLinkPressureVerdict(current: current, eligible: ladder, transfer: transfer, transferCurrent: true,
            sessionId: "s", candidateId: current.id, mildSamples: mildSamples, producerPressure: producer,
            runwaySeconds: runway, playbackRate: 1, rejected: [], pressureOnly: pressureOnly, nowMs: nowMs)
    }

    // MARK: Fix 1 — declared catalog costs stand until a measured output replaces them

    func testDeclaredCatalogCostsStandUntilAMeasuredOutputReplacesThem() {
        let rung = candidate(720, peak: 7_500_000, average: 5_000_000)
        // No complete full-mux measurement: the declared VBV cap plus audio
        // is the rung's cost. The old catalog overwrote it with nil.
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
        let costed = autoCostedCatalog([original], outputs: nil).first
        XCTAssertNil(costed?.peakBps)
        XCTAssertEqual(costed?.averageBps, 28_814_703)
    }

    func testARemuxUnderLinkPressureNowHasACostedLowerRungToRecoverTo() {
        // The link measured 8.69 Mb/s in the 10 Mb/s stage.
        XCTAssertEqual(autoRecoveryCandidate(ladder, current: current, rejected: [],
                                             linkCeiling: 8_690_000, currentCost: 28_814_703)?.height, 720)
        XCTAssertEqual(autoRecoveryCandidate(ladder, current: current, rejected: [],
                                             linkCeiling: 5_000_000, currentCost: 28_814_703)?.height, 480)
    }

    // MARK: Fix 2 — an original is costed by its declared source rate, and pressure does not wait for the runway

    func testAnOriginalIsCostedByItsDeclaredSourceRateWithoutAFreshBody() {
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

    func testLinkPressureOnACostedOriginalDoesNotWaitForTheRunwayToDrain() throws {
        // The 10 Mb/s stage as the player saw it: a 40 MB body that took
        // 37 s (8.65 Mb/s) against a 28.8 Mb/s original, with a minute of
        // runway still loaded. A pressure-only evaluation — the one a
        // completed body triggers at arrival — must move, not wait for the
        // runway to reach ten seconds.
        let arrival = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 37, completedAt: 30_000), runway: 60))
        XCTAssertTrue(arrival.severe)
        XCTAssertTrue(arrival.pressure)
        XCTAssertEqual(arrival.chosen?.height, 720)
        XCTAssertEqual(arrival.downsideCost, 28_814_703)
        // The same evidence on the five-second tick decides the same.
        let tick = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 37, completedAt: 30_000), runway: 60, pressureOnly: false))
        XCTAssertEqual(tick.chosen?.height, 720)
        // With under four seconds of runway the rescue minimises acquisition
        // cost instead: the cheapest costed rung, not the largest that fits.
        let starved = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 37, completedAt: 30_000), runway: 3))
        XCTAssertEqual(starved.chosen?.height, 360)
        // Healthy headroom is not pressure, and a pressure-only evaluation
        // then has no verdict at all.
        XCTAssertNil(pressureVerdict(transfer: body(bodySeconds: 5, completedAt: 30_000, bytes: 40_000_000), runway: 60))
        let healthy = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 5, completedAt: 30_000), runway: 60, pressureOnly: false))
        XCTAssertFalse(healthy.pressure)
        XCTAssertNil(healthy.chosen)
        // Two mild bodies are pressure too, whatever the runway: 40 MB in
        // 13.3 s is 24 Mb/s, the 25 Mb/s stage against the 28.8 Mb/s title.
        let mild = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 13.3, completedAt: 30_000), runway: 60, mildSamples: 2))
        XCTAssertFalse(mild.severe)
        XCTAssertTrue(mild.mild && mild.pressure)
        XCTAssertEqual(mild.chosen?.height, 720)
        XCTAssertNil(pressureVerdict(transfer: body(bodySeconds: 13.3, completedAt: 30_000), runway: 60, mildSamples: 1))
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

    func testTheIncumbentLinkReadsTheSameBodyInTheTickAndInTheStallPath() throws {
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
        // The tick, thirty seconds after a 37 s body: still the link.
        let tick = try XCTUnwrap(pressureVerdict(transfer: body(bodySeconds: 37, completedAt: 0), runway: 60, pressureOnly: false))
        XCTAssertEqual(tick.link, Double(40_000_000) * 8 / 37)
        XCTAssertEqual(tick.chosen?.height, 720)
        // The stall path, thirty seconds after the same body: a cause and a
        // rung, rather than reopening the delivery the link cannot carry.
        let stalled = try XCTUnwrap(autoStallRecoveryVerdict(current: current, candidates: ladder,
            transfer: body(bodySeconds: 37, completedAt: 0), transferCurrent: true, sessionId: "s", candidateId: current.id,
            mildSamples: 0, producerPressure: false, rejected: [], switchBudgetOpen: false, nowMs: 30_000))
        XCTAssertTrue(stalled.severe)
        XCTAssertEqual(stalled.next?.height, 720)
        // Forty seconds on, that body is spent and the stall has no cause.
        XCTAssertNil(autoStallRecoveryVerdict(current: current, candidates: ladder,
            transfer: body(bodySeconds: 37, completedAt: 0), transferCurrent: true, sessionId: "s", candidateId: current.id,
            mildSamples: 0, producerPressure: false, rejected: [], switchBudgetOpen: true, nowMs: 40_000))
        // A body that is not the incumbent's is no link, in either path.
        XCTAssertNil(autoLinkPressureVerdict(current: current, eligible: ladder, transfer: body(bodySeconds: 37, completedAt: 0),
            transferCurrent: false, sessionId: "s", candidateId: current.id, mildSamples: 0, producerPressure: false,
            runwaySeconds: 60, playbackRate: 1, rejected: [], pressureOnly: true, nowMs: 30_000))
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

    func testThePresenterClockSamplesOnlyOnceTheObserverHasBeenSilentForAnInterval() {
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: nil, nowMs: 0))
        XCTAssertFalse(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 10_499))
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 10_500))
        XCTAssertTrue(PlayerController.surfaceClockSamplesPresentation(lastSampleAtMs: 10_000, nowMs: 22_000))
    }

    @MainActor
    func testThePresenterClockRecordsAStoppedFilmClockAsNotPresenting() {
        // The observer's last sample, over a clock that still moved: the
        // model says presenting. Then the clock stops and the observer goes
        // quiet. The presenter's clock, one interval later, must take the
        // sample the observer no longer can and tell the model the picture
        // stopped — or every wait raised after it is retired in the same
        // step by that stale sample, which is what shipped.
        let controller = PlayerController()
        controller.attachSurfaceForTesting(AVPlayerItem(url: URL(fileURLWithPath: "/nonexistent/title.m3u8")), generation: 1)
        controller.player.rate = 1
        let picture = LocalVideoEvidence(bindingMatches: true, targetVisible: true, readyForDisplay: true)
        controller.localVideoEvidenceForTesting = { _, _ in picture }
        controller.sampleObserverPresentationForTesting(positionMs: 0, atMs: 1_000, evidence: picture)
        XCTAssertTrue(controller.surface.presenting, "a moving clock with a picture at rate 1 is presenting")
        // Within an interval of the observer's sample the clock adds nothing.
        controller.surfaceClockTick(nowMs: 1_400)
        XCTAssertTrue(controller.surface.presenting)
        XCTAssertEqual(controller.surfaceLastSampleAtMsForTesting, 1_000)
        // One interval of silence: the clock samples the stationary
        // position, and the model learns the picture is not presenting.
        controller.surfaceClockTick(nowMs: 1_500)
        XCTAssertFalse(controller.surface.presenting, "a position that has not moved is not presentation")
        XCTAssertNotEqual(controller.surfaceLastSampleAtMsForTesting, 1_000, "the clock's sample is the sampler's sample")
    }

    func testAWaitRaisedOverAStationaryClockIsKeptAndOneOverAStaleSampleIsNot() {
        // The model half, for the record: a wait raised against a presenting
        // sample is retired in the same step; once the stopped clock has been
        // sampled, the same raise stands.
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
