/// Pure transcription of the `live` section of
/// `tests/playback/player-input-contract.json`. Platform events never enter
/// this file; the views map them to the enums below.
///
/// Live television is the finite player's contract with everything that
/// assumes a timeline removed and one thing added. There is nothing to seek or
/// scrub; guide, menu, programme-detail and Info panels are explicit states. This is
/// a sibling of `PlayerInputRouting` rather than more cases inside it — a live
/// stream can never enter timeline or scrub states.

enum LiveTvInputSurface: String, CaseIterable {
    case tenFoot = "ten-foot"
    case desktop
    case touch
}

enum LiveTvInputState: String, CaseIterable {
    case browser
    case fullscreenHidden = "fullscreen_hidden"
    case fullscreenControls = "fullscreen_controls"
    case temporaryGuide = "temporary_guide"
    case menu
    case programmeDetails = "programme_details"
    case streamInfo = "stream_info"
}

enum LiveTvContractInput: String, CaseIterable {
    case left
    case right
    case up
    case down
    case select
    case back
    case playPause = "play_pause"
    case tapSurface = "tap_surface"
    case idle
}

enum LiveTvInputOutcome: String, CaseIterable {
    case reveal
    case hide
    case delegate
    case focusControl = "focus_control"
    case focusCell = "focus_cell"
    case focusPanel = "focus_panel"
    case activate
    case closePanel = "close_panel"
    case returnBrowser = "return_browser"
    case stripPrev = "strip_prev"
    case stripNext = "strip_next"
    case tune
    case channelUp = "channel_up"
    case channelDown = "channel_down"
    case togglePlay = "toggle_play"
    case toggleChrome = "toggle_chrome"
    case exit
    case ignore
}

enum LiveTvInputRouting {
    /// The overlay hides this long after the last input, and only while
    /// playing — the finite player's numbers unchanged.
    static let overlayAutoHideNanoseconds: UInt64 = 4_000_000_000
    /// A held channel key accumulates and starts ONE session after this much
    /// quiet. Ten presses must not be ten tuner GETs.
    static let channelCoalesceMilliseconds: Int = 350

    /// When the guide answers `unavailable` and says nothing about when it
    /// comes back, ask again this often. Short, because an owner with nothing
    /// to serve is usually an owner about to have something.
    static let guidePollUnavailableSeconds: Int = 30

    /// Never poll the guide faster than this, whatever `next_refresh_at` says.
    /// A floor on fan-out, not a cadence.
    static let guidePollMinSeconds: Int = 15

    /// Poll this long after the owner's `next_refresh_at`, so the client asks
    /// once the answer exists rather than just before it does.
    static let guidePollAfterNextRefreshSeconds: Int = 5

    /// Never sleep longer than this between guide reads, whatever the document
    /// claims — the far end of the same clamp `guidePollMinSeconds` holds.
    /// Transcribed like its siblings: the contract's `guide_poll_ceiling_s`.
    static let guidePollCeilingSeconds: Int = 1200

    /// How long a pressing *web* document waits for a sibling tab to claim a
    /// start hint before treating it as an orphan. Transcribed because the
    /// contract pins all of `live.timings` in every client; Apple has one
    /// process and one hint file, so there is no sibling to probe — see
    /// `LiveTvLease.retireOrphanedHint`.
    static let retireLivenessProbeMilliseconds: Int = 250

    /// A hint touched more recently than this many five-second keepalives is
    /// held by something alive. Web mechanics, for the same reason as
    /// `retireLivenessProbeMilliseconds`.
    static let retireOrphanAfterKeepalives: Int = 3

    /// How many times the lease re-sends a start that got no answer, with the
    /// same request id. One — the owner joins the replay to the same session.
    static let startReplayAttempts: Int = 1

    static func route(
        surface: LiveTvInputSurface,
        state: LiveTvInputState,
        input: LiveTvContractInput
    ) -> LiveTvInputOutcome {
        switch surface {
        case .tenFoot: tenFoot(state: state, input: input)
        case .desktop: desktop(state: state, input: input)
        case .touch: touch(state: state, input: input)
        }
    }

    private static func tenFoot(state: LiveTvInputState, input: LiveTvContractInput) -> LiveTvInputOutcome {
        switch state {
        case .fullscreenHidden:
            // The 2026-09-02 ruling, unchanged: a direction on a hidden
            // overlay only reveals it. Nothing here changes channel.
            switch input {
            case .left, .right, .up, .down, .select, .tapSurface: .reveal
            case .back: .returnBrowser
            case .playPause: .togglePlay
            case .idle: .ignore
            }
        case .fullscreenControls:
            // Preview-then-commit: move focus, Select tunes, Back hides.
            switch input {
            case .left, .right, .up, .down: .focusControl
            case .select: .activate
            case .back, .idle: .hide
            case .playPause: .togglePlay
            case .tapSurface: .ignore
            }
        case .browser:
            switch input {
            case .left, .right, .up, .down, .select, .tapSurface: .delegate
            case .back: .exit
            case .playPause, .idle: .ignore
            }
        case .temporaryGuide:
            switch input {
            case .left, .right, .up, .down: .focusCell
            case .select: .activate
            case .back: .closePanel
            case .playPause: .togglePlay
            case .tapSurface, .idle: .ignore
            }
        case .menu, .programmeDetails, .streamInfo:
            switch input {
            case .left, .right, .up, .down: .focusPanel
            case .select: .activate
            case .back: .closePanel
            case .playPause: .togglePlay
            case .tapSurface, .idle: .ignore
            }
        }
    }

    private static func desktop(state: LiveTvInputState, input: LiveTvContractInput) -> LiveTvInputOutcome {
        switch state {
        case .fullscreenHidden:
            switch input {
            case .left: .stripPrev
            case .right: .stripNext
            case .up: .channelUp
            case .down: .channelDown
            case .select, .idle: .ignore
            case .back: .exit
            case .playPause: .togglePlay
            case .tapSurface: .reveal
            }
        case .fullscreenControls:
            switch input {
            case .left: .stripPrev
            case .right: .stripNext
            case .up: .channelUp
            case .down: .channelDown
            case .select: .tune
            case .back: .exit
            case .playPause: .togglePlay
            case .tapSurface: .ignore
            case .idle: .hide
            }
        case .browser:
            switch input {
            case .left, .right, .up, .down, .select, .tapSurface: .delegate
            default: .ignore
            }
        case .temporaryGuide:
            switch input {
            case .left, .right, .up, .down: .focusCell
            case .select: .activate
            case .back: .closePanel
            case .playPause: .togglePlay
            case .tapSurface, .idle: .ignore
            }
        case .menu, .programmeDetails, .streamInfo:
            switch input {
            case .left, .right, .up, .down, .select: .delegate
            case .back: .closePanel
            case .playPause: .togglePlay
            case .tapSurface, .idle: .ignore
            }
        }
    }

    private static func touch(state: LiveTvInputState, input: LiveTvContractInput) -> LiveTvInputOutcome {
        switch state {
        case .fullscreenHidden:
            switch input {
            case .tapSurface: .toggleChrome
            case .back: .exit
            default: .ignore
            }
        case .fullscreenControls:
            switch input {
            case .tapSurface: .toggleChrome
            case .select: .activate
            case .back: .exit
            case .idle: .hide
            default: .ignore
            }
        case .browser:
            switch input {
            case .back: .exit
            default: .ignore
            }
        case .temporaryGuide:
            switch input {
            case .select: .activate
            case .back: .closePanel
            default: .ignore
            }
        case .menu, .programmeDetails, .streamInfo:
            switch input {
            case .select: .activate
            case .back, .tapSurface: .closePanel
            default: .ignore
            }
        }
    }
}
