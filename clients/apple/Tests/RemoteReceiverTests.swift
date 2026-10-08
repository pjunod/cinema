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
    func testPairingCloseAndTargetSwitchRetireLateApprovedReply() {
        let target = CinemaRemoteTarget(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())
        let receiver = UUID(), other = UUID()
        var lifetime = RemotePairingLifetime()
        let ticket = lifetime.begin(receiverID: receiver, target: target)
        XCTAssertTrue(lifetime.accepts(ticket, receiverID: receiver, target: target))
        XCTAssertFalse(lifetime.accepts(ticket, receiverID: other, target: target))
        XCTAssertFalse(lifetime.accepts(ticket, receiverID: receiver, target: .init(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())))
        lifetime.retire() // close before result callback
        XCTAssertFalse(lifetime.accepts(ticket, receiverID: receiver, target: target))
        let newer = lifetime.begin(receiverID: other, target: target)
        XCTAssertFalse(lifetime.accepts(ticket, receiverID: receiver, target: target))
        XCTAssertTrue(lifetime.accepts(newer, receiverID: other, target: target))
    }
    @MainActor
    func testNullStateTimeoutPreservesStateUntilControllerRetirement() {
        let model = RemoteClientModel()
        let nonce = UUID()
        let state = CinemaRemoteState(stateRevision: 1, contextRevision: 2, focusRevision: 3, route: "search",
                                      capabilities: [.textReplace], focusedLabel: "Search", credits: [.init(nonce: UUID(), kind: .interaction)], textNonce: nonce, playback: nil)
        model.acceptState(state)
        model.acceptState(nil) // B04 unchanged timeout
        XCTAssertEqual(model.controllerState?.contextRevision, 2)
        XCTAssertEqual(model.controllerState?.textNonce, nonce)
        XCTAssertEqual(model.controllerState?.credits.count, 1)
        model.closeController()
        XCTAssertNil(model.controllerState)
        model.acceptState(nil)
        XCTAssertNil(model.controllerState)
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
    func testNegativeZeroRejectedByCommandAndProductionNestedPollDecoder() throws {
        let values = try XCTUnwrap(try fixture()["valid"] as? [[String: Any]])
        var command = try XCTUnwrap(values.first?["command"] as? [String: Any])
        command["action"] = ["type": "seek_absolute", "position_ms": 0]
        let data = try JSONSerialization.data(withJSONObject: command)
        let raw = try XCTUnwrap(String(data: data, encoding: .utf8)).replacingOccurrences(of: "\"position_ms\":0", with: "\"position_ms\":-0")
        XCTAssertThrowsError(try CinemaRemoteCommand.decode(Data(raw.utf8)))
        let target = try JSONSerialization.data(withJSONObject: command["target"]!)
        let poll = "{\"version\":\"cinema.remote.v1\",\"target\":" + String(decoding: target, as: UTF8.self) + ",\"response_revision\":1,\"delivery_id\":1,\"control\":null,\"commands\":[" + raw + "],\"pairings\":[]}"
        XCTAssertThrowsError(try CinemaRemoteAPI.decodeResponse(Data(poll.utf8)) as CinemaRemotePollResponse)
    }
    func testTVPairSurfaceCloseReplacementAndExpiredCodeRetireOldReplies() {
        let receiver = UUID()
        let target = CinemaRemoteTarget(ownerNodeID: "node", sessionID: UUID(), receiverEpoch: UUID())
        var lifetime = RemotePairingLifetime()
        let first = lifetime.begin(receiverID: receiver, target: target)
        lifetime.retire(); XCTAssertFalse(lifetime.accepts(first, receiverID: receiver, target: target))
        let approval = lifetime.begin(receiverID: receiver, target: target)
        let newer = lifetime.begin(receiverID: receiver, target: target)
        XCTAssertFalse(lifetime.accepts(approval, receiverID: receiver, target: target)); XCTAssertTrue(lifetime.accepts(newer, receiverID: receiver, target: target))
        let advertised = RemotePairingDeadline(start: 1_000, budget: 120_000)
        XCTAssertTrue(advertised.admits(120_999)); XCTAssertFalse(advertised.admits(121_000)); XCTAssertEqual(advertised.remaining(121_000), 0)
    }
    func testPhonePairingMonotonicDeadlineRejectsLateApprovalBeforeSave() {
        let deadline = RemotePairingDeadline(start: 1_000)
        XCTAssertTrue(deadline.admits(120_999)); XCTAssertFalse(deadline.admits(121_000))
        XCTAssertFalse(deadline.admits(999)); XCTAssertEqual(deadline.remaining(120_999), 1)
        XCTAssertFalse(RemotePairingDeadline(start: .max).admits(.max))
    }
    func testExactAckBefore202CannotReturnToPendingOrCrossCommandOwner() {
        var result = RemoteCommandResult(); let epoch = UUID()
        result.begin(epoch: epoch, sequence: 1)
        XCTAssertTrue(result.observe(epoch: epoch, sequence: 1, outcome: .applied)); XCTAssertEqual(result.known(epoch: epoch, sequence: 1), .applied)
        result.begin(epoch: epoch, sequence: 2)
        XCTAssertFalse(result.observe(epoch: epoch, sequence: 1, outcome: .restrictedSurface)); XCTAssertNil(result.known(epoch: epoch, sequence: 2))
        XCTAssertTrue(result.observe(epoch: epoch, sequence: 2, outcome: .unsupported)); XCTAssertEqual(result.known(epoch: epoch, sequence: 2), .unsupported)
        result.retire(); XCTAssertNil(result.known(epoch: epoch, sequence: 2))
    }
    func testNormalizedPresenceBudgetPreservesExactIDsAndTransport() throws {
        let summary = CinemaRemotePlaybackSummary(media: .item(1), title: "Playing", playing: true, positionMs: 0, durationMs: 1_000,
            tracks: (0..<64).map { index in CinemaRemoteTrackOption(kind: .audio, optionID: "id:\(index):" + String(repeating: "\u{1}", count: 120), label: String(repeating: "\u{2}", count: 256)) })
        let state = CinemaRemoteState(stateRevision: 1, contextRevision: 1, focusRevision: 1, route: "playback", capabilities: [.setPlaying, .stop, .openTracks, .chooseTrack], focusedLabel: nil, credits: [], textNonce: nil, playback: summary)
        let fitted = RemoteStateBudget.fit(state)
        XCTAssertLessThanOrEqual(try JSONEncoder().encode(fitted).count + 256, RemoteStateBudget.maximumBytes)
        XCTAssertEqual(fitted.route, "playback"); XCTAssertEqual(fitted.capabilities, [.setPlaying, .stop]); XCTAssertNil(fitted.playback)
    }

    @MainActor
    func testProductionPresenceRevisionAdvancesAcrossPairingOpenPendingExpiredAndClose() {
        let navigation = RemoteNavigationCoordinator()
        navigation.presentationBlocked = { false }
        let model = RemoteClientModel(navigation: navigation)
        // Open, pending approval, and expired-code retry all publish restricted;
        // close resumes the existing safe route with a newer publication.
        let states = [false, true, true, true, false].map { model.makeState(pairingRestricted: $0) }
        XCTAssertEqual(states.map(\.route), ["home", "restricted", "restricted", "restricted", "home"])
        for (previous, next) in zip(states, states.dropFirst()) {
            XCTAssertGreaterThan(next.stateRevision, previous.stateRevision)
        }
        for state in states.dropFirst().dropLast() {
            XCTAssertTrue(state.capabilities.isEmpty); XCTAssertTrue(state.credits.isEmpty)
            XCTAssertNil(state.playback); XCTAssertNil(state.focusedLabel)
        }
    }

    @MainActor
    func testDeferredReservationOriginalDeadlinePhysicalRetirementAndNoReplay() throws {
        let rows = try XCTUnwrap(try fixture()["valid"] as? [[String: Any]])
        let base = try decode(try XCTUnwrap(rows[1]["command"]))
        let receiver = RemoteReceiverGuard(), owner = UUID()
        try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
            contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil))
        let credit = try receiver.mint(.interaction, now: 100)
        var wire = try XCTUnwrap(rows[1]["command"] as? [String: Any]); wire["credit"] = credit.nonce.uuidString
        let command = try decode(wire)
        guard case .admitted(let permit) = receiver.reserve(command, owner: owner, now: 101, semantic: nil) else { return XCTFail("Reservation refused") }
        XCTAssertTrue(receiver.isPending(command, now: 102))
        XCTAssertTrue(receiver.permitsDispatch(permit, owner: owner, now: 1099))
        XCTAssertFalse(receiver.permitsDispatch(permit, owner: owner, now: 1100))
        XCTAssertFalse(receiver.permitsDispatch(permit, owner: UUID(), now: 1100))
        XCTAssertEqual(receiver.complete(permit, owner: owner, now: 1101, outcome: .unavailable)?.outcome, .unavailable)
        XCTAssertFalse(receiver.isPending(command, now: 1102))
        XCTAssertNil(receiver.complete(permit, owner: owner, now: 1102, outcome: .applied))
        XCTAssertEqual(receiver.apply(command, now: 1103, semantic: nil) { XCTFail("Replayed"); return .applied }, .duplicateOrOld)
        let fresh = try receiver.mint(.interaction, now: 1200)
        wire["credit"] = fresh.nonce.uuidString; wire["sequence"] = 2
        let next = try decode(wire)
        guard case .admitted(let pending) = receiver.reserve(next, owner: owner, now: 1201, semantic: nil) else { return XCTFail("Reservation refused") }
        receiver.invalidate()
        XCTAssertFalse(receiver.permitsDispatch(pending, owner: owner, now: 1202))
        XCTAssertNil(receiver.complete(pending, owner: owner, now: 1203, outcome: .applied))
        XCTAssertEqual(receiver.apply(next, now: 1204, semantic: nil) { XCTFail("Replayed"); return .applied }, .duplicateOrOld)
    }

    @MainActor
    func testLateSharedOfferCleanupRunsAfterPermitExpiryWithoutRendererMutation() async {
        var cleanup = 0, renderer = 0
        let result = await SharedRemoteControlCompletion.finish(cleanup: { cleanup += 1 }, permit: { false }) {
            renderer += 1; return .applied
        }
        XCTAssertEqual(result, .unavailable); XCTAssertEqual(cleanup, 1); XCTAssertEqual(renderer, 0)
    }
    @MainActor
    func testSharedPhysicalReplacementAndCancelledNetworkLeaveOwnedCleanupAlive() async {
        var currentOwner = UUID(), cleanup = 0, renderer = 0
        let capturedOwner = currentOwner
        currentOwner = UUID()
        let task = Task { @MainActor in
            await SharedRemoteControlCompletion.finish(cleanup: { cleanup += 1 }, permit: { currentOwner == capturedOwner }) {
                renderer += 1; return .applied
            }
        }
        task.cancel()
        let result = await task.value
        XCTAssertEqual(result, .unavailable); XCTAssertEqual(cleanup, 1); XCTAssertEqual(renderer, 0)
    }

    @MainActor
    func testNewerStopReplacesOnlyAdmittedNetworkReservationAndPreservesHighWater() throws {
        let rows = try XCTUnwrap(try fixture()["valid"] as? [[String: Any]])
        let base = try decode(try XCTUnwrap(rows[1]["command"]))
        let receiver = RemoteReceiverGuard(), owner = UUID()
        try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
            contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil))
        let credit = try receiver.mint(.interaction, now: 100)
        var wire = try XCTUnwrap(rows[1]["command"] as? [String: Any]); wire["credit"] = credit.nonce.uuidString
        let initial = try decode(wire)
        guard case .admitted(let old) = receiver.reserve(initial, owner: owner, now: 101, semantic: nil) else { return XCTFail("Reservation refused") }
        wire["sequence"] = 2; wire["action"] = ["type": "stop"]
        // Wrong-kind Stop cannot retire the existing operation.
        let invalidStop = try decode(wire)
        guard case .rejected(.invalid) = receiver.reserve(invalidStop, owner: owner, now: 102, semantic: nil, replacingPending: true) else { return XCTFail("Invalid Stop admitted") }
        XCTAssertTrue(receiver.permitsDispatch(old, owner: owner, now: 103))
        wire["credit"] = try receiver.mint(.playback, now: 104).nonce.uuidString
        let stop = try decode(wire)
        guard case .admitted(let current) = receiver.reserve(stop, owner: owner, now: 105, semantic: nil, replacingPending: true) else { return XCTFail("Stop refused") }
        XCTAssertFalse(receiver.permitsDispatch(old, owner: owner, now: 106))
        XCTAssertNil(receiver.complete(old, owner: owner, now: 107, outcome: .applied))
        XCTAssertTrue(receiver.permitsDispatch(current, owner: owner, now: 108))
        XCTAssertEqual(receiver.complete(current, owner: owner, now: 109, outcome: .applied)?.sequence, 2)
        XCTAssertEqual(receiver.apply(initial, now: 110, semantic: nil) { XCTFail("Replayed"); return .applied }, .duplicateOrOld)
    }

    @MainActor
    func testOwnedBackHomeCanReplacePendingNetworkPermitWithoutReplayingIt() throws {
        let rows = try XCTUnwrap(try fixture()["valid"] as? [[String: Any]])
        let base = try decode(try XCTUnwrap(rows[1]["command"]))
        for exit in ["back", "home"] {
            let receiver = RemoteReceiverGuard(), owner = UUID()
            try receiver.setContext(.init(grantID: base.grantID, target: base.target, controlEpoch: base.controlEpoch,
                contextRevision: base.contextRevision, focusRevision: base.focusRevision, textNonce: nil))
            var wire = try XCTUnwrap(rows[1]["command"] as? [String: Any])
            wire["credit"] = try receiver.mint(.interaction, now: 100).nonce.uuidString
            let first = try decode(wire)
            guard case .admitted(let old) = receiver.reserve(first, owner: owner, now: 101, semantic: nil) else { return XCTFail("Initial reservation refused") }
            wire["sequence"] = 2; wire["action"] = ["type": exit]
            wire["credit"] = try receiver.mint(.interaction, now: 102).nonce.uuidString
            guard case .admitted(let current) = receiver.reserve(try decode(wire), owner: owner, now: 103, semantic: nil, replacingPending: true) else { return XCTFail("Owned exit refused") }
            XCTAssertFalse(receiver.permitsDispatch(old, owner: owner, now: 104))
            XCTAssertNil(receiver.complete(old, owner: owner, now: 104, outcome: .applied))
            XCTAssertEqual(receiver.complete(current, owner: owner, now: 105, outcome: .applied)?.sequence, 2)
            XCTAssertEqual(receiver.apply(first, now: 106, semantic: nil) { XCTFail("Replayed"); return .applied }, .duplicateOrOld)
        }
    }

}
