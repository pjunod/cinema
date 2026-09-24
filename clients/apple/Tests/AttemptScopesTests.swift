import AVFoundation
import XCTest
@testable import plurx

/// `Attempt` exists so a continuation can say which of the controller's nine
/// epochs its work depends on. The property that has to hold for that to be
/// worth anything is the one these cases enumerate: a scope is invalidated by
/// its own counter and by no other. If any two scopes ever answered for each
/// other, a viewer's pause would cancel an unrelated prepared replacement, or
/// a new `open()` would be survived by the previous one's alignment seek.
final class AttemptScopesTests: XCTestCase {
    /// A snapshot whose nine counters are all different, so a case that reads
    /// the wrong field cannot accidentally read the right value. Passing a
    /// scope advances that one counter and leaves the other eight alone.
    private static func attempt(bumping moved: Attempt.Scope?) -> Attempt {
        func value(_ scope: Attempt.Scope, _ base: Int) -> Int {
            scope == moved ? base + 1 : base
        }
        return Attempt(
            lifecycle: value(.lifecycle, 11),
            open: value(.open, 22),
            viewerAction: value(.viewerAction, 33),
            initialDecision: value(.initialDecision, 44),
            createRetry: value(.createRetry, 55),
            preparedAlignment: value(.preparedAlignment, 66),
            seek: value(.seek, 77),
            pgsSelection: value(.pgsSelection, 88),
            pgsItem: value(.pgsItem, 99),
            item: nil
        )
    }

    /// The nine by nine: every scope against every counter that could move.
    func testEachScopeIsInvalidatedByItsOwnCounterAndNoOther() {
        let captured = Self.attempt(bumping: nil)
        XCTAssertEqual(Attempt.Scope.allCases.count, 9)
        for moved in Attempt.Scope.allCases {
            let now = Self.attempt(bumping: moved)
            for asked in Attempt.Scope.allCases {
                XCTAssertEqual(
                    captured.stillCurrent(now, scopes: [asked]),
                    asked != moved,
                    ".\(asked) answered for .\(moved): the nine scopes are not independent"
                )
            }
        }
    }

    /// The same property stated the other way round, so a `stillCurrent` that
    /// ignored its argument and always returned true could not pass both.
    func testAskingForEveryScopeSeesEveryCounter() {
        let captured = Self.attempt(bumping: nil)
        let all = Set(Attempt.Scope.allCases)
        XCTAssertTrue(captured.stillCurrent(captured, scopes: all))
        for moved in Attempt.Scope.allCases {
            XCTAssertFalse(captured.stillCurrent(Self.attempt(bumping: moved), scopes: all))
            XCTAssertEqual(
                captured.staleScopes(Self.attempt(bumping: moved), scopes: all),
                [moved]
            )
        }
    }

    func testAnEmptyScopeSetIsCurrentBecauseItDependsOnNothing() {
        let captured = Self.attempt(bumping: nil)
        for moved in Attempt.Scope.allCases {
            XCTAssertTrue(captured.stillCurrent(Self.attempt(bumping: moved), scopes: []))
        }
    }

    // MARK: - The migrated fences, through the controller

    /// Each migrated fence's scope set, copied from the conjunction it
    /// replaced (the removed lines are in PR #464's diff). This table is the
    /// pin: swapping, widening or narrowing any fence's set in `AttemptFence`
    /// fails `testEachMigratedFenceComparesExactlyTheFieldsItsConjunctionDid`,
    /// and a fence migrated without a row here fails it too.
    private static let migratedFences: [AttemptFence: Set<Attempt.Scope>] = [
        // `self.openGeneration == recoveryGeneration`,
        // `self.viewerActionEpoch == recoveryActionEpoch`,
        // `self.seekState.generation == generation`
        .seekPresentationDeadline: [.open, .viewerAction, .seek],
        // `self.isCurrentLifecycle(lifecycle)`,
        // `self.viewerActionEpoch == actionEpoch`
        .blackFrameDecodeFailure: [.lifecycle, .viewerAction],
        // `openGeneration == generation`, `viewerActionEpoch == actionEpoch`
        .stallRecovery: [.open, .viewerAction],
        // `openGeneration == generation`, `viewerActionEpoch == actionEpoch`
        .itemFailureLadder: [.open, .viewerAction],
    ]

    func testEachMigratedFenceComparesExactlyTheFieldsItsConjunctionDid() {
        XCTAssertEqual(Set(AttemptFence.allCases), Set(Self.migratedFences.keys))
        for fence in AttemptFence.allCases {
            XCTAssertEqual(
                fence.scopes,
                Self.migratedFences[fence],
                "\(fence) no longer compares what its conjunction compared"
            )
        }
    }

    @MainActor
    private func start(_ controller: PlayerController, model: AppModel, file: Int = 1) {
        controller.start(model: model, itemId: file, fileId: file, startMs: 0,
                         durationMs: 600_000, title: "Title \(file)")
    }

    /// A viewer Pause, through the controller's own `togglePlayPause`, moves
    /// exactly one of the nine epochs — `viewerActionEpoch` — and every
    /// migrated fence reads it, so every one of them refuses the continuation
    /// it was guarding, and says so with `scope=viewerAction`. A fence whose
    /// set lost `.viewerAction` would let a late recovery reopen a stream the
    /// viewer had just paused.
    @MainActor
    func testEveryMigratedFenceRefusesAContinuationAcrossAViewerPause() {
        for fence in AttemptFence.allCases {
            let controller = PlayerController()
            start(controller, model: AppModel())
            let captured = controller.snapshotAttempt()
            XCTAssertTrue(controller.attemptStillCurrent(captured, fence: fence))
            XCTAssertNil(controller.lastAttemptStaleDetail, "a current fence raised attempt_stale")

            controller.togglePlayPause()

            XCTAssertEqual(
                captured.staleScopes(controller.snapshotAttempt(), scopes: Set(Attempt.Scope.allCases)),
                [.viewerAction],
                "a Pause is expected to move the viewer-action epoch and nothing else"
            )
            XCTAssertFalse(
                controller.attemptStillCurrent(captured, fence: fence),
                "\(fence) survived a viewer Pause"
            )
            XCTAssertEqual(
                controller.lastAttemptStaleDetail,
                "fence=\(fence.rawValue) scope=viewerAction item=same"
            )
            controller.stop()
        }
    }

    /// A new title, through `stop()` and `start(...)`, moves the lifecycle and
    /// open generations. The black-frame fence answers to the lifecycle and
    /// the other three to the attach attempt, so each must refuse, and each
    /// must name the scope it refused on.
    @MainActor
    func testEveryMigratedFenceRefusesAContinuationFromThePreviousTitle() {
        let attachScope: [AttemptFence: Attempt.Scope] = [
            .seekPresentationDeadline: .open,
            .blackFrameDecodeFailure: .lifecycle,
            .stallRecovery: .open,
            .itemFailureLadder: .open,
        ]
        for fence in AttemptFence.allCases {
            let controller = PlayerController()
            let model = AppModel()
            start(controller, model: model)
            let captured = controller.snapshotAttempt()

            controller.stop()
            start(controller, model: model, file: 2)

            XCTAssertFalse(
                controller.attemptStillCurrent(captured, fence: fence),
                "\(fence) survived into the next title"
            )
            let stale = captured.staleScopes(controller.snapshotAttempt(), scopes: fence.scopes)
            guard let expected = attachScope[fence] else {
                XCTFail("\(fence) has no attach scope in this test's table")
                continue
            }
            XCTAssertTrue(
                stale.contains(expected),
                "\(fence) refused on \(stale), not on the attach scope it owns"
            )
            XCTAssertEqual(
                controller.lastAttemptStaleDetail?.hasPrefix("fence=\(fence.rawValue) scope="),
                true
            )
            controller.stop()
        }
    }

    /// The plan's `lateSeekContinuationIsRefusedAfterViewerPause`, on the
    /// fence it names: a seek is issued, the deadline monitor captures its
    /// attempt, and the viewer presses Pause before the eight-second deadline
    /// fires. The reopen that monitor was about to ask for is no longer
    /// anyone's intent.
    @MainActor
    func testLateSeekContinuationIsRefusedAfterViewerPause() {
        let controller = PlayerController()
        start(controller, model: AppModel())
        controller.seek(toMs: 90_000)
        let recovery = controller.snapshotAttempt()

        controller.togglePlayPause()

        XCTAssertFalse(controller.attemptStillCurrent(recovery, fence: .seekPresentationDeadline))
        XCTAssertEqual(
            controller.lastAttemptStaleDetail,
            "fence=seek_presentation_deadline scope=viewerAction item=same"
        )
        controller.stop()
    }

    /// The other half of the same rule, and the reason the nine are not one:
    /// a prepared-commit alignment does not read the viewer-action epoch, so a
    /// real Pause must not cancel it. `awaitPreparedAlignment` is not migrated
    /// yet (it is still an allow-list row), so this pins the controller side of
    /// the property — a Pause leaves the lifecycle and prepared-alignment
    /// epochs where they were — for the fence that will name those scopes.
    @MainActor
    func testPreparedAlignmentSurvivesViewerPause() {
        let controller = PlayerController()
        start(controller, model: AppModel())
        let captured = controller.snapshotAttempt()

        controller.togglePlayPause()

        let now = controller.snapshotAttempt()
        XCTAssertTrue(captured.stillCurrent(now, scopes: [.lifecycle, .preparedAlignment]))
        XCTAssertEqual(captured.staleScopes(now, scopes: [.lifecycle, .preparedAlignment]), [])
        controller.stop()
    }

    /// The `attempt_stale` detail is bounded by what it is built from: the
    /// fence's raw value, the moved scopes in declaration order, and a
    /// two-valued item field.
    @MainActor
    func testAttemptStaleDetailNamesTheFenceTheMovedScopesAndTheItem() {
        let item = AVPlayerItem(url: URL(string: "http://example.invalid/a.m3u8")!)
        let other = AVPlayerItem(url: URL(string: "http://example.invalid/b.m3u8")!)
        func snapshot(open: Int, viewerAction: Int, item: AVPlayerItem) -> Attempt {
            Attempt(
                lifecycle: 1, open: open, viewerAction: viewerAction, initialDecision: 1,
                createRetry: 1, preparedAlignment: 1, seek: 1,
                pgsSelection: 1, pgsItem: 1, item: ObjectIdentifier(item)
            )
        }
        XCTAssertEqual(
            PlayerController.attemptStaleDetail(
                fence: .itemFailureLadder,
                captured: snapshot(open: 1, viewerAction: 1, item: item),
                now: snapshot(open: 2, viewerAction: 2, item: other)
            ),
            "fence=item_failure_ladder scope=open,viewerAction item=replaced"
        )
    }

    func testStaleScopesReportsOnlyTheScopesItWasAskedAbout() {
        let captured = Self.attempt(bumping: nil)
        let afterOpen = Self.attempt(bumping: .open)
        XCTAssertEqual(captured.staleScopes(afterOpen, scopes: [.open, .seek]), [.open])
        XCTAssertEqual(captured.staleScopes(afterOpen, scopes: [.seek]), [])
    }

    /// Item identity is carried, and is not a scope: `stillCurrent` must not
    /// start answering for it.
    func testTheAttachedItemIsNotAScope() {
        let item = AVPlayerItem(url: URL(string: "http://example.invalid/a.m3u8")!)
        let other = AVPlayerItem(url: URL(string: "http://example.invalid/b.m3u8")!)
        let captured = Attempt(
            lifecycle: 1, open: 1, viewerAction: 1, initialDecision: 1,
            createRetry: 1, preparedAlignment: 1, seek: 1,
            pgsSelection: 1, pgsItem: 1, item: ObjectIdentifier(item)
        )
        let replaced = Attempt(
            lifecycle: 1, open: 1, viewerAction: 1, initialDecision: 1,
            createRetry: 1, preparedAlignment: 1, seek: 1,
            pgsSelection: 1, pgsItem: 1, item: ObjectIdentifier(other)
        )
        XCTAssertTrue(captured.stillCurrent(replaced, scopes: Set(Attempt.Scope.allCases)))
        XCTAssertFalse(captured.hasSameItem(as: replaced))
        XCTAssertTrue(captured.hasSameItem(as: captured))
    }
}
