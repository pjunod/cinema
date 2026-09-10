package tv.plurx.app.livetv

/**
 * Pure transcription of the `live` section of
 * `tests/playback/player-input-contract.json`.
 *
 * Live television is the finite player's contract with everything that assumes
 * a timeline removed and one thing added. There is nothing to seek or scrub;
 * guide, menu, programme-detail and Info panels are explicit states. This is a sibling of
 * [tv.plurx.app.player.PlayerInputPolicy] rather than more cases inside it — a
 * live stream can never enter timeline or scrub states.
 */
internal enum class LiveTvInputSurface(val contractName: String) {
    TenFoot("ten-foot"),
    Touch("touch"),
}

internal enum class LiveTvInputState(val contractName: String) {
    Browser("browser"),
    FullscreenHidden("fullscreen_hidden"),
    FullscreenControls("fullscreen_controls"),
    TemporaryGuide("temporary_guide"),
    Menu("menu"),
    ProgrammeDetails("programme_details"),
    StreamInfo("stream_info"),
}

internal enum class LiveTvContractInput(val contractName: String) {
    Left("left"),
    Right("right"),
    Up("up"),
    Down("down"),
    Select("select"),
    Back("back"),
    PlayPause("play_pause"),
    TapSurface("tap_surface"),
    Idle("idle"),
}

internal enum class LiveTvInputOutcome(val contractName: String) {
    Reveal("reveal"),
    Hide("hide"),
    Delegate("delegate"),
    FocusControl("focus_control"),
    FocusCell("focus_cell"),
    FocusPanel("focus_panel"),
    Activate("activate"),
    ClosePanel("close_panel"),
    ReturnBrowser("return_browser"),
    StripPrev("strip_prev"),
    StripNext("strip_next"),
    Tune("tune"),
    ChannelUp("channel_up"),
    ChannelDown("channel_down"),
    TogglePlay("toggle_play"),
    ToggleChrome("toggle_chrome"),
    Exit("exit"),
    Ignore("ignore"),
}

internal object LiveTvInputPolicy {
    /**
     * The contract's own numbers, in one place. Kotlin cannot read
     * `tests/playback/player-input-contract.json` at runtime, so these are
     * transcribed — and `LiveTvInputPolicyTest` asserts every one of them
     * against the fixture, which is the half that keeps them honest.
     */
    const val HIDE_AFTER_MS: Long = 4_000L

    /**
     * A held channel key is ONE tuner start, not ten. This is the whole
     * guardrail against a channel-surf storm opening a device repeatedly.
     */
    const val CHANNEL_COALESCE_MS: Long = 350L

    fun route(
        surface: LiveTvInputSurface,
        state: LiveTvInputState,
        input: LiveTvContractInput,
    ): LiveTvInputOutcome = when (surface) {
        LiveTvInputSurface.TenFoot -> tenFoot(state, input)
        LiveTvInputSurface.Touch -> touch(state, input)
    }

    // Exhaustive with no `else`: adding an input to the enum must break
    // compilation at every state until it has been ruled on.
    private fun tenFoot(state: LiveTvInputState, input: LiveTvContractInput): LiveTvInputOutcome =
        when (state) {
            // The 2026-09-02 ruling, unchanged: a direction on a hidden
            // overlay only reveals it. Nothing here changes channel.
            LiveTvInputState.FullscreenHidden -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                LiveTvContractInput.Select,
                LiveTvContractInput.TapSurface,
                -> LiveTvInputOutcome.Reveal
                LiveTvContractInput.Back -> LiveTvInputOutcome.ReturnBrowser
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.Idle -> LiveTvInputOutcome.Ignore
            }
            // Preview-then-commit: move focus, Select tunes, Back hides.
            LiveTvInputState.FullscreenControls -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                -> LiveTvInputOutcome.FocusControl
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back, LiveTvContractInput.Idle -> LiveTvInputOutcome.Hide
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.Ignore
            }
            // Root browsing delegates ordinary focus and activation to
            // Compose; this table owns only the explicit Back escape.
            LiveTvInputState.Browser -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                LiveTvContractInput.Select,
                LiveTvContractInput.TapSurface,
                -> LiveTvInputOutcome.Delegate
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                LiveTvContractInput.PlayPause, LiveTvContractInput.Idle -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.TemporaryGuide -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                -> LiveTvInputOutcome.FocusCell
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back -> LiveTvInputOutcome.ClosePanel
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.TapSurface, LiveTvContractInput.Idle -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.Menu,
            LiveTvInputState.ProgrammeDetails,
            LiveTvInputState.StreamInfo,
            -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                -> LiveTvInputOutcome.FocusPanel
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back -> LiveTvInputOutcome.ClosePanel
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.TapSurface, LiveTvContractInput.Idle -> LiveTvInputOutcome.Ignore
            }
        }

    private fun touch(state: LiveTvInputState, input: LiveTvContractInput): LiveTvInputOutcome =
        when (state) {
            LiveTvInputState.FullscreenHidden -> when (input) {
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.ToggleChrome
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                else -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.FullscreenControls -> when (input) {
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.ToggleChrome
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                LiveTvContractInput.Idle -> LiveTvInputOutcome.Hide
                else -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.Browser -> if (input == LiveTvContractInput.Back) {
                LiveTvInputOutcome.Exit
            } else {
                LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.TemporaryGuide -> when (input) {
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back -> LiveTvInputOutcome.ClosePanel
                else -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.Menu,
            LiveTvInputState.ProgrammeDetails,
            LiveTvInputState.StreamInfo,
            -> when (input) {
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back, LiveTvContractInput.TapSurface -> LiveTvInputOutcome.ClosePanel
                else -> LiveTvInputOutcome.Ignore
            }
        }
}
