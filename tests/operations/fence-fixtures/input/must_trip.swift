struct Screen: View {
    var body: some View {
        content
            .onMoveCommand { direction in move(direction) }
            .onExitCommand { dismiss() }
    }
}
