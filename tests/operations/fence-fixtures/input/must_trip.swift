struct Screen: View {
    var body: some View {
        content
            .onMoveCommand { direction in move(direction) }
            .onExitCommand { dismiss() }
            /* focus = */ .onPlayPauseCommand { toggle() }
    }
    /*
     * a block whose closing line then runs code
     */ .onKeyPress { press in consume(press) }
}
