struct Screen: View {
    /// `onMoveCommand` consumes every direction it is given, so anything this
    /// declines is a dead press — which is why the chips are handled here.
    // onExitCommand is the engine's, not ours.
    /* pressesBegan is documented in PlayerRemoteAdapter.swift. */
    /*
     * onPlayPauseCommand lives in PlayerRemoteAdapter too.
     */
    var body: some View { content }
}
