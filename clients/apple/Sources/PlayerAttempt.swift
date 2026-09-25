import Foundation

/// What a continuation captured before it awaited, and the comparison that
/// tells it whether the work it was going to do still belongs to anyone.
///
/// `PlayerController` carries nine independent epoch counters. Each one names
/// a different thing that can go out from under a suspended continuation, and
/// each one is advanced by a different event — `docs/clients/
/// APPLE-PLAYER-CONTROLLER-ATTEMPT-AND-OBSERVATION.md` §2.1 has the table. A
/// continuation that crossed an `await` therefore cannot ask one question
/// ("am I still current?"); it has to ask about the subset of the nine that
/// invalidates *its* work, and only that subset.
///
/// Before this type, every one of those questions was a hand-written
/// conjunction at the continuation — `self.openGeneration == g &&
/// self.viewerActionEpoch == e && …` — which is correct only for as long as
/// whoever writes the next one remembers which counters belong in it, and
/// which was already spelled 58 different times. `Attempt` does not change
/// what any of them compares. It gives the comparison a name, so the scope
/// set is written down where a reader can see it, and so the census in
/// `validation/attempt_census.py` can tell a fence that named its scopes from
/// one that did not.
///
/// **The nine stay nine.** Merging any two of them would make a viewer's pause
/// cancel an unrelated prepared replacement, or let a new `open()` be survived
/// by an alignment seek belonging to the old one. `stillCurrent(_:scopes:)`
/// compares exactly the scopes it is given and nothing else; a migration that
/// widens a scope set is a behaviour change, not a restructuring.
struct Attempt: Equatable, Sendable {
    /// One epoch counter, named by what it invalidates. The nine cases are the
    /// nine counters; nothing else in the controller is a scope.
    ///
    /// `createRetryExpiredEpoch` is deliberately absent. It is an optional
    /// latch rather than a monotonic counter — the create-retry watchdog
    /// compares it for equality with a captured epoch to decide whether *this*
    /// sequence's expiry has already fired — so it is a predicate beside an
    /// `Attempt`, not a tenth scope inside one.
    enum Scope: String, CaseIterable, Sendable {
        case lifecycle
        case open
        case viewerAction
        case initialDecision
        case createRetry
        case preparedAlignment
        case seek
        case pgsSelection
        case pgsItem
    }

    /// `lifecycleGeneration`: a title owns its decision and every asynchronous
    /// thing that follows from it.
    let lifecycle: Int
    /// `openGeneration`: one attach attempt.
    let open: Int
    /// `viewerActionEpoch`: an explicit Play, Pause, seek or track change.
    /// Session generation alone cannot see any of those.
    let viewerAction: Int
    /// `initialDecisionGeneration`: the pre-play decision fetch and its
    /// deadline task.
    let initialDecision: Int
    /// `createRetryEpoch`: one create sequence and its watchdog.
    let createRetry: Int
    /// `preparedAlignmentGeneration`: one prepared-commit alignment seek.
    let preparedAlignment: Int
    /// `seekState.generation`: one pending seek target.
    let seek: Int
    /// `pgsOverlaySelectionGeneration`: one PGS overlay track selection.
    let pgsSelection: Int
    /// `pgsOverlayItemGeneration`: one attached item's overlay windows.
    let pgsItem: Int
    /// The `AVPlayerItem` attached when the snapshot was taken, or `nil` if
    /// none was.
    ///
    /// This is **not** a scope and `stillCurrent(_:scopes:)` never reads it.
    /// Call sites that care about item identity already hold the item itself
    /// and compare it with `===`, which is stronger: it survives an
    /// `ObjectIdentifier` being reused by a later allocation at the same
    /// address. What this field is for is the `attempt_stale` client-log
    /// event: `PlayerController.attemptStaleDetail(fence:captured:now:)`
    /// reports through `hasSameItem(as:)` whether the item was replaced too.
    let item: ObjectIdentifier?

    /// This attempt's value for one scope.
    func value(of scope: Scope) -> Int {
        switch scope {
        case .lifecycle: return lifecycle
        case .open: return open
        case .viewerAction: return viewerAction
        case .initialDecision: return initialDecision
        case .createRetry: return createRetry
        case .preparedAlignment: return preparedAlignment
        case .seek: return seek
        case .pgsSelection: return pgsSelection
        case .pgsItem: return pgsItem
        }
    }

    /// Whether the work this attempt was captured for still belongs to the
    /// current state, judged on the named scopes only.
    ///
    /// An empty scope set answers `true`: an attempt that depends on none of
    /// the nine is current by definition, and saying so here is better than a
    /// silent trap, because the call site that passes an empty set has simply
    /// written a fence with no epoch in it.
    func stillCurrent(_ now: Attempt, scopes: Set<Scope>) -> Bool {
        for scope in scopes where value(of: scope) != now.value(of: scope) {
            return false
        }
        return true
    }

    /// Which of the named scopes moved, in the declaration order of `Scope` so
    /// a log line is stable. `PlayerController.attemptStillCurrent(_:fence:)`
    /// puts it in the `attempt_stale` event it raises when a migrated fence
    /// refuses a continuation, to say *why* it was refused.
    func staleScopes(_ now: Attempt, scopes: Set<Scope>) -> [Scope] {
        Scope.allCases.filter { scopes.contains($0) && value(of: $0) != now.value(of: $0) }
    }

    /// Whether the same item object was attached at both snapshots. Both being
    /// `nil` counts as the same: no item then, no item now. Reported in the
    /// `attempt_stale` event; never a reason to refuse on its own.
    func hasSameItem(as now: Attempt) -> Bool {
        item == now.item
    }
}

/// Every continuation fence that has been migrated to `Attempt`, with the one
/// scope set it compares.
///
/// The set is the whole decision a fence makes — the plan's rule is that it is
/// copied from the conjunction the fence replaced and never widened — so it is
/// written down once, here, rather than as a literal at the call site where a
/// later edit could swap it unnoticed. Three things hold it still:
///
/// - `AttemptScopesTests` pins each case's set exactly, and drives a real
///   `PlayerController` through a viewer Pause and a new title to show every
///   fence refuses them through `PlayerController.attemptStillCurrent`;
/// - `validation/attempt-census.toml` `[fences]` records each case's set and
///   the one function that may use it, and the census fails on a difference
///   in either direction;
/// - the raw value is the `fence=` of the `attempt_stale` client-log event.
///
/// Adding a case is how the next fence is migrated: the case, its set, its
/// census row and the call site land together, and the allow-list row for the
/// conjunction it replaced goes down in the same commit.
enum AttemptFence: String, CaseIterable, Sendable {
    /// `beginSeekPresentationMonitor`: the eight-second deadline reopen.
    case seekPresentationDeadline = "seek_presentation_deadline"
    /// `makePeriodicPlaybackObservation`: the black-frame decode-failure
    /// handler. `started` stays beside it as a predicate.
    case blackFrameDecodeFailure = "black_frame_decode_failure"
    /// `retrySameDeliveryAfterStall`: the same-delivery reopen after the
    /// control ask.
    case stallRecovery = "stall_recovery"
    /// `handleItemFailure`: the failure ladder after its control ask.
    case itemFailureLadder = "item_failure_ladder"
    /// `issueSeek`: coalesced intent before reporting to control.
    case seekIntent = "seek_intent"
    /// `issueSeek`: the intent after the awaited control report.
    case seekIntentAfterControl = "seek_intent_after_control"
    /// `issueSeek`: the awaited native seek completion.
    case nativeSeekCompletion = "native_seek_completion"
    /// `issueSeek`: the native seek after awaited subtitle reconciliation.
    case nativeSeekAfterSelection = "native_seek_after_selection"
    /// `startRecoveryEvidencePoll`: one attachment's status stream. A viewer
    /// Pause does not end the poll; session identity is checked beside it.
    case recoveryEvidencePoll = "recovery_evidence_poll"

    /// The epochs this fence depends on — exactly the fields its old
    /// conjunction compared.
    var scopes: Set<Attempt.Scope> {
        switch self {
        case .seekPresentationDeadline: return [.open, .viewerAction, .seek]
        case .blackFrameDecodeFailure: return [.lifecycle, .viewerAction]
        case .stallRecovery: return [.open, .viewerAction]
        case .itemFailureLadder: return [.open, .viewerAction]
        case .seekIntent: return [.viewerAction, .seek]
        case .seekIntentAfterControl: return [.viewerAction, .seek]
        case .nativeSeekCompletion: return [.open, .viewerAction, .seek]
        case .nativeSeekAfterSelection: return [.open, .viewerAction, .seek]
        case .recoveryEvidencePoll: return [.open]
        }
    }
}
