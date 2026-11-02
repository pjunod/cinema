import CryptoKit
import XCTest
@testable import plurx

@MainActor
final class AutoPreparedCandidateBindingTests: XCTestCase {
    private func candidate(_ height: Int, recipe: UInt8, compatible: Bool = true, invalidDigest: Bool = false,
                           route: String? = nil, grade: String = "sdr", processed: Bool = false,
                           codec: String? = nil) -> QualityCandidate {
        let digest = Array(repeating: recipe, count: 32)
        var bytes = Data("plurx:auto-quality-candidate:v1\0".utf8)
        bytes.append(contentsOf: digest)
        let id = SHA256.hash(data: bytes).prefix(16).map { String(format: "%02x", $0) }.joined()
        return QualityCandidate(id: id, recipeDigest: invalidDigest ? Array(repeating: 99, count: 32) : digest,
            route: route ?? (height == 1080 ? "remux" : "encode"),
            width: height * 16 / 9, height: height, targetHeight: height,
            averageBps: 2_000_000, peakBps: 3_000_000, grade: grade, decoderCompatible: compatible,
            completeCache: false, sustainable: false, plannedCodec: codec,
            processedHdr10: processed ? true : nil)
    }

    func testProcessedSdrIdentityResolvesAutoWhileInvalidRouteCodecAndGradeDoNot() {
        let processedSdr = candidate(720, recipe: 40, grade: "sdr", processed: true, codec: "hevc")
        XCTAssertTrue(processedSdr.hasValidIdentity)
        XCTAssertEqual(autoControlQualitySelection(desired: processedSdr, installedId: nil,
            candidates: [processedSdr]), .autoCandidate(height: 720, candidateId: processedSdr.id))

        let invalid = [
            candidate(720, recipe: 41, route: "remux", grade: "sdr", processed: true, codec: "hevc"),
            candidate(720, recipe: 42, grade: "sdr", processed: true, codec: "avc"),
            candidate(720, recipe: 43, grade: "hlg", processed: true, codec: "hevc"),
        ]
        for candidate in invalid {
            XCTAssertFalse(candidate.hasValidIdentity)
            XCTAssertEqual(autoControlQualitySelection(desired: candidate, installedId: nil,
                candidates: [candidate]), .auto)
        }
    }

    private func action(_ candidate: QualityCandidate, id: String) -> PreparedReplacementAction {
        var effective = EffectiveSelection(qualityAuto: true, height: candidate.height,
            audioTrack: nil, subtitleBurn: nil, audioOffsetMs: 0, codec: "server_selected", dynamicRange: "sdr")
        effective.candidateId = candidate.id
        let session = "00000000-0000-0000-0000-000000000003"
        return PreparedReplacementAction(ControlAction(type: PlaybackControl.prepareActionType,
            actionId: id, sessionId: session, playlistUrl: "/api/v1/hls/\(session)/index.m3u8",
            mediaOriginMs: 0, effectiveSelection: effective))!
    }

    func testAbandonedProposalReportsInstalled720InsteadOfUnnamedAuto() {
        let installed = candidate(720, recipe: 1), proposed = candidate(360, recipe: 2)
        let catalog = [installed, proposed]
        XCTAssertEqual(autoControlQualitySelection(desired: proposed, installedId: installed.id, candidates: catalog),
            .autoCandidate(height: 360, candidateId: proposed.id))
        // The proposal failed without exposure. Clearing it must not ask the server
        // to restart planning from the original source while 720p still plays.
        XCTAssertEqual(autoControlQualitySelection(desired: nil, installedId: installed.id, candidates: catalog),
            .autoCandidate(height: 720, candidateId: installed.id))
    }

    func testInvalidOrRetiredProposalRetainsValidatedInstalledCandidate() {
        let installed = candidate(720, recipe: 1)
        let retired = candidate(360, recipe: 2)
        let invalid = candidate(360, recipe: 2, invalidDigest: true)
        for proposal in [retired, invalid] {
            XCTAssertEqual(autoControlQualitySelection(desired: proposal, installedId: installed.id,
                candidates: [installed]), .autoCandidate(height: 720, candidateId: installed.id))
        }
        XCTAssertEqual(autoControlQualitySelection(desired: retired, installedId: installed.id,
            candidates: nil), .auto, "Explicit audio/subtitle recipe invalidation still requires a new catalog")
    }

    func testCatalogInvalidationAndInitialBootstrapRetainUnnamedAuto() {
        let installed = candidate(720, recipe: 1)
        XCTAssertEqual(autoControlQualitySelection(desired: nil, installedId: nil, candidates: [installed]), .auto)
        XCTAssertEqual(autoControlQualitySelection(desired: nil, installedId: installed.id, candidates: nil), .auto)
        let incompatible = candidate(720, recipe: 1, compatible: false)
        XCTAssertEqual(autoControlQualitySelection(desired: nil, installedId: installed.id, candidates: [incompatible]), .auto)
        let invalid = candidate(720, recipe: 1, invalidDigest: true)
        XCTAssertEqual(autoControlQualitySelection(desired: nil, installedId: installed.id, candidates: [invalid]), .auto)
    }

    func testPressure360RejectsStaleOriginalBeforeBuildAndAcceptsValid360() {
        let installed = candidate(720, recipe: 1), requested = candidate(360, recipe: 2)
        let original = candidate(1080, recipe: 3)
        let host = CandidateBindingHost()
        host.selection = autoControlQualitySelection(desired: requested, installedId: installed.id,
            candidates: [installed, requested, original])
        let coordinator = PreparedReplacementCoordinator(host: host)
        let stale = action(original, id: "00000000-0000-0000-0000-000000000011")
        XCTAssertEqual(coordinator.offer(stale, filmPositionMs: 320_000), .declined)
        XCTAssertTrue(host.started.isEmpty)
        XCTAssertEqual(host.discards, 0)
        XCTAssertEqual(host.failures, 0)
        XCTAssertEqual(host.fallbacks, 0)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.actionId, stale.actionId)
        XCTAssertEqual(coordinator.pendingAcknowledgement?.state, .aborted)
        let exchanges = host.exchanges
        XCTAssertEqual(coordinator.offer(stale, filmPositionMs: 320_000), .alreadySettled)
        XCTAssertEqual(host.exchanges, exchanges, "a replayed stale offer must not trigger an exchange storm")
        let valid = action(requested, id: "00000000-0000-0000-0000-000000000012")
        XCTAssertEqual(coordinator.offer(valid, filmPositionMs: 320_000), .build(valid))
        XCTAssertEqual(host.started, [valid])
        XCTAssertEqual(host.selection, .autoCandidate(height: 360, candidateId: requested.id))
    }

    func testMismatchedSupersedePreservesCurrentSuccessorAndManualSemantics() {
        let requested = candidate(360, recipe: 2), original = candidate(1080, recipe: 3)
        let host = CandidateBindingHost()
        host.selection = .autoCandidate(height: 360, candidateId: requested.id)
        let coordinator = PreparedReplacementCoordinator(host: host)
        let valid = action(requested, id: "00000000-0000-0000-0000-000000000021")
        let stale = action(original, id: "00000000-0000-0000-0000-000000000022")
        XCTAssertEqual(coordinator.offer(valid, filmPositionMs: 320_000), .build(valid))
        XCTAssertEqual(coordinator.offer(stale, filmPositionMs: 320_000), .declined)
        XCTAssertEqual(coordinator.activeAction, valid)
        XCTAssertEqual(host.started, [valid])
        XCTAssertEqual(host.discards, 0)
        XCTAssertEqual(host.failures, 0)
        XCTAssertEqual(host.fallbacks, 0)
        guard let aborted = coordinator.pendingAcknowledgement else {
            XCTFail("The rejected staging owes aborted")
            return
        }
        XCTAssertEqual(aborted.actionId, stale.actionId)
        XCTAssertEqual(aborted.state, .aborted)
        coordinator.acknowledgementDelivered(aborted)
        XCTAssertNil(coordinator.pendingAcknowledgement)
        let exchanges = host.exchanges
        let admissions = host.admissions
        XCTAssertEqual(coordinator.offer(stale, filmPositionMs: 320_000), .alreadySettled)
        XCTAssertNil(coordinator.pendingAcknowledgement, "A delivered abort must not be queued again")
        XCTAssertEqual(host.exchanges, exchanges)
        XCTAssertEqual(host.admissions, admissions, "A retired offer must not be admitted or logged again")
        XCTAssertEqual(coordinator.activeAction, valid)
        XCTAssertEqual(host.started, [valid])
        XCTAssertEqual(host.discards, 0)
        XCTAssertEqual(host.failures, 0)
        XCTAssertEqual(host.fallbacks, 0)
        XCTAssertTrue(autoPreparedCandidateMatches(selection: .manual(height: 360), effective: stale.effectiveSelection))
        XCTAssertTrue(autoPreparedCandidateMatches(selection: .auto, effective: stale.effectiveSelection))
        XCTAssertTrue(autoPreparedCandidateMatches(selection: .original, effective: stale.effectiveSelection))
    }
}

@MainActor
private final class CandidateBindingHost: PreparedSuccessorHost {
    var selection: QualitySelection = .auto
    var started: [PreparedReplacementAction] = []
    var discards = 0
    var failures = 0
    var fallbacks = 0
    var exchanges = 0
    var admissions = 0
    func acceptsPreparedSuccessor(_ action: PreparedReplacementAction) -> Bool {
        admissions += 1
        return autoPreparedCandidateMatches(selection: selection, effective: action.effectiveSelection)
    }
    func startPreparedSuccessor(_ action: PreparedReplacementAction, filmPositionMs: Int) -> Bool {
        started.append(action)
        return true
    }
    func discardPreparedSuccessor() { discards += 1 }
    func commitPreparedSuccessor(_ action: PreparedReplacementAction) async -> PreparedCommitOutcome { .refused }
    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction) { fallbacks += 1 }
    func preparedSuccessorOwesAnExchange() { exchanges += 1 }
    func notePreparedSuccessorAbandoned(_ reason: PreparedReplacementAbandonment) { failures += 1 }
    func recordPreparedFallbackInterruption(ms: Int) {}
}
