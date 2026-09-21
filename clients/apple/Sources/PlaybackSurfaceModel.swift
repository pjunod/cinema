import Foundation

// The Apple presenter for the playback surface contract
// (docs/clients/PLAYBACK-SURFACE-CONTRACT.md §3, implementation plan §3.3).
//
// Recovery owns the player; the presenter owns the pixels. Everything in this
// file is a pure function of (faults, evidence, identities) → surface: it
// never pauses, resumes, seeks, reopens, cancels a timer or reports to the
// control plane, and it holds no reference to AVFoundation at all. The only
// things it emits besides a surface are log entries, which the caller forwards
// to the client log and the Playback debug ledger.
//
// The tables below are a transcription of
// `tests/playback/playback-surface-contract.json`, the way `PlayerInputRouting`
// transcribes `player-input-contract.json`. `AppleClientTests` decodes the
// fixture and fails if the two ever disagree, and runs all of the fixture's
// ordered-event cases against `PlaybackSurfaceModel`.

/// One typed fault. `attached` is the media generation the fault is *about*
/// (`PlayerController.openGeneration`); `intent` is the viewer request it
/// belongs to, when it belongs to one (`viewerActionEpoch` / the seek
/// generation). A fault about the attached media is retired by that media's
/// presentation evidence; a fault about a pending destination is retired only
/// by that destination settling or being superseded.
struct PlaybackFault: Equatable, Sendable {
    enum Class: String, Codable, CaseIterable, Sendable {
        case preparing, buffering, recovering, hold, degraded, refused, exhausted, stopped
    }

    enum Action: String, Codable, CaseIterable, Sendable {
        case keepWaiting = "keep_waiting"
        case retry
        case close
        case forceTranscode = "force_transcode"
        case signIn = "sign_in"
    }

    /// The class the source table gave this fault.
    ///
    /// `var`, not `let` as the plan's sketch spells it: the agreement rule
    /// (§3.2) demotes a blocking fault to `degraded` in place — keeping its
    /// actions, its position and its identity — and `media_owner_lost_410`
    /// re-classes to `stopped` when its owner stops. Both are the same fault
    /// changing class, not a new one, which is exactly why the ledger row and
    /// the Try again button survive the change.
    var cls: Class
    /// The source id from the fixture's `sources` table.
    let source: String
    /// The media generation this fault is about (`openGeneration`).
    let attached: Int
    /// The viewer request this fault belongs to, when it belongs to one.
    let intent: Int?
    /// `var` because a hidden app carries every raise time forward by the
    /// interval it was away, rather than letting wall time expire a notice
    /// nobody could read (§3.1).
    var raisedAt: ContinuousClock.Instant
    var positionMs: Int?
    var title: String?
    var detail: String?
    var actions: [Action]
    var playerStopped: Bool

    /// The class this fault becomes when its own owner stops the player.
    /// Set from the source row (`media_owner_lost_410` → `stopped`).
    fileprivate(set) var thenWhenStopped: Class?
    /// Whether the agreement rule has already demoted this fault. A demoted
    /// fault is never promoted by a later owner stop: its blocking sentence
    /// has already been answered by the picture.
    private(set) var demoted: Bool
    /// Raise order, so faults of equal severity break by recency.
    fileprivate(set) var seq: Int

    init(
        cls: Class,
        source: String,
        attached: Int,
        intent: Int? = nil,
        raisedAt: ContinuousClock.Instant,
        positionMs: Int? = nil,
        title: String? = nil,
        detail: String? = nil,
        actions: [Action] = [],
        playerStopped: Bool = false
    ) {
        self.cls = cls
        self.source = source
        self.attached = attached
        self.intent = intent
        self.raisedAt = raisedAt
        self.positionMs = positionMs
        self.title = title
        self.detail = detail
        self.actions = actions
        self.playerStopped = playerStopped
        self.thenWhenStopped = nil
        self.demoted = false
        self.seq = 0
    }

    fileprivate mutating func markDemoted() { demoted = true }
}

/// What is drawn over the picture. `blocking` is only ever reached over a
/// player its recovery owner has already stopped, or over a picture that is
/// not presenting.
struct PlaybackSurface: Equatable, Sendable {
    enum Kind: String, Sendable, CaseIterable {
        case none, indicator, banner, blocking
    }

    let kind: Kind
    let fault: PlaybackFault?

    static let empty = PlaybackSurface(kind: .none, fault: nil)

    var cls: PlaybackFault.Class? { fault?.cls }
    var source: String? { fault?.source }
    var title: String? { fault?.title }
    var detail: String? { fault?.detail }
    var actions: [PlaybackFault.Action] { fault?.actions ?? [] }
    var positionMs: Int? { fault?.positionMs }
    var attached: Int? { fault?.attached }
    var intent: Int? { fault?.intent }
    var playerStopped: Bool { fault?.playerStopped ?? false }
    var isDemoted: Bool { fault?.demoted ?? false }

    /// The player input contract's `failed` state: a BLOCKING surface whose
    /// class is a prompt or a terminal — a fault with actions the viewer must
    /// answer. A full-screen `preparing`/`buffering`/`recovering` covers the
    /// picture but asks nothing, so it keeps today's routing
    /// (PLAYBACK-SURFACE-CONTRACT.md §4).
    var entersFailedRouting: Bool {
        guard kind == .blocking, let fault else { return false }
        guard let rule = PlaybackSurfaceContract.classes[fault.cls] else { return false }
        return rule.severity == .prompt || rule.severity == .terminal
    }
}

/// One entry the presenter asks its caller to emit. The presenter knows the
/// fault; the caller adds `session_id`, `attempt` and the player snapshot,
/// which are not the presenter's to read (§3.6).
struct PlaybackSurfaceLog: Equatable, Sendable {
    enum Event: String, Sendable {
        case raised = "surface_raised"
        case cleared = "surface_cleared"
        case disagreement = "surface_disagreement"
        case logOnly = "surface_log_only"
        case error = "surface_error"
    }

    enum Reason: String, Sendable {
        case presenting
        case intentSettled = "intent_settled"
        case intentSuperseded = "intent_superseded"
        case attachedRetired = "attached_retired"
        case ownerSuccess = "owner_success"
        case ownerStopped = "owner_stopped"
        case playbackNotRequested = "playback_not_requested"
        case systemResumed = "system_resumed"
        case timer
        case user
    }

    enum Failure: String, Sendable, Error {
        case blockingWithoutStop = "blocking_without_stop"
        case sourceContextMismatch = "source_context_mismatch"
        case unknownSource = "unknown_source"
    }

    let event: Event
    var cls: PlaybackFault.Class?
    var source: String?
    /// The raiser's own sentence. Carried for every event, and load-bearing
    /// for `surface_log_only`: `log_only` is one source id standing for three
    /// unrelated facts (contract §3.3 row 18), so a row without this says
    /// "something the viewer was right not to see happened" and nothing else.
    var detail: String?
    var attached: Int?
    var intent: Int?
    var positionMs: Int?
    var actions: [PlaybackFault.Action]
    var playerStopped: Bool
    var by: Reason?
    var action: PlaybackFault.Action?
    var error: Failure?
    var context: PlaybackSurfaceModel.Context?

    init(
        event: Event,
        cls: PlaybackFault.Class? = nil,
        source: String? = nil,
        detail: String? = nil,
        attached: Int? = nil,
        intent: Int? = nil,
        positionMs: Int? = nil,
        actions: [PlaybackFault.Action] = [],
        playerStopped: Bool = false,
        by: Reason? = nil,
        action: PlaybackFault.Action? = nil,
        error: Failure? = nil,
        context: PlaybackSurfaceModel.Context? = nil
    ) {
        self.event = event
        self.cls = cls
        self.source = source
        self.detail = detail
        self.attached = attached
        self.intent = intent
        self.positionMs = positionMs
        self.actions = actions
        self.playerStopped = playerStopped
        self.by = by
        self.action = action
        self.error = error
        self.context = context
    }

    fileprivate init(_ event: Event, _ fault: PlaybackFault, by: Reason? = nil, action: PlaybackFault.Action? = nil) {
        self.init(
            event: event,
            cls: fault.cls,
            source: fault.source,
            detail: fault.detail,
            attached: fault.attached,
            intent: fault.intent,
            positionMs: fault.positionMs,
            actions: fault.actions,
            playerStopped: fault.playerStopped,
            by: by,
            action: action
        )
    }
}

/// The fixture's `classes`, `sources` and `timings`, transcribed.
enum PlaybackSurfaceContract {
    typealias Context = PlaybackSurfaceModel.Context

    enum Severity: String, Equatable, Sendable, CaseIterable {
        case notice, progress, prompt, terminal
    }

    enum Blocking: Equatable, Sendable {
        /// A notice beside the picture.
        case never
        /// Always covers the picture — only ever over a stopped player.
        case always
        /// Full-screen while the attached picture is not presenting, an
        /// in-chrome indicator while it is.
        case whileNotPresenting
    }

    enum Retirement: String, Equatable, Sendable {
        case presenting
        case presentingAfterRaise = "presenting_after_raise"
        case presentingContinuousMs = "presenting_continuous_ms"
        case intentSettled = "intent_settled"
        case intentSuperseded = "intent_superseded"
        case attachedRetired = "attached_retired"
        case ownerSuccess = "owner_success"
        case playbackNotRequested = "playback_not_requested"
        case systemResumed = "system_resumed"
        case timer
        case user
    }

    struct ClassRule: Equatable, Sendable {
        let severity: Severity
        let blocking: Blocking
        /// How long the fault must have lasted before it is DRAWN. It exists
        /// from the moment it is raised either way.
        let minMs: Int?
        let timedMs: Int?
        let timerPausedWhileActions: Bool
        let requiresPlayerStopped: Bool
        let retiredBy: [Retirement]
        let title: String?
        let defaultActions: [PlaybackFault.Action]

        init(
            severity: Severity,
            blocking: Blocking = .never,
            minMs: Int? = nil,
            timedMs: Int? = nil,
            timerPausedWhileActions: Bool = false,
            requiresPlayerStopped: Bool = false,
            retiredBy: [Retirement],
            title: String? = nil,
            defaultActions: [PlaybackFault.Action] = []
        ) {
            self.severity = severity
            self.blocking = blocking
            self.minMs = minMs
            self.timedMs = timedMs
            self.timerPausedWhileActions = timerPausedWhileActions
            self.requiresPlayerStopped = requiresPlayerStopped
            self.retiredBy = retiredBy
            self.title = title
            self.defaultActions = defaultActions
        }
    }

    struct SourceRow: Equatable, Sendable {
        let id: String
        /// `nil` is the fixture's `"any"`: every context matches.
        let context: Context?
        /// `nil` is a log-only row: it produces no surface.
        let cls: PlaybackFault.Class?
        let codes: [String]
        let actions: [PlaybackFault.Action]
        let requiresPlayerStopped: Bool
        let thenWhenStopped: PlaybackFault.Class?
        let carries: [String]
        let retryable: Bool
        /// A source may narrow its class's retirement rules. System suspension
        /// is a hold notice, but unlike a server hold it must not time out.
        let retiredBy: [Retirement]?

        init(
            _ id: String,
            _ context: Context?,
            _ cls: PlaybackFault.Class?,
            codes: [String] = [],
            actions: [PlaybackFault.Action] = [],
            requiresPlayerStopped: Bool = false,
            thenWhenStopped: PlaybackFault.Class? = nil,
            carries: [String] = [],
            retryable: Bool = false,
            retiredBy: [Retirement]? = nil
        ) {
            self.id = id
            self.context = context
            self.cls = cls
            self.codes = codes
            self.actions = actions
            self.requiresPlayerStopped = requiresPlayerStopped
            self.thenWhenStopped = thenWhenStopped
            self.carries = carries
            self.retryable = retryable
            self.retiredBy = retiredBy
        }
    }

    struct Timings: Equatable, Sendable {
        let bufferingMinMs: Int
        let holdNoticeMs: Int
        let degradedNoticeMs: Int
        let refusedProgressMs: Int
        let disagreementNoticeMs: Int
    }

    static let classes: [PlaybackFault.Class: ClassRule] = [
        .preparing: ClassRule(
            severity: .progress,
            blocking: .whileNotPresenting,
            retiredBy: [.presenting, .intentSettled, .attachedRetired]
        ),
        .buffering: ClassRule(
            severity: .progress,
            blocking: .whileNotPresenting,
            minMs: 350,
            // `playback_not_requested`, and on no other class. A `buffering`
            // fault is about a player that WANTS media: the wait is only a wait
            // while something is trying to play. Presentation evidence is the
            // only other thing that retires it and a paused picture never
            // produces another sample, so without this a buffer that filled
            // while the viewer had paused left a spinner over a still frame
            // until the generation changed — the overlay outliving the thing it
            // described, which is the defect this contract exists to kill.
            // Ruled 2026-09-13; found independently by this session and the
            // Android one, and true of the web too.
            retiredBy: [.presenting, .playbackNotRequested, .attachedRetired]
        ),
        .recovering: ClassRule(
            severity: .progress,
            blocking: .whileNotPresenting,
            retiredBy: [.presentingAfterRaise, .ownerSuccess, .attachedRetired]
        ),
        .hold: ClassRule(
            severity: .notice,
            timedMs: 30_000,
            // `presenting_after_raise`, not `presenting`: a server hold over a
            // picture that never stopped has not been answered by that picture,
            // so it expires on its own timer instead.
            retiredBy: [.timer, .presentingAfterRaise]
        ),
        .degraded: ClassRule(
            severity: .notice,
            timedMs: 5_000,
            timerPausedWhileActions: true,
            retiredBy: [.timer, .presentingContinuousMs]
        ),
        .refused: ClassRule(
            severity: .notice,
            retiredBy: [.intentSuperseded, .presentingContinuousMs],
            defaultActions: [.retry]
        ),
        .exhausted: ClassRule(
            severity: .prompt,
            blocking: .always,
            requiresPlayerStopped: true,
            retiredBy: [.user],
            title: "Playback is stalled.",
            defaultActions: [.keepWaiting, .retry, .close]
        ),
        .stopped: ClassRule(
            severity: .terminal,
            blocking: .always,
            requiresPlayerStopped: true,
            retiredBy: [.user],
            defaultActions: [.retry, .close]
        ),
    ]

    /// Rows are scanned IN ORDER and the first whose id and context both match
    /// wins. Order is the contract; do not sort this array.
    static let sources: [SourceRow] = [
        SourceRow("owner_stopped", nil, .stopped, requiresPlayerStopped: true),
        SourceRow("owner_exhausted", nil, .exhausted, requiresPlayerStopped: true),
        SourceRow(
            "startup_exhausted",
            nil,
            .exhausted,
            actions: [.close, .retry],
            requiresPlayerStopped: true
        ),
        SourceRow("hls_init_invalid", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("hls_init_unsupported", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("auth_401_403", nil, .stopped, actions: [.signIn, .close], requiresPlayerStopped: true),
        SourceRow("vod_source_rescan_required", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("vod_source_unsupported", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("vod_transcode_unavailable", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("vod_subtitle_burn_unavailable", .start, .stopped, requiresPlayerStopped: true),
        SourceRow("vod_disabled", .start, .stopped, requiresPlayerStopped: true),
        SourceRow(
            "create_503_not_yet",
            .start,
            .preparing,
            codes: ["startup_timeout", "media_owner_transition", "vod_index_pending", "vod_engine_unattested"],
            retryable: true
        ),
        // The commonest `preparing` surface there is: a staged start with no
        // refusal behind it. Without this row a cold start had to borrow
        // `owner_recovery_step` and tell the ledger a first play was a recovery.
        SourceRow("client_preparing", nil, .preparing),
        SourceRow("change_failed", .change, .refused, actions: [.retry]),
        // The 503 codes a playlist or segment request can actually come back
        // with. On those two resources a 503 is only ever a "not yet" — the
        // terminal answers are 404/410/502 — and matching on the status alone
        // made `vod_disabled` a recovery. Apple reads media refusals as a
        // status and nothing else (`mediaFailureSurfaceSource`), so this list
        // is carried for the shared table's sake rather than consulted here.
        SourceRow(
            "segment_503_not_yet",
            .attached,
            .recovering,
            codes: [
                "startup_timeout",
                "playlist_state_changed",
                "segment_pending",
                "segment_wait_busy",
                "node_wait_capacity",
                "media_owner_transition",
                "vod_resurrection_unavailable",
                "response_owner_transition",
                "response_state_changed",
                "response_owner_reclassification_unavailable",
                "response_publication_timeout",
                "response_completion_capacity",
                "response_snapshot_capacity",
                "node_maintenance",
                "node_removal_fenced",
                "learner_route_ineligible",
            ]
        ),
        // `any`, not `attached`: a session handed to a node that is already
        // gone answers 410 on the create with the same body, and this is the one
        // row that carries `film_position_ms`. First-match still puts
        // `change_failed` above it, so a 410 during a pending change stays a
        // refusal about the destination rather than a verdict on the predecessor.
        SourceRow("media_owner_lost_410", nil, .recovering, thenWhenStopped: .stopped, carries: ["position_ms"]),
        SourceRow("control_hold", .attached, .hold),
        SourceRow("system_interruption", .attached, .hold, retiredBy: [.systemResumed]),
        SourceRow("media_waiting", .attached, .buffering),
        SourceRow("owner_recovery_step", nil, .recovering),
        SourceRow("readiness_deadline_rungs_left", nil, .recovering),
        SourceRow("decoder_failed", nil, .stopped, requiresPlayerStopped: true),
        SourceRow("black_frame_ladder_spent", .start, .exhausted, actions: [.close, .retry], requiresPlayerStopped: true),
        SourceRow("repeated_early_end", .attached, .stopped, requiresPlayerStopped: true),
        SourceRow("degraded_notice", nil, .degraded),
        SourceRow("log_only", nil, nil),
    ]

    static let timings = Timings(
        bufferingMinMs: 350,
        holdNoticeMs: 30_000,
        degradedNoticeMs: 5_000,
        refusedProgressMs: 10_000,
        disagreementNoticeMs: 30_000
    )

    static let severityRank: [Severity: Int] = [.notice: 1, .progress: 2, .prompt: 3, .terminal: 4]

    static let actions: [PlaybackFault.Action] = [.keepWaiting, .retry, .close, .forceTranscode, .signIn]

    /// Which timing bounds each class's "this much CONTINUOUS presentation"
    /// rule. `refused` retires when the picture has been fine long enough that
    /// the viewer has been told; the banner a demotion leaves behind retires
    /// when its offer has gone stale.
    static let continuousTiming: [PlaybackFault.Class: Int] = [
        .refused: timings.refusedProgressMs,
        .degraded: timings.disagreementNoticeMs,
    ]

    /// Only the owner's own stop promotes a fault that declared a successor
    /// class. Any other blocking source is its own fault with its own sentence
    /// and its own actions — an `auth_401_403` folded into a 410 loses its
    /// Sign in, which is the misattribution the ledger exists to prevent.
    static let promotingSources: [String] = ["owner_stopped", "owner_exhausted"]

    static let blockingClasses: [PlaybackFault.Class] =
        PlaybackFault.Class.allCases.filter { classes[$0]?.blocking == .always }

    static func rule(for cls: PlaybackFault.Class) -> ClassRule {
        // Every case of `PlaybackFault.Class` has a row above, and the fixture
        // test fails if one is ever missing. The fallback exists so no call
        // site has to unwrap a table that is total by construction.
        classes[cls] ?? ClassRule(severity: .notice, retiredBy: [])
    }

    /// The first row whose id and context both match, or the error the fixture
    /// names for a miss.
    static func row(
        source: String,
        context: Context
    ) -> Result<SourceRow, PlaybackSurfaceLog.Failure> {
        var sawId = false
        for row in sources where row.id == source {
            sawId = true
            if row.context == nil || row.context == context { return .success(row) }
        }
        return .failure(sawId ? .sourceContextMismatch : .unknownSource)
    }
}

/// The presenter. Pure: `apply` is the whole of it, and the log entries it
/// returns are its only output besides `surface`.
struct PlaybackSurfaceModel: Equatable, Sendable {
    /// Supplied by the adapter: `start` before first presentation on this
    /// playback, `change` while a viewer-requested replacement is pending and
    /// a predecessor is attached, `attached` otherwise.
    enum Context: String, Sendable, CaseIterable {
        case start, attached, change
    }

    enum Event: Sendable {
        case attach(Int)
        case retire(Int)
        case presenting(Bool, attached: Int)
        /// A platform event that is not evidence: `canplay`, `playing`,
        /// `isPlayingChanged`, `timeControlStatus`. It prompts a look and
        /// never moves a surface — not even by letting a due timer run.
        case inert(String)
        case raise(PlaybackFault, context: Context)
        case intentSettled(Int)
        case intentSuperseded(Int)
        case ownerSuccess(Int)
        /// The viewer's transport intent. `false` retires every fault whose
        /// class names `playback_not_requested`; `true` does nothing, because
        /// the raise sites decide what comes back — if the player is still
        /// waiting when it resumes, the next sample raises the wait again and
        /// THAT is the raise.
        case playbackRequested(Bool)
        /// System suspension is distinct from viewer transport intent. It
        /// raises one indefinite hold and clears only when the system ends it.
        case systemPaused(Bool)
        case userAction(PlaybackFault.Action)
        case hidden(Bool)
        case tick
    }

    private(set) var faults: [PlaybackFault] = []
    private(set) var attached: Int?
    private(set) var hidden = false
    private(set) var presenting = false
    private(set) var presentingSince: ContinuousClock.Instant?
    private(set) var surface = PlaybackSurface.empty

    private var hiddenSince: ContinuousClock.Instant?
    private var seq = 0

    init() {}

    /// What is drawn right now, and the fault behind it.
    var kind: PlaybackSurface.Kind { surface.kind }
    var currentFault: PlaybackFault? { surface.fault }

    /// The player input contract's `failed` state, read through the surface so
    /// there is one definition of it (contract §4).
    var entersFailedRouting: Bool { surface.entersFailedRouting }

    /// Build the event for a fault the adapter is raising. The source table
    /// decides the class, so the adapter never has to.
    /// The raise carries no timestamp: `apply` stamps the fault with the
    /// `now` it is applied at, which is the only clock the reducer has.
    static func raise(
        source: String,
        context: Context,
        attached: Int,
        intent: Int? = nil,
        playerStopped: Bool = false,
        actions: [PlaybackFault.Action] = [],
        positionMs: Int? = nil,
        title: String? = nil,
        detail: String? = nil
    ) -> Event {
        // A proposed class is only a placeholder: `apply` resolves the row and
        // overwrites it, so the table and the call site cannot disagree.
        let proposed = (try? PlaybackSurfaceContract.row(source: source, context: context).get())?.cls
        let fault = PlaybackFault(
            cls: proposed ?? .degraded,
            source: source,
            attached: attached,
            intent: intent,
            // Overwritten by `applyRaise` with the instant it is applied at.
            raisedAt: ContinuousClock.now,
            positionMs: positionMs,
            title: title,
            detail: detail,
            actions: actions,
            playerStopped: playerStopped
        )
        return .raise(fault, context: context)
    }

    /// Pure. Returns the log entries the caller must emit (client log + ledger
    /// ring). Nothing here touches the player.
    @discardableResult
    mutating func apply(_ event: Event, now: ContinuousClock.Instant) -> [PlaybackSurfaceLog] {
        var log: [PlaybackSurfaceLog] = []

        if case .inert = event {
            // Inert by contract. The surface is recomputed because time has
            // moved — a `buffering` fault past its debounce becomes drawable —
            // but no fault is raised, retired or expired here.
            surface = surfaceFrom(now: now)
            return log
        }

        switch event {
        case .inert:
            break

        case .attach(let generation):
            drop(&log, by: .attachedRetired) { $0.attached != generation }
            attached = generation
            presenting = false
            presentingSince = nil

        case .retire(let generation):
            drop(&log, by: .attachedRetired) { $0.attached == generation }
            if attached == generation {
                attached = nil
                presenting = false
                presentingSince = nil
            }

        case .hidden(let isHidden):
            if isHidden, !hidden {
                hiddenSince = now
            } else if !isHidden, hidden {
                // Carry the hidden interval forward rather than letting wall
                // time run under a frozen surface: a 30 s hold the viewer
                // backgrounded for a minute has not been on screen for 30 s.
                // Nothing sampled the picture while the app was away, so the
                // continuous-presenting clock restarts.
                if let hiddenSince, hiddenSince < now {
                    let away = hiddenSince.duration(to: now)
                    for index in faults.indices {
                        faults[index].raisedAt = faults[index].raisedAt.advanced(by: away)
                    }
                }
                hiddenSince = nil
                presenting = false
                presentingSince = nil
            }
            hidden = isHidden

        case .presenting(let isPresenting, let generation):
            if !hidden, generation == attached {
                if isPresenting {
                    if !presenting {
                        presenting = true
                        presentingSince = now
                    }
                    resolveDisagreement(&log, generation: generation, now: now)
                } else {
                    presenting = false
                    presentingSince = nil
                }
            }

        case .raise(let proposed, let context):
            applyRaise(proposed, context: context, now: now, log: &log)

        case .intentSettled(let intent):
            drop(&log, by: .intentSettled) { $0.intent == intent && Self.retires($0, by: .intentSettled) }

        case .intentSuperseded(let intent):
            drop(&log, by: .intentSuperseded) { $0.intent == intent && Self.retires($0, by: .intentSuperseded) }

        case .ownerSuccess:
            // One recovery owner per player: its success retires every
            // `recovering` fault, not only the ones about the generation it
            // replaced.
            drop(&log, by: .ownerSuccess) { Self.retires($0, by: .ownerSuccess) }

        case .playbackRequested(let requested):
            // Only classes that NAME the reason, which today is `buffering`
            // alone: a `preparing` start has not been paused by a viewer who
            // has not seen it yet, and a blocking prompt is answered by the
            // viewer rather than by a transport change — a pause under one must
            // not clear the only thing offering a way out.
            guard !requested else { break }
            drop(&log, by: .playbackNotRequested) {
                Self.retires($0, by: .playbackNotRequested)
            }

        case .systemPaused(let paused):
            if paused, let attached {
                let event = Self.raise(
                    source: "system_interruption",
                    context: .attached,
                    attached: attached,
                    title: "Paused — call in progress",
                    detail: "Paused — audio interrupted"
                )
                if case .raise(let fault, let context) = event {
                    applyRaise(fault, context: context, now: now, log: &log)
                }
            } else if !paused {
                drop(&log, by: .systemResumed) {
                    $0.source == "system_interruption"
                        && Self.retires($0, by: .systemResumed)
                }
            }

        case .userAction(let action):
            let current = surfaceFrom(now: now).fault
            var target: Int? = nil
            if let current, current.actions.contains(action) {
                target = faults.firstIndex { $0.seq == current.seq }
            }
            if target == nil {
                target = faults.firstIndex { $0.actions.contains(action) }
            }
            if let target {
                let seq = faults[target].seq
                drop(&log, by: .user, action: action) { $0.seq == seq }
            }

        case .tick:
            break
        }

        sweep(&log, now: now)
        surface = surfaceFrom(now: now)
        return log
    }

    // MARK: - Raising

    private mutating func applyRaise(
        _ proposed: PlaybackFault,
        context: Context,
        now: ContinuousClock.Instant,
        log: inout [PlaybackSurfaceLog]
    ) {
        let found = PlaybackSurfaceContract.row(source: proposed.source, context: context)
        let row: PlaybackSurfaceContract.SourceRow
        switch found {
        case .failure(let error):
            log.append(PlaybackSurfaceLog(
                event: .error,
                source: proposed.source,
                error: error,
                context: context
            ))
            return
        case .success(let matched):
            row = matched
        }

        guard let cls = row.cls else {
            log.append(PlaybackSurfaceLog(
                event: .logOnly,
                source: row.id,
                detail: proposed.detail,
                attached: proposed.attached
            ))
            return
        }

        let rule = PlaybackSurfaceContract.rule(for: cls)
        let playerStopped = proposed.playerStopped
        guard rule.blocking != .always || playerStopped else {
            // The whole point of the contract: a blocking class raised over a
            // player nobody stopped is a fixture error, not a surface.
            log.append(PlaybackSurfaceLog(
                event: .error,
                cls: cls,
                source: row.id,
                attached: proposed.attached,
                error: .blockingWithoutStop
            ))
            return
        }

        let attachedGeneration = proposed.attached
        let resolvedActions: [PlaybackFault.Action]
        if !proposed.actions.isEmpty {
            resolvedActions = proposed.actions
        } else if !row.actions.isEmpty {
            resolvedActions = row.actions
        } else {
            resolvedActions = rule.defaultActions
        }

        var promoted: Int? = nil
        if PlaybackSurfaceContract.promotingSources.contains(row.id) {
            promoted = faults.firstIndex {
                $0.attached == attachedGeneration && $0.thenWhenStopped == cls && !$0.demoted
            }
        }

        if let index = promoted {
            faults[index].cls = cls
            faults[index].playerStopped = playerStopped
            faults[index].raisedAt = now
            faults[index].thenWhenStopped = nil
            if let title = proposed.title { faults[index].title = title }
            if let detail = proposed.detail { faults[index].detail = detail }
            if !proposed.actions.isEmpty {
                faults[index].actions = proposed.actions
            } else if faults[index].actions.isEmpty {
                faults[index].actions = resolvedActions
            }
            log.append(PlaybackSurfaceLog(.raised, faults[index], by: .ownerStopped))
        } else {
            seq += 1
            var fault = PlaybackFault(
                cls: cls,
                source: row.id,
                attached: attachedGeneration,
                intent: proposed.intent,
                raisedAt: now,
                positionMs: proposed.positionMs,
                title: proposed.title ?? rule.title,
                detail: proposed.detail,
                actions: resolvedActions,
                playerStopped: playerStopped
            )
            fault.thenWhenStopped = row.thenWhenStopped
            fault.seq = seq
            faults.append(fault)
            log.append(PlaybackSurfaceLog(.raised, fault))
        }

        // A blocking fault raised over a picture that is presenting is a
        // disagreement the moment it is raised, not whenever the next evidence
        // sample happens to arrive.
        if rule.blocking == .always, presenting, attached == attachedGeneration {
            resolveDisagreement(&log, generation: attachedGeneration, now: now)
        }
    }

    // MARK: - Retirement

    private static func retires(_ fault: PlaybackFault, by reason: PlaybackSurfaceContract.Retirement) -> Bool {
        // Each retirement reason is a property of the CLASS, not of the event
        // that carries it: a prompt the viewer has to answer is not swept away
        // because a seek happened to land underneath it.
        let sourceRetirement = PlaybackSurfaceContract.sources
            .first(where: { $0.id == fault.source })?
            .retiredBy
        return (sourceRetirement ?? PlaybackSurfaceContract.rule(for: fault.cls).retiredBy)
            .contains(reason)
    }

    private mutating func drop(
        _ log: inout [PlaybackSurfaceLog],
        by reason: PlaybackSurfaceLog.Reason,
        action: PlaybackFault.Action? = nil,
        where predicate: (PlaybackFault) -> Bool
    ) {
        var kept: [PlaybackFault] = []
        kept.reserveCapacity(faults.count)
        for fault in faults {
            if predicate(fault) {
                log.append(PlaybackSurfaceLog(.cleared, fault, by: reason, action: action))
            } else {
                kept.append(fault)
            }
        }
        faults = kept
    }

    /// `presenting_after_raise` ONLY. The run of presentation has to have
    /// begun at or after the fault was raised: the picture that was already on
    /// screen when the server said "held" is not proof the hold is over, and an
    /// owner that reopens in place produces exactly this, which is why
    /// `recovering` does not need a new generation to be retired.
    ///
    /// Plain `presenting` is NOT gated by it, and gating it was M0's bug: a
    /// picture that is presenting is not buffering and is not preparing,
    /// whenever its run began. Safari fires `waiting` at every fMP4 boundary on
    /// healthy 4K, so a `media_waiting` raised over a picture that never
    /// stopped had no evidence that could ever postdate it and the spinner
    /// stayed up for the rest of the film.
    private func evidencePostdates(_ fault: PlaybackFault) -> Bool {
        guard let presentingSince else { return false }
        return presentingSince >= fault.raisedAt
    }

    /// Continuous means continuous: a picture that is not presenting right now
    /// has a run length of nothing, whatever it did earlier.
    private func continuousElapsed(_ fault: PlaybackFault, now: ContinuousClock.Instant) -> Duration? {
        guard presenting, let presentingSince else { return nil }
        let from = max(presentingSince, fault.raisedAt)
        guard from <= now else { return .zero }
        return from.duration(to: now)
    }

    private mutating func sweep(_ log: inout [PlaybackSurfaceLog], now: ContinuousClock.Instant) {
        // A hidden app samples nothing and expires nothing. The clock it is
        // measured against is rewound when it comes back (see `.hidden`).
        guard !hidden else { return }
        var expired: Set<Int> = []
        var reasons: [Int: PlaybackSurfaceLog.Reason] = [:]
        for fault in faults {
            let rule = PlaybackSurfaceContract.rule(for: fault.cls)
            let retiredBy = PlaybackSurfaceContract.sources
                .first(where: { $0.id == fault.source })?
                .retiredBy ?? rule.retiredBy
            if let timedMs = rule.timedMs,
               retiredBy.contains(.timer),
               !(rule.timerPausedWhileActions && !fault.actions.isEmpty),
               fault.raisedAt <= now,
               fault.raisedAt.duration(to: now) >= .milliseconds(timedMs) {
                expired.insert(fault.seq)
                reasons[fault.seq] = .timer
                continue
            }
            if retiredBy.contains(.presentingContinuousMs),
               let bound = PlaybackSurfaceContract.continuousTiming[fault.cls],
               let elapsed = continuousElapsed(fault, now: now),
               elapsed >= .milliseconds(bound) {
                expired.insert(fault.seq)
                reasons[fault.seq] = .presenting
                continue
            }
            // Evidence never retires a fault about a pending destination.
            if fault.intent != nil { continue }
            guard presenting else { continue }
            if retiredBy.contains(.presenting) {
                expired.insert(fault.seq)
                reasons[fault.seq] = .presenting
                continue
            }
            if retiredBy.contains(.presentingAfterRaise), evidencePostdates(fault) {
                expired.insert(fault.seq)
                reasons[fault.seq] = .presenting
            }
        }
        guard !expired.isEmpty else { return }
        var kept: [PlaybackFault] = []
        kept.reserveCapacity(faults.count)
        for fault in faults {
            if expired.contains(fault.seq), let reason = reasons[fault.seq] {
                log.append(PlaybackSurfaceLog(.cleared, fault, by: reason))
            } else {
                kept.append(fault)
            }
        }
        faults = kept
    }

    /// The agreement rule (§3.2): a blocking surface over a moving picture is
    /// a disagreement, and the picture wins. The fault keeps its actions and
    /// its data — a `media_owner_lost` still carries its position and its Try
    /// again — so when the buffer drains the viewer gets the specific
    /// recovery. Nothing is re-paused and no timer is cancelled: the owner's
    /// detectors are running because the owner never stopped them.
    private mutating func resolveDisagreement(
        _ log: inout [PlaybackSurfaceLog],
        generation: Int,
        now: ContinuousClock.Instant
    ) {
        for index in faults.indices {
            guard PlaybackSurfaceContract.blockingClasses.contains(faults[index].cls) else { continue }
            guard faults[index].attached == generation else { continue }
            log.append(PlaybackSurfaceLog(.disagreement, faults[index], by: .presenting))
            faults[index].cls = .degraded
            faults[index].markDemoted()
            faults[index].title = Self.recoveredTitle
            faults[index].raisedAt = now
        }
    }

    static let recoveredTitle = "Playback recovered"

    // MARK: - Projection

    private func drawable(_ fault: PlaybackFault, now: ContinuousClock.Instant) -> Bool {
        guard let minMs = PlaybackSurfaceContract.rule(for: fault.cls).minMs else { return true }
        guard fault.raisedAt <= now else { return false }
        return fault.raisedAt.duration(to: now) >= .milliseconds(minMs)
    }

    private func kind(for fault: PlaybackFault) -> PlaybackSurface.Kind {
        switch PlaybackSurfaceContract.rule(for: fault.cls).blocking {
        case .always: return .blocking
        case .whileNotPresenting: return presenting ? .indicator : .blocking
        case .never: return .banner
        }
    }

    private func surfaceFrom(now: ContinuousClock.Instant) -> PlaybackSurface {
        var chosen: PlaybackFault? = nil
        var chosenRank = -1
        for fault in faults {
            guard drawable(fault, now: now) else { continue }
            let rule = PlaybackSurfaceContract.rule(for: fault.cls)
            let rank = PlaybackSurfaceContract.severityRank[rule.severity] ?? 0
            // Highest severity owns the surface; equal severity breaks by
            // recency, newest first.
            if chosen == nil || rank > chosenRank || (rank == chosenRank && fault.seq > (chosen?.seq ?? -1)) {
                chosen = fault
                chosenRank = rank
            }
        }
        guard let chosen else { return .empty }
        return PlaybackSurface(kind: kind(for: chosen), fault: chosen)
    }
}

/// The Playback debug ledger's SURFACE history: the last sixteen faults, with
/// what cleared them and what the player was doing when each was raised.
///
/// Not part of the reducer — it is fed from the reducer's log entries by the
/// caller, which is the only party that knows the session id, the attempt and
/// the player's rate (§5).
struct PlaybackSurfaceHistory: Equatable, Sendable {
    struct Entry: Equatable, Sendable {
        let cls: PlaybackFault.Class
        let source: String
        let attached: Int?
        let intent: Int?
        let sessionId: String?
        let attempt: String?
        let raisedAtMs: Int
        let rate: Double
        let positionMs: Int
        let presenting: Bool
        let stoppedByOwner: Bool
        var clearedAtMs: Int?
        var clearedBy: PlaybackSurfaceLog.Reason?
        var disagreed: Bool = false
    }

    static let capacity = 16

    private(set) var entries: [Entry] = []

    /// Snapshot of what the player was doing, taken by the caller.
    struct PlayerSnapshot: Equatable, Sendable {
        let rate: Double
        let positionMs: Int
        let presenting: Bool
        let sessionId: String?
        let attempt: String?

        init(rate: Double, positionMs: Int, presenting: Bool, sessionId: String?, attempt: String?) {
            self.rate = rate
            self.positionMs = positionMs
            self.presenting = presenting
            self.sessionId = sessionId
            self.attempt = attempt
        }
    }

    mutating func record(_ entry: PlaybackSurfaceLog, atMs: Int, player: PlayerSnapshot) {
        switch entry.event {
        case .raised:
            guard let cls = entry.cls, let source = entry.source else { return }
            // A promotion re-raises a fault that is already in the ring; move
            // its class rather than opening a second row for one fault.
            if let index = openIndex(source: source, attached: entry.attached) {
                entries[index] = Entry(
                    cls: cls,
                    source: source,
                    attached: entry.attached,
                    intent: entry.intent,
                    sessionId: entries[index].sessionId,
                    attempt: entries[index].attempt,
                    raisedAtMs: entries[index].raisedAtMs,
                    rate: entries[index].rate,
                    positionMs: entries[index].positionMs,
                    presenting: entries[index].presenting,
                    stoppedByOwner: entry.playerStopped,
                    clearedAtMs: nil,
                    clearedBy: nil,
                    disagreed: entries[index].disagreed
                )
                return
            }
            entries.append(Entry(
                cls: cls,
                source: source,
                attached: entry.attached,
                intent: entry.intent,
                sessionId: player.sessionId,
                attempt: player.attempt,
                raisedAtMs: atMs,
                rate: player.rate,
                positionMs: player.positionMs,
                presenting: player.presenting,
                stoppedByOwner: entry.playerStopped,
                clearedAtMs: nil,
                clearedBy: nil
            ))
            if entries.count > Self.capacity { entries.removeFirst(entries.count - Self.capacity) }
        case .cleared:
            guard let source = entry.source,
                  let index = openIndex(source: source, attached: entry.attached) else { return }
            entries[index].clearedAtMs = atMs
            entries[index].clearedBy = entry.by
        case .disagreement:
            guard let source = entry.source,
                  let index = openIndex(source: source, attached: entry.attached) else { return }
            entries[index].disagreed = true
            entries[index].clearedBy = nil
        case .logOnly, .error:
            return
        }
    }

    private func openIndex(source: String, attached: Int?) -> Int? {
        entries.lastIndex { $0.source == source && $0.attached == attached && $0.clearedAtMs == nil }
    }

    /// One line per fault, newest last, for the ledger's History row.
    var ledgerSummary: String {
        entries.map { entry in
            let cleared = entry.clearedAtMs.map { cleared in
                "→ \(cleared) ms (\(entry.clearedBy?.rawValue ?? "unknown"))"
            } ?? "→ open"
            let demoted = entry.disagreed ? " · demoted" : ""
            let rate = String(format: "%.1f", entry.rate)
            return "\(entry.cls.rawValue) · \(entry.source) · \(entry.raisedAtMs) ms \(cleared)"
                + "\(demoted) · rate \(rate) · \(entry.positionMs) ms"
                + " · \(entry.presenting ? "presenting" : "not presenting")"
                + " · \(entry.stoppedByOwner ? "owner stopped" : "owner running")"
        }.joined(separator: "\n")
    }
}
