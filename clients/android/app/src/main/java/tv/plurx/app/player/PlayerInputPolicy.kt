package tv.plurx.app.player

internal enum class PlayerInputSurface(val contractName: String) {
    TenFoot("ten-foot"),
    Touch("touch"),
}

internal enum class PlayerInputState(val contractName: String) {
    Hidden("hidden"),
    Transport("transport"),
    Timeline("timeline"),
    Scrub("scrub"),
    Menu("menu"),
    Info("info"),
    Failed("failed"),
}

internal enum class PlayerContractInput(val contractName: String) {
    Left("left"),
    Right("right"),
    Up("up"),
    Down("down"),
    Select("select"),
    Back("back"),
    PlayPause("play_pause"),
    SkipBack("skip_back"),
    SkipForward("skip_forward"),
    TapSurface("tap_surface"),
    Idle("idle"),
}

internal enum class PlayerInputOutcome(val contractName: String) {
    Reveal("reveal"),
    FocusRow("focus_row"),
    FocusMarkerOrIgnore("focus_marker_or_ignore"),
    FocusTransport("focus_transport"),
    Activate("activate"),
    TogglePlay("toggle_play"),
    Skip("skip"),
    Preview("preview"),
    Commit("commit"),
    Cancel("cancel"),
    CancelThenFocusTransport("cancel_then_focus_transport"),
    CancelThenFocusMarkerOrIgnore("cancel_then_focus_marker_or_ignore"),
    CommitThenTogglePlay("commit_then_toggle_play"),
    CloseMenu("close_menu"),
    CloseInfo("close_info"),
    MenuFocus("menu_focus"),
    Hide("hide"),
    Exit("exit"),
    ToggleChrome("toggle_chrome"),
    Ignore("ignore"),
}

/** Pure transcription of tests/playback/player-input-contract.json. */
internal object PlayerInputPolicy {
    fun route(
        surface: PlayerInputSurface,
        state: PlayerInputState,
        input: PlayerContractInput,
    ): PlayerInputOutcome = when (surface) {
        PlayerInputSurface.TenFoot -> tenFoot(state, input)
        PlayerInputSurface.Touch -> touch(state, input)
    }

    private fun tenFoot(
        state: PlayerInputState,
        input: PlayerContractInput,
    ): PlayerInputOutcome = when (state) {
        PlayerInputState.Hidden -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            PlayerContractInput.Up,
            PlayerContractInput.Down,
            PlayerContractInput.Select,
            PlayerContractInput.TapSurface,
            -> PlayerInputOutcome.Reveal
            PlayerContractInput.Back -> PlayerInputOutcome.Exit
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.Idle -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Transport -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            PlayerContractInput.Up,
            PlayerContractInput.Down,
            -> PlayerInputOutcome.FocusRow
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.Hide
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.TapSurface -> PlayerInputOutcome.Ignore
            PlayerContractInput.Idle -> PlayerInputOutcome.Hide
        }
        PlayerInputState.Timeline -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            -> PlayerInputOutcome.Preview
            PlayerContractInput.Up -> PlayerInputOutcome.FocusMarkerOrIgnore
            PlayerContractInput.Down -> PlayerInputOutcome.FocusTransport
            PlayerContractInput.Select,
            PlayerContractInput.PlayPause,
            -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.Back -> PlayerInputOutcome.Hide
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.TapSurface -> PlayerInputOutcome.Ignore
            PlayerContractInput.Idle -> PlayerInputOutcome.Hide
        }
        PlayerInputState.Scrub -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            -> PlayerInputOutcome.Preview
            PlayerContractInput.Up -> PlayerInputOutcome.CancelThenFocusMarkerOrIgnore
            PlayerContractInput.Down -> PlayerInputOutcome.CancelThenFocusTransport
            PlayerContractInput.Select -> PlayerInputOutcome.Commit
            PlayerContractInput.Back -> PlayerInputOutcome.Cancel
            PlayerContractInput.PlayPause -> PlayerInputOutcome.CommitThenTogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            PlayerContractInput.TapSurface,
            PlayerContractInput.Idle,
            -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Menu -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            PlayerContractInput.Up,
            PlayerContractInput.Down,
            -> PlayerInputOutcome.MenuFocus
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.CloseMenu
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Ignore
            PlayerContractInput.TapSurface -> PlayerInputOutcome.CloseMenu
            PlayerContractInput.Idle -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Info -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            PlayerContractInput.Up,
            PlayerContractInput.Down,
            -> PlayerInputOutcome.MenuFocus
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.CloseInfo
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Ignore
            PlayerContractInput.TapSurface -> PlayerInputOutcome.CloseInfo
            PlayerContractInput.Idle -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Failed -> when (input) {
            PlayerContractInput.Left,
            PlayerContractInput.Right,
            -> PlayerInputOutcome.FocusRow
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.Exit
            PlayerContractInput.Up,
            PlayerContractInput.Down,
            PlayerContractInput.PlayPause,
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            PlayerContractInput.TapSurface,
            PlayerContractInput.Idle,
            -> PlayerInputOutcome.Ignore
        }
    }

    private fun touch(
        state: PlayerInputState,
        input: PlayerContractInput,
    ): PlayerInputOutcome = when (state) {
        PlayerInputState.Hidden -> when (input) {
            PlayerContractInput.Back -> PlayerInputOutcome.Exit
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.TapSurface -> PlayerInputOutcome.ToggleChrome
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Transport -> when (input) {
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.Hide
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.TapSurface -> PlayerInputOutcome.ToggleChrome
            PlayerContractInput.Idle -> PlayerInputOutcome.Hide
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Timeline -> when (input) {
            PlayerContractInput.Back -> PlayerInputOutcome.Hide
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.SkipBack,
            PlayerContractInput.SkipForward,
            -> PlayerInputOutcome.Skip
            PlayerContractInput.TapSurface -> PlayerInputOutcome.ToggleChrome
            PlayerContractInput.Idle -> PlayerInputOutcome.Hide
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Scrub -> when (input) {
            PlayerContractInput.Select -> PlayerInputOutcome.Commit
            PlayerContractInput.Back -> PlayerInputOutcome.Cancel
            PlayerContractInput.PlayPause -> PlayerInputOutcome.CommitThenTogglePlay
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Menu -> when (input) {
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.CloseMenu
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.TapSurface -> PlayerInputOutcome.CloseMenu
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Info -> when (input) {
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.CloseInfo
            PlayerContractInput.PlayPause -> PlayerInputOutcome.TogglePlay
            PlayerContractInput.TapSurface -> PlayerInputOutcome.CloseInfo
            else -> PlayerInputOutcome.Ignore
        }
        PlayerInputState.Failed -> when (input) {
            PlayerContractInput.Select -> PlayerInputOutcome.Activate
            PlayerContractInput.Back -> PlayerInputOutcome.Exit
            else -> PlayerInputOutcome.Ignore
        }
    }

    /**
     * The contract's own numbers, in one place. Kotlin cannot read
     * `tests/playback/player-input-contract.json` at runtime, so these are
     * transcribed — and `PlayerInputPolicyTest` asserts every one of them
     * against the fixture, which is the half that keeps them honest.
     */
    const val HIDE_AFTER_MS: Long = 4_000L
    const val SKIP_STEP_MS: Long = 10_000L

    fun previewStepMs(repeatCount: Int): Long = when {
        repeatCount >= 10 -> 60_000L
        repeatCount >= 5 -> 30_000L
        else -> 10_000L
    }

    fun backAction(state: PlayerInputState): PlayerInputOutcome =
        route(PlayerInputSurface.TenFoot, state, PlayerContractInput.Back)

    /**
     * `close_control` in the fixture: the phone's back arrow is a button, not
     * the BACK key. Routed through `Back`, it answered `Hide` in `Transport` —
     * the state it is tapped from — and exited in no state a viewer could tap
     * it from. Every row closes what is open and then ends in `Exit`.
     */
    fun closeSteps(state: PlayerInputState): List<PlayerInputOutcome> = when (state) {
        PlayerInputState.Hidden,
        PlayerInputState.Transport,
        PlayerInputState.Timeline,
        PlayerInputState.Failed,
        -> listOf(PlayerInputOutcome.Exit)
        PlayerInputState.Scrub -> listOf(PlayerInputOutcome.Cancel, PlayerInputOutcome.Exit)
        PlayerInputState.Menu -> listOf(PlayerInputOutcome.CloseMenu, PlayerInputOutcome.Exit)
        PlayerInputState.Info -> listOf(PlayerInputOutcome.CloseInfo, PlayerInputOutcome.Exit)
    }
}
