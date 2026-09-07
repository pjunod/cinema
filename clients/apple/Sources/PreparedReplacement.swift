import Foundation

/// The client half of a prepared quality handoff.
///
/// The server stages a whole second session — a real encoder, a durable row,
/// an actor slot — and then waits to be told what happened to it. Everything
/// in this file except `PreparedSuccessorHost` is deliberately free of
/// AVFoundation, because the rules that decide a viewer's picture are the ones
/// that must be provable in a unit test: which acknowledgement is owed, in
/// what order, whether a repeated action is a second preparation, and whether
/// a settlement can be lost.
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
    /// pathological peer cannot grow this without bound. The oldest
    /// *non-terminal* entry is what goes, never a settlement.
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
        if entries.count >= Self.capacity,
           let stale = entries.firstIndex(where: { !$0.state.isTerminal }) {
            entries.remove(at: stale)
        }
        guard entries.count < Self.capacity else { return }
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
    /// The switch has happened and the first qualifying frame is being waited
    /// for. The commit is not sent until it arrives (§5.4).
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
    /// was ignored; either way there is nothing left to build.
    case alreadySettled
    /// A different staging arrived while one was live. The old one is owed
    /// `aborted` and its pipeline freed before the new one is built.
    case supersede(previous: PreparedReplacementAction, next: PreparedReplacementAction)
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

    init() {}

    /// What an offered `prepare` means here. Pure: it decides nothing about
    /// pipelines, so a caller can consult it before spending anything.
    func offer(_ action: PreparedReplacementAction) -> PreparedReplacementOffer {
        if let active {
            if active.actionId == action.actionId { return .alreadyOpen }
            return .supersede(previous: active, next: action)
        }
        if settled.contains(action.actionId) { return .alreadySettled }
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
    mutating func noteSwitching() {
        guard active != nil else { return }
        phase = .switching
    }

    /// The successor's own first qualifying frame rendered. This is the only
    /// thing that may produce a `committed`.
    mutating func noteCommitted(firstFrameUnixMs: Int) {
        guard let active, firstFrameUnixMs > 0 else { return }
        acknowledgements.record(
            ActionAcknowledgement(
                actionId: active.actionId,
                state: .committed,
                // Echoed from the offer, never recomputed from the player: the
                // server compares it against the staged successor's own origin
                // and refuses a mismatch, so a value derived here would turn a
                // rounding difference into a refused commit.
                committedMediaOriginMs: active.mediaOriginMs,
                firstFrameUnixMs: firstFrameUnixMs
            )
        )
        retire(active.actionId)
    }

    /// This client is giving the staging up. Always owed; leaving it to the
    /// 330-second deadline holds the session's only preparation slot for the
    /// rest of its life (§C7).
    mutating func noteAbandoned(_ reason: PreparedReplacementAbandonment) {
        guard let active else { return }
        abandon(active, reason)
    }

    /// The same, for a staging that has just been superseded by a newer one.
    /// Ordered ahead of the newer staging's progress in the queue.
    mutating func noteSuperseded(_ action: PreparedReplacementAction) {
        abandon(action, .aborted)
    }

    private mutating func abandon(
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
    /// the handoff is `failed` and the incumbent is put back.
    static let firstFrameMs = 4_000
    /// How often the readiness and first-frame monitors look.
    static let pollMs = 100
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

    /// Put the primed successor in front of the viewer and retire the
    /// incumbent. Returns the wall clock of the successor's own first
    /// qualifying frame, or `nil` when the switch did not produce one — in
    /// which case the incumbent is still the authoritative stream.
    func commitPreparedSuccessor(_ action: PreparedReplacementAction) async -> Int?

    /// The in-place quality change this platform has always done. The
    /// fallback, and an ordinary outcome rather than an error branch.
    func fallBackToInPlaceReplacement(_ action: PreparedReplacementAction)

    /// One exchange, now, rather than at the next cadence: a settlement the
    /// server is holding a slot for should not wait out `next_exchange_ms`.
    func preparedSuccessorOwesAnExchange()

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
    /// False once a staging died before its successor ever became playable.
    ///
    /// A viewer pays for a preparation twice: once in the wait for the server
    /// to offer one, and again in the wait for a successor that will never be
    /// ready. Paying that on *every* quality change, on a server or a device
    /// that cannot produce a successor at all, would make this capability a
    /// regression rather than an improvement — and there are two live reasons
    /// it might not: the server's own "reserve and prime" phase is not
    /// implemented (`stage_prepared_successor` is documented **stage only**,
    /// so a staged playlist answers `503 media_owner_transition` until the
    /// pointer moves), and a device with one hardware decoder slot cannot hold
    /// a second pipeline whatever M5.5 measured about logical ones.
    ///
    /// So the first failure before readiness is taken as evidence about this
    /// playback, and the cost is paid once rather than per change. This is not
    /// a switch and nothing configures it: it is learned, it is forgotten when
    /// the player ends, and the day a successor becomes primeable this client
    /// starts using it with no change at all.
    private(set) var canOfferPreparation = true

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

    /// A `prepare` arrived. The whole of §5.2's "one successor at a time" rule
    /// lives here: a replay builds nothing, a settled staging builds nothing,
    /// and a genuinely new staging first settles and frees the old one.
    ///
    /// `filmPositionMs` is read lazily because the two branches that do not
    /// build must not ask the player anything.
    @discardableResult
    func offer(
        _ action: PreparedReplacementAction,
        filmPositionMs: @autoclosure () -> Int
    ) -> PreparedReplacementOffer {
        let offer = ledger.offer(action)
        switch offer {
        case .alreadyOpen, .alreadySettled:
            return offer
        case .supersede(let previous, let next):
            host?.discardPreparedSuccessor()
            ledger.noteSuperseded(previous)
            start(next, filmPositionMs: filmPositionMs())
        case .build(let next):
            start(next, filmPositionMs: filmPositionMs())
        }
        return offer
    }

    /// Whether a viewer-initiated change should wait for an offer at all.
    var shouldAskForPreparation: Bool { canOfferPreparation && !ledger.hasActivePreparation }

    private func start(_ action: PreparedReplacementAction, filmPositionMs: Int) {
        ledger.open(action)
        openedAtMs = now()
        guard host?.startPreparedSuccessor(action, filmPositionMs: filmPositionMs) == true else {
            // Not being able to start one is the ordinary one-slot outcome, so
            // it takes the ordinary path: settle, free, and change in place.
            abandon(.failed, fallingBackFor: action)
            return
        }
        host?.preparedSuccessorOwesAnExchange()
    }

    func successorIsMetadataReady() {
        guard ledger.hasActivePreparation else { return }
        ledger.noteMetadataReady()
        host?.preparedSuccessorOwesAnExchange()
    }

    func successorIsBuffered(throughMs: Int) {
        guard ledger.hasActivePreparation else { return }
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

    /// Give the staging up and take the in-place path instead.
    func abandon(_ reason: PreparedReplacementAbandonment) {
        guard let action = ledger.active else { return }
        abandon(reason, fallingBackFor: action)
    }

    /// Give the staging up without falling back — the viewer already moved on,
    /// so there is nothing left to change to.
    func abandonWithoutFallback(_ reason: PreparedReplacementAbandonment) {
        guard ledger.hasActivePreparation else { return }
        host?.discardPreparedSuccessor()
        ledger.noteAbandoned(reason)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
    }

    private func abandon(
        _ reason: PreparedReplacementAbandonment,
        fallingBackFor action: PreparedReplacementAction
    ) {
        // A successor that never became playable is evidence about this
        // playback, not about this attempt. One that failed after reaching
        // metadata is not: something produced media, so the next change is
        // worth trying.
        if reason == .failed && ledger.phase == .building { canOfferPreparation = false }
        host?.discardPreparedSuccessor()
        ledger.noteAbandoned(reason)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
        host?.fallBackToInPlaceReplacement(action)
    }

    /// Switch to the successor and settle it.
    ///
    /// The commit is sent only after the successor's own first qualifying
    /// frame, because the server's CAS moves the playback pointer on it: a
    /// premature commit points the pointer at a session the viewer is not
    /// watching. A switch that produces no frame inside the bound is a
    /// `failed` and the incumbent stays authoritative.
    func commit() async {
        guard let action = ledger.active,
              ledger.phase == .metadataReady || ledger.phase == .bufferReady
        else { return }
        ledger.noteSwitching()
        let startedAt = now()
        guard let firstFrameUnixMs = await host?.commitPreparedSuccessor(action) else {
            host?.recordPreparedFallbackInterruption(ms: max(0, now() - startedAt))
            abandon(.failed, fallingBackFor: action)
            return
        }
        ledger.noteCommitted(firstFrameUnixMs: firstFrameUnixMs)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
    }

    func acknowledgementDelivered(_ value: ActionAcknowledgement) {
        ledger.acknowledgementDelivered(value)
    }

    /// The player is going away. Anything still live is owed `aborted` before
    /// the reporter stops, and the pipeline is freed either way.
    func playerIsEnding() {
        // A new player is a new server, a new device state and a new source.
        // What this one learned about preparation does not travel.
        canOfferPreparation = true
        host?.discardPreparedSuccessor()
        guard ledger.hasActivePreparation else { return }
        ledger.noteAbandoned(.aborted)
        openedAtMs = nil
        host?.preparedSuccessorOwesAnExchange()
    }
}
