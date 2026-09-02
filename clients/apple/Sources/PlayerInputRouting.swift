enum PlayerInputSurface: String, CaseIterable {
    case tenFoot = "ten-foot"
    case touch
}

enum PlayerInputState: String, CaseIterable {
    case hidden
    case transport
    case timeline
    case scrub
    case menu
    case info
    case failed
}

enum PlayerContractInput: String, CaseIterable {
    case left
    case right
    case up
    case down
    case select
    case back
    case playPause = "play_pause"
    case skipBack = "skip_back"
    case skipForward = "skip_forward"
    case tapSurface = "tap_surface"
    case idle
}

enum PlayerInputOutcome: String, CaseIterable {
    case reveal
    case focusRow = "focus_row"
    case focusMarkerOrIgnore = "focus_marker_or_ignore"
    case focusTransport = "focus_transport"
    case activate
    case togglePlay = "toggle_play"
    case skip
    case preview
    case commit
    case cancel
    case cancelThenFocusTransport = "cancel_then_focus_transport"
    case cancelThenFocusMarkerOrIgnore = "cancel_then_focus_marker_or_ignore"
    case commitThenTogglePlay = "commit_then_toggle_play"
    case closeMenu = "close_menu"
    case closeInfo = "close_info"
    case menuFocus = "menu_focus"
    case hide
    case exit
    case toggleChrome = "toggle_chrome"
    case ignore
}

/// Pure transcription of `tests/playback/player-input-contract.json`.
/// Platform events never enter this file; adapters map them to the enums above.
enum PlayerInputRouting {
    static func route(
        surface: PlayerInputSurface,
        state: PlayerInputState,
        input: PlayerContractInput
    ) -> PlayerInputOutcome {
        switch surface {
        case .tenFoot: tenFoot(state: state, input: input)
        case .touch: touch(state: state, input: input)
        }
    }

    static func previewStepSeconds(repeatCount: Int) -> Double {
        if repeatCount >= 10 { return 60 }
        if repeatCount >= 5 { return 30 }
        return 10
    }

    private static func tenFoot(
        state: PlayerInputState,
        input: PlayerContractInput
    ) -> PlayerInputOutcome {
        switch state {
        case .hidden:
            switch input {
            case .left, .right, .up, .down, .select, .tapSurface: .reveal
            case .back: .exit
            case .playPause: .togglePlay
            case .skipBack, .skipForward: .skip
            case .idle: .ignore
            }
        case .transport:
            switch input {
            case .left, .right, .up, .down: .focusRow
            case .select: .activate
            case .back: .hide
            case .playPause: .togglePlay
            case .skipBack, .skipForward: .skip
            case .tapSurface: .ignore
            case .idle: .hide
            }
        case .timeline:
            switch input {
            case .left, .right: .preview
            case .up: .focusMarkerOrIgnore
            case .down: .focusTransport
            case .select, .playPause: .togglePlay
            case .back, .idle: .hide
            case .skipBack, .skipForward: .skip
            case .tapSurface: .ignore
            }
        case .scrub:
            switch input {
            case .left, .right: .preview
            case .up: .cancelThenFocusMarkerOrIgnore
            case .down: .cancelThenFocusTransport
            case .select: .commit
            case .back: .cancel
            case .playPause: .commitThenTogglePlay
            case .skipBack, .skipForward, .tapSurface, .idle: .ignore
            }
        case .menu:
            switch input {
            case .left, .right, .up, .down: .menuFocus
            case .select: .activate
            case .back, .tapSurface: .closeMenu
            case .playPause: .togglePlay
            case .skipBack, .skipForward, .idle: .ignore
            }
        case .info:
            switch input {
            case .left, .right, .up, .down: .menuFocus
            case .select: .activate
            case .back, .tapSurface: .closeInfo
            case .playPause: .togglePlay
            case .skipBack, .skipForward, .idle: .ignore
            }
        case .failed:
            switch input {
            case .left, .right: .focusRow
            case .select: .activate
            case .back: .exit
            case .up, .down, .playPause, .skipBack, .skipForward, .tapSurface, .idle: .ignore
            }
        }
    }

    private static func touch(
        state: PlayerInputState,
        input: PlayerContractInput
    ) -> PlayerInputOutcome {
        switch state {
        case .hidden:
            switch input {
            case .back: .exit
            case .playPause: .togglePlay
            case .skipBack, .skipForward: .skip
            case .tapSurface: .toggleChrome
            case .left, .right, .up, .down, .select, .idle: .ignore
            }
        case .transport:
            switch input {
            case .select: .activate
            case .back, .idle: .hide
            case .playPause: .togglePlay
            case .skipBack, .skipForward: .skip
            case .tapSurface: .toggleChrome
            case .left, .right, .up, .down: .ignore
            }
        case .timeline:
            switch input {
            case .back, .idle: .hide
            case .playPause: .togglePlay
            case .skipBack, .skipForward: .skip
            case .tapSurface: .toggleChrome
            case .left, .right, .up, .down, .select: .ignore
            }
        case .scrub:
            switch input {
            case .select: .commit
            case .back: .cancel
            case .playPause: .commitThenTogglePlay
            case .left, .right, .up, .down, .skipBack, .skipForward, .tapSurface, .idle: .ignore
            }
        case .menu:
            switch input {
            case .select: .activate
            case .back, .tapSurface: .closeMenu
            case .playPause: .togglePlay
            case .left, .right, .up, .down, .skipBack, .skipForward, .idle: .ignore
            }
        case .info:
            switch input {
            case .select: .activate
            case .back, .tapSurface: .closeInfo
            case .playPause: .togglePlay
            case .left, .right, .up, .down, .skipBack, .skipForward, .idle: .ignore
            }
        case .failed:
            switch input {
            case .select: .activate
            case .back: .exit
            case .left, .right, .up, .down, .playPause, .skipBack, .skipForward, .tapSurface, .idle: .ignore
            }
        }
    }
}
