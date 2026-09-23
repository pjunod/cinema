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

    /// The seek-presentation deadline reopen: the viewer pressed Pause while
    /// the eight-second deadline was running, so the reopen this monitor was
    /// about to ask for is no longer anyone's intent.
    func testLateSeekContinuationIsRefusedAfterViewerPause() {
        let captured = Self.attempt(bumping: nil)
        let afterPause = Self.attempt(bumping: .viewerAction)
        XCTAssertFalse(
            captured.stillCurrent(afterPause, scopes: [.open, .viewerAction, .seek])
        )
    }

    /// The other half of the same rule, and the reason the nine are not one:
    /// a prepared-commit alignment does not read the viewer-action epoch, so a
    /// pause must not cancel it here. The commit path decides for itself
    /// whether a paused viewer's change can be committed.
    func testPreparedAlignmentSurvivesViewerPause() {
        let captured = Self.attempt(bumping: nil)
        let afterPause = Self.attempt(bumping: .viewerAction)
        XCTAssertTrue(
            captured.stillCurrent(afterPause, scopes: [.lifecycle, .preparedAlignment])
        )
        XCTAssertEqual(
            captured.staleScopes(afterPause, scopes: [.lifecycle, .preparedAlignment]),
            []
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
