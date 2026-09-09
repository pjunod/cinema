/// Pure transcription of the `live` section of
/// `tests/playback/player-input-contract.json`. Platform events never enter
/// this file; the views map them to the enums below.
///
/// Live television is the finite player's contract with everything that
/// assumes a timeline removed and one thing added. There is nothing to seek,
/// nothing to scrub, no menu and no info panel; there is a channel. So this is
/// a sibling of `PlayerInputRouting` rather than more cases inside it — a live
/// stream can never enter `timeline`, `scrub`, `menu` or `info`, and a reducer
/// that claimed otherwise would be lying in the one place the clients are
/// tested against.

enum LiveTvInputSurface: String, CaseIterable {
    case tenFoot = "ten-foot"
    case desktop
    case touch
}

enum LiveTvInputState: String, CaseIterable {
    case hidden
    case overlay
    case page
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
    case focusRow = "focus_row"
    case activate
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
        case .hidden:
            // The 2026-09-02 ruling, unchanged: a direction on a hidden
            // overlay only reveals it. Nothing here changes channel.
            switch input {
            case .left, .right, .up, .down, .select, .tapSurface: .reveal
            case .back: .exit
            case .playPause: .togglePlay
            case .idle: .ignore
            }
        case .overlay:
            // Preview-then-commit: move focus, Select tunes, Back hides.
            switch input {
            case .left, .right, .up, .down: .focusRow
            case .select: .activate
            case .back, .idle: .hide
            case .playPause: .togglePlay
            case .tapSurface: .ignore
            }
        case .page:
            // A television never shows an inline player; the state exists only
            // so the three surfaces are one shape.
            .ignore
        }
    }

    private static func desktop(state: LiveTvInputState, input: LiveTvContractInput) -> LiveTvInputOutcome {
        switch state {
        case .hidden:
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
        case .overlay:
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
        case .page:
            // Browsing owns the keyboard while the player is inline; a stray
            // arrow must not tune a channel out from under someone scrolling.
            switch input {
            case .playPause: .togglePlay
            default: .ignore
            }
        }
    }

    private static func touch(state: LiveTvInputState, input: LiveTvContractInput) -> LiveTvInputOutcome {
        switch state {
        case .hidden:
            switch input {
            case .tapSurface: .toggleChrome
            case .back: .exit
            default: .ignore
            }
        case .overlay:
            switch input {
            case .tapSurface: .toggleChrome
            case .select: .activate
            case .back: .exit
            case .idle: .hide
            default: .ignore
            }
        case .page:
            .ignore
        }
    }
}
