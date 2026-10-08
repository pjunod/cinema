import Foundation
import XCTest
@testable import plurx

final class RemoteReceiverTests: XCTestCase {
    private func fixture() throws -> [String: Any] {
        let url = try XCTUnwrap(Bundle(for: Self.self).url(forResource: "remote-control-v1", withExtension: "json"))
        return try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }
    private func decode(_ value: Any) throws -> CinemaRemoteCommand {
        try CinemaRemoteCommand.decode(JSONSerialization.data(withJSONObject: value))
    }
    func testCanonicalWireFixturesAndStrictNestedPollDecode() throws {
        let cases = try fixture()
        for row in try XCTUnwrap(cases["valid"] as? [[String: Any]]) {
            let command = try decode(try XCTUnwrap(row["command"]))
            XCTAssertEqual(command.action.creditKind.rawValue, row["credit_kind"] as? String)
            XCTAssertEqual(try CinemaRemoteCommand.decode(command.encode()), command)
        }
        for row in try XCTUnwrap(cases["invalid"] as? [[String: Any]]) {
            XCTAssertThrowsError(try decode(try XCTUnwrap(row["command"])), String(describing: row["name"]))
        }
        let valid = try XCTUnwrap(cases["valid"] as? [[String: Any]])
        let command = try XCTUnwrap(valid.first?["command"] as? [String: Any])
        var poll: [String: Any] = ["version": CinemaRemoteCommand.versionName, "target": command["target"]!, "response_revision": 1,
                                   "delivery_id": 1, "control": NSNull(), "commands": [command], "pairings": []]
        let response: CinemaRemotePollResponse = try CinemaRemoteAPI.decodeResponse(JSONSerialization.data(withJSONObject: poll))
        XCTAssertEqual(response.commands.count, 1)
        var malicious = command
        malicious["action"] = ["type": "select", "playing": true]
        poll["commands"] = [malicious]
        XCTAssertThrowsError(try CinemaRemoteAPI.decodeResponse(JSONSerialization.data(withJSONObject: poll)) as CinemaRemotePollResponse)
    }
    func testDuplicateJSONKeysAndFloatingIntegerLexemesAreRejected() throws {
        let cases = try fixture()
        let valid = try XCTUnwrap(cases["valid"] as? [[String: Any]])
        let command = try decode(try XCTUnwrap(valid.first?["command"]))
        let text = try XCTUnwrap(String(data: command.encode(), encoding: .utf8))
        XCTAssertThrowsError(try CinemaRemoteCommand.decode(Data(text.replacingOccurrences(of: "\"sequence\":1", with: "\"sequence\":1.0").utf8)))
        XCTAssertThrowsError(try CinemaRemoteCommand.decode(Data(text.replacingOccurrences(of: "\"sequence\":1", with: "\"sequence\":1,\"sequence\":1").utf8)))
        XCTAssertThrowsError(try CinemaRemoteCommand.decode(Data(repeating: 32, count: 16_385)))
    }
    @MainActor
    func testCanonicalReceiverScenariosUseLocalClockAndGrantEpoch() throws {
        let cases = try fixture()
        let rows = try XCTUnwrap(cases["valid"] as? [[String: Any]])
        let baseline = try decode(try XCTUnwrap(rows[1]["command"]))
        for scenario in try XCTUnwrap(cases["scenarios"] as? [[String: Any]]) {
            let receiver = RemoteReceiverGuard()
            var context = RemoteReceiverGuard.Context(grantID: baseline.grantID, target: baseline.target, controlEpoch: baseline.controlEpoch,
                                                      contextRevision: baseline.contextRevision, focusRevision: baseline.focusRevision, textNonce: nil)
            try receiver.setContext(context)
            let issued = try XCTUnwrap(scenario["issued_ms"] as? UInt64)
            let now = try XCTUnwrap(scenario["now_ms"] as? UInt64)
            let kind = try XCTUnwrap(CinemaRemoteCreditKind(rawValue: try XCTUnwrap(scenario["credit_kind"] as? String)))
            let credit = try receiver.mint(kind, now: issued)
            var wire = try XCTUnwrap(scenario["command"] as? [String: Any])
            wire["credit"] = credit.nonce.uuidString
            let command = try decode(wire)
            if scenario["preapply"] as? Bool == true {
                XCTAssertEqual(receiver.apply(command, now: issued, semantic: nil) { .applied }, .applied)
            }
            if let epoch = (scenario["new_control_epoch"] as? String).flatMap(UUID.init(uuidString:)) {
                context = .init(grantID: baseline.grantID, target: baseline.target, controlEpoch: epoch,
                                contextRevision: baseline.contextRevision, focusRevision: baseline.focusRevision, textNonce: nil)
                try receiver.setContext(context)
            }
            let outcome = receiver.apply(command, now: now, semantic: nil) { .applied }
            XCTAssertEqual(outcome.rawValue, scenario["expected"] as? String, String(describing: scenario["name"]))
        }
    }
    @MainActor
    func testSequenceConsumedBeforeSynchronousEffectAndRejectedEffectIsNotReplayed() throws {
        let cases = try fixture()
        let rows = try XCTUnwrap(cases["valid"] as? [[String: Any]])
        let base = try decode(try XCTUnwrap(rows[1]["command"]))
        let receiver = RemoteReceiverGuard()
        try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
                                      contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil))
        let credit = try receiver.mint(.interaction, now: 100)
        var wire = try XCTUnwrap(rows[1]["command"] as? [String: Any]); wire["credit"] = credit.nonce.uuidString
        let command = try decode(wire)
        var activations = 0
        let outcome = receiver.apply(command, now: 101, semantic: nil) {
            XCTAssertNil(receiver.result(controlEpoch: base.controlEpoch, sequence: 1, now: 101))
            XCTAssertEqual(receiver.apply(command, now: 101, semantic: nil) { activations += 1; return .applied }, .duplicateOrOld)
            activations += 1
            return .unsupported
        }
        XCTAssertEqual(outcome, .unsupported)
        XCTAssertEqual(activations, 1)
        XCTAssertEqual(receiver.result(controlEpoch: base.controlEpoch, sequence: 1, now: 101)?.outcome, .unsupported)
        receiver.deactivate()
        XCTAssertThrowsError(try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
                                                          contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil)))
    }
    @MainActor
    func testCreditsAreBoundedAndClockRollbackRetiresThem() throws {
        let cases = try fixture()
        let rows = try XCTUnwrap(cases["valid"] as? [[String: Any]])
        let base = try decode(try XCTUnwrap(rows[0]["command"]))
        let receiver = RemoteReceiverGuard()
        try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
                                      contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil))
        for _ in 0..<40 { _ = try receiver.mint(.interaction, now: 100) }
        XCTAssertEqual(receiver.currentCredits.count, 16)
        XCTAssertThrowsError(try receiver.mint(.interaction, now: 99))
        XCTAssertTrue(receiver.currentCredits.isEmpty)
    }
    func testPhysicalScrubSurvivesUnrelatedInputAndOnlyNetworkPreviewIsCancelled() {
        var owner = RemotePreviewOwnership()
        XCTAssertEqual(owner.cancel(current: 30_000), 30_000)
        owner.claim(before: 30_000, after: 30_000)
        XCTAssertEqual(owner.cancel(current: 30_000), 30_000)
        owner.claim(before: nil, after: 40_000)
        XCTAssertNil(owner.cancel(current: 40_000))
        owner.claim(before: nil, after: 40_000)
        XCTAssertEqual(owner.cancel(current: 50_000), 50_000)
    }
    @MainActor
    func testGuideOrderRefreshesWhenSameChannelsHaveNewProgrammeTimes() {
        let original = RemoteGuideOrder.keys([(channelID: "news", programmeStarts: [100, 200])])
        let later = RemoteGuideOrder.keys([(channelID: "news", programmeStarts: [300, 400])])
        XCTAssertNotEqual(original, later)
        XCTAssertTrue(original.contains("live:programme:news:100"))
        XCTAssertFalse(later.contains("live:programme:news:100"))
        XCTAssertTrue(later.contains("live:programme:news:300"))
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        navigation.selectedTab = .liveTv
        navigation.synchronizeRoute()
        navigation.setOrder(scope: "live-tv", keys: original, columns: 1)
        navigation.setOrder(scope: "live-tv", keys: later, columns: 1)
        for _ in 0..<6 { XCTAssertEqual(navigation.dispatch(.navigate(.down), context: navigation.context), .applied) }
        XCTAssertEqual(navigation.requestedFocus, "live:programme:news:300")
        XCTAssertEqual(navigation.dispatch(.select, context: navigation.context), .unsupported)
    }

    func testReopenAndConnectionRecoveryRequireExplicitControlAcquisition() {
        var eligibility = RemoteControlEligibility()
        let original = UUID(), reopened = UUID()
        XCTAssertFalse(eligibility.permits(original))
        eligibility.acquired(original)
        XCTAssertTrue(eligibility.permits(original))
        eligibility.retire()
        XCTAssertFalse(eligibility.permits(original))
        XCTAssertFalse(eligibility.permits(reopened))
        eligibility.acquired(reopened)
        XCTAssertTrue(eligibility.permits(reopened))
        eligibility.retire() // state-poll/renewal failure
        XCTAssertFalse(eligibility.permits(reopened)) // a recovered state reply cannot reacquire
    }

    func testUnknownOwnEpochRecoveryCarriesObservedEpochAndCannotTargetOtherGrant() {
        let target = CinemaRemoteTarget(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())
        let grant = UUID(), oldEpoch = UUID(), newerEpoch = UUID()
        var allocator = RemoteControlSequence()
        let own = CinemaRemoteControl(controlEpoch: oldEpoch, activeGrantID: grant, controllerName: "This phone")
        XCTAssertEqual(allocator.recoveryEpoch(target: target, grant: grant, control: own), oldEpoch)
        let other = CinemaRemoteControl(controlEpoch: newerEpoch, activeGrantID: UUID(), controllerName: "Other phone")
        XCTAssertNil(allocator.recoveryEpoch(target: target, grant: grant, control: other))
        XCTAssertNil(allocator.recoveryEpoch(target: target, grant: grant, control: nil))
        allocator.acquired(target: target, grant: grant, epoch: oldEpoch)
        XCTAssertNil(allocator.recoveryEpoch(target: target, grant: grant, control: own))
    }

    func testInactiveSceneSuspendsEffectsWithoutRotatingForegroundIdentity() {
        var scene = RemoteSceneEligibility()
        scene.transition(active: true, background: false)
        let initial = scene.foregroundID
        XCTAssertTrue(scene.eligible)
        scene.transition(active: false, background: false)
        XCTAssertFalse(scene.eligible)
        scene.transition(active: true, background: false)
        XCTAssertEqual(scene.foregroundID, initial)
        scene.transition(active: false, background: true)
        XCTAssertFalse(scene.eligible)
        scene.transition(active: false, background: false)
        scene.transition(active: true, background: false)
        XCTAssertNotEqual(scene.foregroundID, initial)
    }

    func testControlSequenceSurvivesCloseAndUnknownEpochRequiresAcquisition() {
        let target = CinemaRemoteTarget(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())
        let grant = UUID(), epoch = UUID()
        var allocator = RemoteControlSequence()
        XCTAssertFalse(allocator.knows(target: target, grant: grant, epoch: epoch))
        XCTAssertNil(allocator.next(target: target, grant: grant, epoch: epoch))
        allocator.acquired(target: target, grant: grant, epoch: epoch)
        XCTAssertEqual(allocator.next(target: target, grant: grant, epoch: epoch), 1)
        allocator.acquired(target: target, grant: grant, epoch: epoch)
        XCTAssertEqual(allocator.next(target: target, grant: grant, epoch: epoch), 2)
        XCTAssertNil(allocator.next(target: target, grant: grant, epoch: UUID()))
        let restarted = RemoteControlSequence()
        XCTAssertFalse(restarted.knows(target: target, grant: grant, epoch: epoch))
    }

    func testSafeLabelsAndQRNeverChangeServerOrTarget() throws {
        XCTAssertEqual(RemoteTextBounds.label("\n\u{0} \t"), "Untitled")
        XCTAssertEqual(RemoteTextBounds.label("  Film\n title "), "Film title")
        XCTAssertLessThanOrEqual(RemoteTextBounds.label(String(repeating: "é", count: 300)).utf8.count, 256)
        let target = CinemaRemoteTarget(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())
        let challenge = CinemaRemoteChallenge(target: target, challengeID: UUID(), code: "12345678", expiresInMs: 120_000)
        let payload = try XCTUnwrap(RemotePairingQR.payload(instance: "server-a", challenge: challenge))
        XCTAssertNotNil(RemotePairingQR.parse(payload, instance: "server-a", target: target))
        XCTAssertNil(RemotePairingQR.parse(payload, instance: "server-b", target: target))
        XCTAssertNil(RemotePairingQR.parse(payload, instance: "server-a", target: .init(ownerNodeID: "other", sessionID: target.sessionID, receiverEpoch: target.receiverEpoch)))
        XCTAssertTrue(payload.contains("#code="))
    }
}
