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
    /// address. What this field is for is saying which item an attempt was
    /// captured against when a drift is described.
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
    /// a log line is stable. Used to say *why* a continuation was refused.
    func staleScopes(_ now: Attempt, scopes: Set<Scope>) -> [Scope] {
        Scope.allCases.filter { scopes.contains($0) && value(of: $0) != now.value(of: $0) }
    }

    /// Whether the same item object was attached at both snapshots. Both being
    /// `nil` counts as the same: no item then, no item now.
    func hasSameItem(as now: Attempt) -> Bool {
        item == now.item
    }
}
