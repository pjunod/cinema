import Foundation

/// The client half of a prepared quality handoff.
///
/// The server stages a whole second session — an incarnation, a durable row,
/// the actor's one preparation slot — and then waits to be told what happened
/// to it. Everything in this file except `PreparedSuccessorHost` is
/// deliberately free of AVFoundation, because the rules that decide a viewer's
/// picture are the ones that must be provable in a unit test: which
/// acknowledgement is owed, in what order, whether a repeated action is a
/// second preparation, and whether a settlement can be lost.
///
/// `docs/playback-control/M6-CLIENT-REPLACEMENT-CONTRACT.md` is the contract
/// these rules implement; the §C-numbers in the comments point into it.

// MARK: - What the client owes the server

/// The acknowledgements waiting to go out, oldest first.
///
/// A queue rather than a slot for one reason: a new `action_id` arriving while
/// an older preparation is still live owes the older one `aborted` *and* owes
/// the newer one its progress, and exactly one acknowledgement rides each
/// exchange (§C6 — there is no array). A slot would drop one of them, and the
/// one it would drop is the terminal settlement that frees the server's
/// preparation slot for the rest of the session (§C7).
///
/// At most one entry per `action_id`, because progress is monotonic and the
/// server accepts a repeat of any state: a newer state for a staging already
/// queued replaces it rather than queueing behind it. A terminal state is
/// final and is never replaced.
struct PreparedAcknowledgementLedger: Equatable {
    /// A ceiling that can only be reached by a server staging faster than this
    /// client can settle, which is not a thing that happens; it exists so a
    /// pathological peer cannot grow this without bound.
    static let capacity = 8

    private(set) var entries: [ActionAcknowledgement] = []

    var head: ActionAcknowledgement? { entries.first }
    var isEmpty: Bool { entries.isEmpty }

    /// Record what a staging is owed now. Ignores anything the server would
    /// refuse, because a malformed acknowledgement costs the whole exchange
    /// rather than itself (§C7).
    mutating func record(_ value: ActionAcknowledgement) {
        guard value.isValid else { return }
        if let index = entries.firstIndex(where: { $0.actionId == value.actionId }) {
            guard !entries[index].state.isTerminal else { return }
            entries[index] = value
            return
        }
        // At the ceiling, the *incoming* value is the one worth keeping: it is
        // the newest thing this client knows, and if it is a settlement it is
        // the one holding a server slot open right now. So a progress report
        // goes first, and only if there is none does the oldest settlement go
        // — never the value being recorded, which is what an early `return`
        // here would have discarded.
        if entries.count >= Self.capacity {
            let victim = entries.firstIndex { !$0.state.isTerminal } ?? entries.startIndex
            entries.remove(at: victim)
        }
        entries.append(value)
    }

    /// An exchange carrying this acknowledgement came back. Until it does the
    /// same value rides every snapshot, which is what makes a settlement
    /// impossible to lose to the reporter's coalescing.
    ///
    /// A `200` is not proof the server bound it — an acknowledgement whose
    /// `action_id` belongs to an older staging is silently ignored (§C7) — but
    /// resending one forever would be worse than sending it once into a
    /// staging that has already been reaped.
    mutating func delivered(_ value: ActionAcknowledgement) {
        guard entries.first == value else { return }
        entries.removeFirst()
    }
}

// MARK: - The state machine

/// How far this client has got with the successor it was offered.
enum PreparedReplacementPhase: Equatable {
    /// Building the second pipeline; nothing is known about it yet.
    case building
    /// The successor's item is playable and its tracks are known.
    case metadataReady
    /// The successor is buffered past the switch point.
    case bufferReady
    /// The switch is in flight. A critical section: nothing may build,
    /// supersede or abandon a staging while its own commit is running, because
    /// the commit's outcome is about *this* staging and arrives after an await.
    case switching
}

/// What to do with an offered `prepare`.
enum PreparedReplacementOffer: Equatable {
    /// Build the second pipeline.
    case build(PreparedReplacementAction)
    /// The same staging, replayed. The server repeats a `prepare` on every
    /// exchange until it settles, so this is the ordinary case once one is
    /// live — and building twice would put two successors on one viewer's
    /// device for one staging.
    case alreadyOpen
    /// A staging this client has already settled. Its replay is in flight or
    /// was ignored; either way there is nothing left to build, and **nothing
    /// is going to change the stream**, so the caller still owes the viewer
    /// the in-place change they asked for.
    case alreadySettled
    /// A different staging arrived while one was live. The old one is owed
    /// `aborted` and its pipeline freed before the new one is built.
    case supersede(previous: PreparedReplacementAction, next: PreparedReplacementAction)
    /// A switch is in flight. Nothing may be built on top of one.
    case busy
    /// This playback has already proved it cannot prime a successor. The
    /// staging is settled immediately rather than left to the server's
    /// 330-second deadline, and nothing is built.
    case declined

    /// Whether the prepared path has taken ownership of the change that
    /// produced this offer. When false the caller must make the change itself,
    /// or the viewer's tap does nothing at all.
    var ownsTheChange: Bool {
        switch self {
        case .build, .alreadyOpen, .supersede: return true
        case .alreadySettled, .busy, .declined: return false
        }
    }
}

/// Why a preparation was given up on. The distinction is the server's: a
/// `failed` is this client saying the successor could not be made ready, and
/// an `aborted` is this client saying the viewer moved on. Both free the
/// server's slot; only the pair of them tells an operator which happened.
enum PreparedReplacementAbandonment: Equatable {
    /// The successor's item failed, or a readiness bound elapsed.
    case failed
    /// The viewer seeked, changed quality again, dismissed, or the session
    /// ended.
    case aborted

    var state: AcknowledgementState { self == .failed ? .failed : .aborted }
}

/// How a switch ended.
enum PreparedCommitOutcome: Equatable {
    /// The switch happened and the successor's own first qualifying frame
    /// rendered at this wall clock.
    case committed(firstFrameUnixMs: Int)
    /// **Nothing was switched.** The incumbent's item, session and pointer are
    /// exactly as they were, so the ordinary in-place change still applies and
    /// this says nothing about whether a successor could have been primed.
    case refused
    /// The switch happened and no qualifying frame arrived inside the bound.
    /// There is no incumbent to put back — its item is gone and its session
    /// released — so this is both a settlement and a reopen, and the picture
    /// the viewer is looking at is frozen until that reopen lands.
    case switchedWithoutAFrame
}

/// One preparation at a time, and everything owed about it.
///
/// Deliberately not `@MainActor` and deliberately free of AVFoundation: this
/// is the half of a prepared handoff that a test can drive end to end.
struct PreparedReplacementLedger: Equatable {
    /// How many settled stagings are remembered, so their replays are ignored
    /// rather than rebuilt. The server replays a `prepare` until the
    /// settlement lands, and a client that rebuilt on the replay of a
    /// preparation it had just committed would build a successor for a session
    /// it is already playing.
    static let settledMemory = 8

    private(set) var active: PreparedReplacementAction?
    private(set) var phase: PreparedReplacementPhase = .building
    private(set) var acknowledgements = PreparedAcknowledgementLedger()
    private var settled: [String] = []

    var pendingAcknowledgement: ActionAcknowledgement? { acknowledgements.head }
    var hasActivePreparation: Bool { active != nil }
    /// A switch is in flight, and nothing may disturb it.
    var isSwitching: Bool { active != nil && phase == .switching }

    init() {}

    /// What an offered `prepare` means here. Pure: it decides nothing about
    /// pipelines, so a caller can consult it before spending anything.
    func offer(_ action: PreparedReplacementAction, canPrepare: Bool = true) -> PreparedReplacementOffer {
        if isSwitching { return .busy }
        if let active {
            if active.actionId == action.actionId { return .alreadyOpen }
            return .supersede(previous: active, next: action)
        }
        if settled.contains(action.actionId) { return .alreadySettled }
        guard canPrepare else { return .declined }
        return .build(action)
    }

    /// Take the offered staging as this ledger's live one. The caller has
    /// already dealt with whatever `offer` told it about.
    mutating func open(_ action: PreparedReplacementAction) {
        active = action
        phase = .building
    }

    /// The successor's item is playable and its tracks are known.
    mutating func noteMetadataReady() {
        guard let active, phase == .building else { return }
        phase = .metadataReady
        acknowledgements.record(
            ActionAcknowledgement(actionId: active.actionId, state: .metadataReady)
        )
    }

    /// The successor is buffered past the switch point. Monotonic: a later
    /// reading replaces an earlier one, and it can be reported before
    /// `metadata_ready` has been sent because progress states are optional.
    mutating func noteBufferReady(bufferedThroughMs: Int) {
        guard let active, phase == .building || phase == .metadataReady || phase == .bufferReady
        else { return }
        phase = .bufferReady
        acknowledgements.record(
            ActionAcknowledgement(
                actionId: active.actionId,
                state: .bufferReady,
                bufferedThroughMs: max(0, bufferedThroughMs)
            )
        )
    }

    /// The switch is under way. Nothing is sent yet: a commit is a claim that
    /// the viewer is already watching the successor, and the server's CAS
    /// moves the playback pointer on it.
    @discardableResult
    mutating func noteSwitching(_ action: PreparedReplacementAction) -> Bool {
        guard active?.actionId == action.actionId,
              phase == .metadataReady || phase == .bufferReady
        else { return false }
        phase = .switching
        return true
    }

    /// The successor's own first qualifying frame rendered. This is the only
    /// thing that may produce a `committed`.
    ///
    /// Named rather than implied: a commit takes minutes of wall clock in the
    /// worst case and the ledger's `active` can have moved on, so settling
    /// "whatever is current" would stamp one staging's frame with another
    /// staging's identity — and the server's `bound_preparation_acknowledgement`
    /// would accept it and move the pointer to a session nothing displayed.
    mutating func noteCommitted(_ action: PreparedReplacementAction, firstFrameUnixMs: Int) {
        guard firstFrameUnixMs > 0 else { return }
        acknowledgements.record(
            ActionAcknowledgement(
                actionId: action.actionId,
                state: .committed,
                // Echoed from the offer, never recomputed from the player: the
                // server compares it against the staged successor's own origin
                // and refuses a mismatch, so a value derived here would turn a
                // rounding difference into a refused commit.
                committedMediaOriginMs: action.mediaOriginMs,
                firstFrameUnixMs: firstFrameUnixMs
            )
        )
        retire(action.actionId)
    }

    /// This client is giving the live staging up. Always owed; leaving it to
    /// the 330-second deadline holds the session's only preparation slot for
    /// the rest of its life (§C7).
    mutating func noteAbandoned(_ reason: PreparedReplacementAbandonment) {
        guard let active else { return }
        noteAbandoned(active, reason)
    }

    /// The same, for a named staging — one that has been superseded, declined,
    /// or whose commit resolved after the ledger moved on.
    mutating func noteAbandoned(
        _ action: PreparedReplacementAction,
        _ reason: PreparedReplacementAbandonment
    ) {
        acknowledgements.record(
            ActionAcknowledgement(actionId: action.actionId, state: reason.state)
        )
        retire(action.actionId)
    }

    private mutating func retire(_ actionId: String) {
        if active?.actionId == actionId {
            active = nil
            phase = .building
        }
        guard !settled.contains(actionId) else { return }
        settled.append(actionId)
        if settled.count > Self.settledMemory { settled.removeFirst() }
    }

    /// An exchange carrying this acknowledgement came back.
    mutating func acknowledgementDelivered(_ value: ActionAcknowledgement) {
        acknowledgements.delivered(value)
    }
}

// MARK: - Bounds

/// How long a successor may take, and how much runway makes it worth
/// switching to.
///
/// These are this platform's answer to an open question the contract records
/// rather than settles (§C14: "how long a client may wait for an action before
/// falling back"). Every one of them is short on purpose: the fallback is an
/// ordinary outcome, not an error branch (§4), and a viewer waiting on a
/// second pipeline that will not come is worse off than one who took the
/// in-place change immediately.
enum PreparedReplacementBounds {
    /// From building to `.readyToPlay`. Past it the successor is `failed`.
    static let metadataMs = 6_000
    /// From building to enough runway to switch. Past it the successor is
    /// `failed` even if its metadata arrived.
    static let readinessMs = 12_000
    /// How much runway past the switch point counts as buffered. Below a
    /// couple of seconds the switch trades a quality change for a stall.
    static let switchRunwayMs = 2_000
    /// From the switch to the successor's own first qualifying frame. Past it
    /// the picture is frozen and the only way out is a reopen, so this is
    /// generous where the readiness bounds are mean.
    static let firstFrameMs = 6_000
    /// From the commit's alignment seek to the seek coming back.
    ///
    /// The successor is already playable and buffered past the point it was
    /// staged at, so the ordinary alignment is a seek inside loaded media and
    /// costs tens of milliseconds. What this bound is really for is the case
    /// where it is not: priming ran long, the incumbent outran the successor's
    /// buffered runway, and the alignment has to fetch. Four seconds is one
    /// segment fetch's worth of patience on a link that is already streaming,
    /// and it is spent while the incumbent is still on the layer — so the cost
    /// of being wrong is a late in-place change, never a frozen picture.
    ///
    /// It is deliberately below `firstFrameMs`: a switch that has already
    /// happened has no incumbent to go back to, and that is worth waiting
    /// longer for than one that has not happened yet.
    static let alignmentMs = 4_000
    /// How often the readiness and first-frame monitors look.
    static let pollMs = 100
}

// MARK: - The commit's rendezvous

/// Where the successor has to be standing before the viewer is allowed to see
/// it, and what it means when it cannot get there.
///
/// The successor is seeked exactly once, when its item first becomes playable,
/// to the film position the offer named — and it is never played. The
/// incumbent meanwhile keeps running, for the whole of the viewer's wait and
/// the whole of priming. Exposing the item where it was staged rewinds the film
/// by that difference, and the commit boundary then rejects every frame until
/// playback catches back up.
struct PreparedCommitRendezvous: Equatable {
    /// The film position the switch happens at.
    let filmPositionMs: Int
    /// Where that falls in the successor's own timeline, whose zero is the
    /// staging's `media_origin_ms`.
    let itemPositionMs: Int

    /// Never behind the staged position: a successor asked to seek backwards
    /// from where it was primed would fetch media the switch does not need.
    static func plan(
        stagedFilmPositionMs: Int,
        incumbentFilmPositionMs: Int,
        mediaOriginMs: Int
    ) -> PreparedCommitRendezvous {
        let film = max(max(0, stagedFilmPositionMs), max(0, incumbentFilmPositionMs))
        return PreparedCommitRendezvous(
            filmPositionMs: film,
            itemPositionMs: max(0, film - mediaOriginMs)
        )
    }

    /// What a commit owes when the alignment does not land inside its bound.
    ///
    /// `refused`, not `failed`, because **nothing was switched**: the
    /// incumbent's item, session and pointer are exactly as they were. That
    /// makes this an ordinary in-place change — one reopen at the position the
    /// viewer has actually reached, one `aborted` freeing the server's single
    /// preparation slot, and a coordinator out of `.switching` and able to take
    /// the next change. Leaving the commit suspended instead lost all four: the
    /// tap produced nothing at all, because the prepared path had already
    /// claimed it.
    static let outcomeWhenAlignmentCannotLand = PreparedCommitOutcome.refused
}

/// Wait for a value another task will produce, and give up on it.
///
/// Deliberately not `withTaskGroup`. A task group waits for every child at
/// scope exit, and the thing this bounds — an AVFoundation seek — does not
/// observe cancellation, so the group would wait out the exact hang the bound
/// exists to escape. The producer is left to resolve, or not, on its own, and
/// nothing after the bound reads it.
///
/// Takes its clock and its sleep rather than owning them, so a test can drive
/// it to its deadline instead of spending four seconds of wall clock there.
@MainActor
func awaitBoundedValue<Value>(
    boundMs: Int,
    pollMs: Int,
    now: () -> Int,
    sleep: (Int) async -> Void,
    read: () -> Value?
) async -> Value? {
    let deadline = now() + max(0, boundMs)
    while true {
        if let value = read() { return value }
        // Read before the deadline test and once more after it: a value that
        // lands in the final poll interval is still the answer.
        if now() >= deadline { return read() }
        await sleep(max(1, pollMs))
    }
}

// MARK: - Waiting for the offer with the picture up

/// What a directed selection change is waiting for after it has been
/// published, read off each exchange's answer.
///
/// Pure, and in this file for the reason the file gives at the top: the rules
/// that decide a viewer's picture are the ones that must be provable in a unit
/// test. The controller feeds it answers and the clock; it says what to do,
/// and it touches no player.
///
/// What it replaces is a 1.5-second `askForAction`, which could only ever see
/// the answer to the exchange that carried the ask — and the server spawns the
/// candidate *after* building that response, so a `prepare` is never on it.
/// This waits across exchanges instead, with the incumbent still playing.
struct PreparedOfferWait: Equatable {
    enum Step: Equatable {
        /// Keep the incumbent playing; exchange again after `nextExchangeMs`.
        case keepWaiting(nextExchangeMs: Int)
        /// A `prepare` for this ask arrived — hand it to the coordinator.
        case offered(PreparedReplacementAction)
        /// The server said `none` after accepting the ask, or the bound
        /// expired: reopen in place now, at the CURRENT film position.
        case reopen(reason: String)
    }

    /// D1: twelve seconds from the tap. Long enough for a server to stage a
    /// whole second session; short enough that a viewer whose change is never
    /// going to be prepared is not left tapping at a picture that will not
    /// change.
    static let boundMs = 12_000
    /// 1 Hz while the server says `staging`.
    static let stagingCadenceMs = 1_000

    /// What a server that is building a candidate for this ask answers with.
    static let stagingPreparation = "staging"
    /// What a server that has decided there will be no candidate answers with.
    /// **Absence is not this value** — see `observe`.
    static let declinedPreparation = "none"

    /// Monotonic milliseconds at the viewer's tap.
    let tappedAtMs: Int
    /// The first request sequence that could carry this ask's answer.
    let floorSequence: Int
    /// Whether any exchange at or past the floor has answered yet.
    ///
    /// The exchange that carried the ask is answered by a server that has only
    /// just accepted it: a new server says `staging` on it, an old one says
    /// nothing at all. A `none` on *that* exchange is therefore not a decline,
    /// and reading it as one would reopen immediately on every server whose
    /// ordering differs by a single hop.
    private(set) var sawAcceptedAnswer = false
    /// The sequence of the exchange that carried the ask — the first one at or
    /// past the floor to come back.
    ///
    /// A sequence, not a flag, and that is the whole of the rule. The caller
    /// can inspect the same answer after a timer wake before the next exchange
    /// lands, so "have I observed an accepted answer before" can become true
    /// for that same response. Keyed that way this rule declined after the
    /// dispatch exchange instead of one exchange later — precisely
    /// the behaviour it was written to prevent, and invisible to any test whose
    /// only timing assertion is "inside the bound".
    ///
    /// It matters because a dispatch-exchange `none` is not final: the server
    /// withholds a purpose while the incumbent is waiting for capacity, and
    /// that is transient. A viewer who taps quality in that window would be
    /// reopened before the exchange that would have said `staging`.
    private(set) var dispatchSequence: Int?
    /// Whether the newest accepted answer said `staging` — the only thing that
    /// earns an extra exchange at `stagingCadenceMs`. Absent is not staging:
    /// an old server is waited out by the bound rather than nudged.
    private(set) var lastSaidStaging = false

    init(tappedAtMs: Int, floorSequence: Int) {
        self.tappedAtMs = tappedAtMs
        self.floorSequence = floorSequence
    }

    mutating func observe(answer: PlaybackControlAnswer?, nowMs: Int) -> Step {
        // First, and before an exchange has been accepted at all. A server
        // that never answers this ask is exactly what the bound exists for,
        // and a bound checked after the answer branches would never fire on
        // one.
        if nowMs - tappedAtMs >= Self.boundMs { return .reopen(reason: "timed_out") }
        guard let answer, answer.requestSequence >= floorSequence else {
            // Nothing yet, or an answer from before the ask — including a
            // replayed `prepare` for an older staging, which is why this test
            // comes before the one below it. Not evidence about this change in
            // either direction.
            return .keepWaiting(nextExchangeMs: Self.stagingCadenceMs)
        }
        // An offer in hand outranks any hint about one: `preparation` is
        // progress reporting, and a `prepare` is the thing it was reporting
        // progress towards.
        if let action = answer.action, let prepared = PreparedReplacementAction(action) {
            return .offered(prepared)
        }
        let dispatch = dispatchSequence ?? answer.requestSequence
        dispatchSequence = dispatch
        sawAcceptedAnswer = true
        lastSaidStaging = answer.preparation == Self.stagingPreparation
        // Absence is never a decline. Older servers and relays do not send
        // this field at all, and reading a missing field as `none` would turn
        // every one of them into an instant reopen — the exact regression this
        // milestone exists to remove.
        //
        // Nor is the dispatch exchange's own answer one, however many times it
        // is observed: only a *later* exchange has looked.
        if answer.preparation == Self.declinedPreparation,
           answer.requestSequence > dispatch {
            return .reopen(reason: "declined")
        }
        return .keepWaiting(nextExchangeMs: Self.stagingCadenceMs)
    }
}

/// Where a quality-only change reopens when the prepared path hands it back.
///
/// A quality change changes what is delivered, not where the film is — but it
/// still creates a presentation destination at the tap, because the progress
/// bar, the presentation monitor and the stall-recovery suppression are all
/// built on one. Resuming *at* that destination was harmless while the wait
/// was 1.5 seconds and could not succeed; at twelve seconds it rewinds the
/// film by the whole wait. So the destination is kept and the position is read
/// again when the fallback actually runs.
enum QualityChangeReopen {
    /// Where the film is now, for a change whose own pin says where it was.
    ///
    /// `livePositionMs` is the incumbent's own clock, or nil when it cannot
    /// honestly be read — nothing attached, or a replacement already in flight
    /// — in which case the pin is the best reading anyone has.
    ///
    /// `carryingASeek` is the one case where the pin is NOT this change's own:
    /// a quality change made on top of a seek that had not landed inherits the
    /// viewer's destination, and the live clock is the position they were
    /// leaving. The live clock is never taken out from under a seek.
    static func positionNowMs(
        pinnedAtTapMs: Int,
        livePositionMs: Int?,
        carryingASeek: Bool
    ) -> Int {
        guard !carryingASeek, let livePositionMs else { return pinnedAtTapMs }
        // Never backwards. A clock read during a rebase, or one that has not
        // caught up, must not send the viewer behind where they tapped.
        return max(pinnedAtTapMs, livePositionMs)
    }

    /// `nil` when a viewer seek has taken the position since the tap: the seek
    /// owns where the film is, and it already carries the new selection
    /// because `recipeRevision.change()` ran at the tap. Reopening here as
    /// well would change the stream twice for one tap, and land the second one
    /// in the wrong place.
    static func target(
        seekGenerationAtTap: Int,
        seekGenerationNow: Int,
        positionNowMs: Int
    ) -> Int? {
        guard seekGenerationAtTap == seekGenerationNow else { return nil }
        return max(0, positionNowMs)
    }
}

// MARK: - The pipeline this client drives

/// The AVFoundation half, behind a protocol so the ledger above can be driven
/// end to end without one.
///
/// Everything here is `@MainActor` because `PlayerController` is, and because
/// a second `AVPlayer` whose lifetime is decided on two actors is exactly the
/// leak that survives a tvOS screensaver.
@MainActor
protocol PreparedSuccessorHost: AnyObject {
    /// Build the muted, layer-less second pipeline for this staging and prime
    /// it to `filmPositionMs`. `false` means it could not be started at all —
    /// an address that will not resolve, or no player to replace — which is a
    /// `failed`, not a crash.
    func startPreparedSuccessor(
        _ action: PreparedReplacementAction,
        filmPositionMs: Int
    ) -> Bool

    /// Release the second pipeline. Idempotent, and called on every exit.
    func discardPreparedSuccessor()

    /// Put the primed successor in front of the viewer. The committed control
    /// exchange, not an eager DELETE from the client, retires the incumbent.
    /// See `PreparedCommitOutcome` — the three answers differ in whether
    /// anything was switched, which decides the settlement and fallback.
    func commitPreparedSuccessor(_ action: PreparedReplacementAction) async -> PreparedCommitOutcome

    /// The in-place quality change this platform has always done. The
    /// fallback, and an ordinary outcome rather than an error branch.
    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction)

    /// One exchange, now, rather than at the next cadence: a settlement the
    /// server is holding a slot for should not wait out `next_exchange_ms`.
    func preparedSuccessorOwesAnExchange()

    /// A live staging was given up. Contract §3.3 row 18: the incumbent is
    /// untouched — nothing was ever switched to — so this draws nothing and
    /// the event is the only trace it happened at all.
    func notePreparedSuccessorAbandoned(_ reason: PreparedReplacementAbandonment)

    /// Record how long the viewer's picture was interrupted by a fallback.
    /// Apple's fallback interruption is unmeasured because Apple passed dual
    /// preparation and never exercised it; this is the instrument that closes
    /// that residual.
    func recordPreparedFallbackInterruption(ms: Int)
}

// MARK: - The coordinator

/// One preparation, from the action arriving to the settlement going out.
///
/// It owns the ledger and drives the host; it owns no AVFoundation state, so
/// every rule it applies — build once, abandon exactly once, never commit
/// before a frame, always settle — is testable against a host that records
/// what it was asked to do.
@MainActor
final class PreparedReplacementCoordinator {
    private weak var host: PreparedSuccessorHost?
    private(set) var ledger = PreparedReplacementLedger()
    private let now: () -> Int
    /// The wall clock at which the live staging was opened, for the readiness
    /// bounds.
    private(set) var openedAtMs: Int?
    init(host: PreparedSuccessorHost?, now: @escaping () -> Int = {
        Int(Date().timeIntervalSince1970 * 1_000)
    }) {
        self.host = host
        self.now = now
    }

    var pendingAcknowledgement: ActionAcknowledgement? { ledger.pendingAcknowledgement }
    var hasActivePreparation: Bool { ledger.hasActivePreparation }
    var activeAction: PreparedReplacementAction? { ledger.active }
    var phase: PreparedReplacementPhase { ledger.phase }

    /// Whether a viewer-initiated change should wait for an offer at all.
    var shouldAskForPreparation: Bool {
        !ledger.hasActivePreparation
    }

    /// A `prepare` arrived. The whole of §5.2's "one successor at a time" rule
    /// lives here: a replay builds nothing, a settled staging builds nothing, a
    /// switch in flight is never disturbed, and a genuinely new staging first
    /// settles and frees the old one.
    ///
    /// `filmPositionMs` is read lazily because the four branches that do not
    /// build must not ask the player anything.
    @discardableResult
    func offer(
        _ action: PreparedReplacementAction,
        filmPositionMs: @autoclosure () -> Int
    ) -> PreparedReplacementOffer {
        let offer = ledger.offer(action, canPrepare: true)
        switch offer {
        case .alreadyOpen, .alreadySettled, .busy:
            return offer
        case .declined:
            // Nothing is built, and the staging is settled at once rather than
            // left to the deadline: a server slot held for 330 seconds costs
            // this session every later preparation, including the one that
            // would work if the priming phase landed mid-film.
            ledger.noteAbandoned(action, .aborted)
            host?.preparedSuccessorOwesAnExchange()
            return offer
        case .supersede(let previous, let next):
            host?.discardPreparedSuccessor()
            ledger.noteAbandoned(previous, .aborted)
            start(next, filmPositionMs: filmPositionMs())
        case .build(let next):
            start(next, filmPositionMs: filmPositionMs())
        }
        return offer
    }

    private func start(_ action: PreparedReplacementAction, filmPositionMs: Int) {
        ledger.open(action)
        openedAtMs = now()
        guard host?.startPreparedSuccessor(action, filmPositionMs: filmPositionMs) == true else {
            // Not being able to start one is the ordinary one-slot outcome, so
            // it takes the ordinary path: settle, free, and change in place.
            abandon(.failed, settling: action)
            return
        }
        host?.preparedSuccessorOwesAnExchange()
    }

    func successorIsMetadataReady() {
        guard ledger.hasActivePreparation, !ledger.isSwitching else { return }
        ledger.noteMetadataReady()
        host?.preparedSuccessorOwesAnExchange()
    }

    func successorIsBuffered(throughMs: Int) {
        guard ledger.hasActivePreparation, !ledger.isSwitching else { return }
        ledger.noteBufferReady(bufferedThroughMs: throughMs)
        host?.preparedSuccessorOwesAnExchange()
    }

    /// Has the live staging outlived one of its bounds?
    func readinessBoundElapsed() -> Bool {
        guard let openedAtMs, ledger.hasActivePreparation else { return false }
        let elapsed = now() - openedAtMs
        switch ledger.phase {
        case .building:
            return elapsed >= PreparedReplacementBounds.metadataMs
        case .metadataReady, .bufferReady:
            return elapsed >= PreparedReplacementBounds.readinessMs
        case .switching:
            return false
        }
    }

    /// Remaining time on the current stage's original monotonic deadline.
    /// Observation may wake earlier, but it must not restart the 6 s or 12 s
    /// clock when the item publishes another status value.
    func readinessRemainingMs() -> Int? {
        guard let openedAtMs, ledger.hasActivePreparation else { return nil }
        let bound: Int
        switch ledger.phase {
        case .building: bound = PreparedReplacementBounds.metadataMs
        case .metadataReady, .bufferReady: bound = PreparedReplacementBounds.readinessMs
        case .switching: return nil
        }
        return max(0, bound - (now() - openedAtMs))
    }

    /// Give the staging up and take the in-place path instead.
    func abandon(_ reason: PreparedReplacementAbandonment) {
        guard let action = ledger.active, !ledger.isSwitching else { return }
        abandon(reason, settling: action)
    }

    /// Give the staging up without falling back — the viewer already moved on,
    /// so there is nothing left to change to.
    ///
    /// Never during a switch: the commit's own outcome is what settles that
    /// staging, and a viewer command landing mid-switch is applied by the
    /// ordinary path once the switch has resolved.
    func abandonWithoutFallback(_ reason: PreparedReplacementAbandonment) {
        guard let action = ledger.active, !ledger.isSwitching else { return }
        host?.notePreparedSuccessorAbandoned(reason)
        host?.discardPreparedSuccessor()
        ledger.noteAbandoned(action, reason)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
    }

    private func abandon(
        _ reason: PreparedReplacementAbandonment,
        settling action: PreparedReplacementAction
    ) {
        host?.notePreparedSuccessorAbandoned(reason)
        host?.discardPreparedSuccessor()
        ledger.noteAbandoned(action, reason)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
        host?.fallBackToInPlaceReplacement(action)
    }

    /// Switch to the successor and settle it.
    ///
    /// The commit is sent only after the successor's own first qualifying
    /// frame, because the server's CAS moves the playback pointer on it: a
    /// premature commit points the pointer at a session the viewer is not
    /// watching. Three outcomes, and they are genuinely different:
    ///
    /// - **committed** — the switch happened and a frame proved it.
    /// - **refused** — nothing was switched, so the incumbent is untouched and
    ///   this is an `aborted` plus the ordinary in-place change. It says
    ///   nothing about whether a successor could have been primed, so it does
    ///   not stop this playback asking again.
    /// - **switchedWithoutAFrame** — the local item was replaced, but the
    ///   predecessor remains the server's authority because no commit was
    ///   sent. This is a `failed` *and* an ordinary reopen, and the
    ///   interruption is measured.
    ///
    /// The staging is named through every branch. `ledger.active` cannot be
    /// trusted across the await — `.switching` stops anything else opening
    /// one, but the ledger is settled by id either way.
    func commit() async {
        guard let action = ledger.active, ledger.noteSwitching(action) else { return }
        let startedAt = now()
        let outcome = await host?.commitPreparedSuccessor(action) ?? .refused
        openedAtMs = nil
        switch outcome {
        case .committed(let firstFrameUnixMs):
            ledger.noteCommitted(action, firstFrameUnixMs: firstFrameUnixMs)
            host?.preparedSuccessorOwesAnExchange()
        case .refused:
            host?.notePreparedSuccessorAbandoned(.aborted)
            host?.discardPreparedSuccessor()
            ledger.noteAbandoned(action, .aborted)
            host?.preparedSuccessorOwesAnExchange()
            host?.fallBackToInPlaceReplacement(action)
        case .switchedWithoutAFrame:
            host?.recordPreparedFallbackInterruption(ms: max(0, now() - startedAt))
            host?.notePreparedSuccessorAbandoned(.failed)
            host?.discardPreparedSuccessor()
            ledger.noteAbandoned(action, .failed)
            host?.preparedSuccessorOwesAnExchange()
            host?.fallBackToInPlaceReplacement(action)
        }
    }

    func acknowledgementDelivered(_ value: ActionAcknowledgement) {
        ledger.acknowledgementDelivered(value)
    }

    /// The player is going away. Anything still live is owed `aborted` before
    /// the reporter stops, and the pipeline is freed either way.
    func playerIsEnding() {
        host?.discardPreparedSuccessor()
        guard ledger.hasActivePreparation else { return }
        ledger.noteAbandoned(.aborted)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
    }
}
