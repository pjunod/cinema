package tv.plurx.app.livetv

/**
 * Pure transcription of the `live` section of
 * `tests/playback/player-input-contract.json`.
 *
 * Live television is the finite player's contract with everything that assumes
 * a timeline removed and one thing added. There is nothing to seek, nothing to
 * scrub, no menu and no info panel; there is a channel. So this is a sibling of
 * [tv.plurx.app.player.PlayerInputPolicy] rather than more cases inside it — a
 * live stream can never enter `timeline`, `scrub`, `menu` or `info`, and a
 * reducer that claimed otherwise would be lying in the one place the clients
 * are tested against.
 */
internal enum class LiveTvInputSurface(val contractName: String) {
    TenFoot("ten-foot"),
    Touch("touch"),
}

internal enum class LiveTvInputState(val contractName: String) {
    Hidden("hidden"),
    Overlay("overlay"),
    Page("page"),
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
    FocusRow("focus_row"),
    Activate("activate"),
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
            LiveTvInputState.Hidden -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                LiveTvContractInput.Select,
                LiveTvContractInput.TapSurface,
                -> LiveTvInputOutcome.Reveal
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.Idle -> LiveTvInputOutcome.Ignore
            }
            // Preview-then-commit: move focus, Select tunes, Back hides.
            LiveTvInputState.Overlay -> when (input) {
                LiveTvContractInput.Left,
                LiveTvContractInput.Right,
                LiveTvContractInput.Up,
                LiveTvContractInput.Down,
                -> LiveTvInputOutcome.FocusRow
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back, LiveTvContractInput.Idle -> LiveTvInputOutcome.Hide
                LiveTvContractInput.PlayPause -> LiveTvInputOutcome.TogglePlay
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.Ignore
            }
            // A television never shows an inline player; the state exists only
            // so the surfaces are one shape.
            LiveTvInputState.Page -> LiveTvInputOutcome.Ignore
        }

    private fun touch(state: LiveTvInputState, input: LiveTvContractInput): LiveTvInputOutcome =
        when (state) {
            LiveTvInputState.Hidden -> when (input) {
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.ToggleChrome
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                else -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.Overlay -> when (input) {
                LiveTvContractInput.TapSurface -> LiveTvInputOutcome.ToggleChrome
                LiveTvContractInput.Select -> LiveTvInputOutcome.Activate
                LiveTvContractInput.Back -> LiveTvInputOutcome.Exit
                LiveTvContractInput.Idle -> LiveTvInputOutcome.Hide
                else -> LiveTvInputOutcome.Ignore
            }
            LiveTvInputState.Page -> LiveTvInputOutcome.Ignore
        }
}
