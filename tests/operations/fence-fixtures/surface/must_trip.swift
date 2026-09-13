extension PlayerController {
    func fail(_ error: Error) {
        failed = true
        self.playbackError = error.localizedDescription
        playbackFailureTitle = "Playback stopped."
        playbackNotice = "Dolby Vision did not start. Retrying…"
    }
    func append(_ extra: String) {
        playbackNotice += extra
    }
    func sneaky() {
        self.failed.toggle()
        (failed, playbackError) = (true, "x")
        self[keyPath: \.playbackError] = "x"
        setValue(true, forKey: "failed")
        failed
            = true
    }
    // The six spellings that got a surface write past the first M2 fence.
    func sneakierStill() {
        surface = PlaybackSurfaceModel()
        self.surface.apply(.raise(fault, context: .attached), now: .now)
        self[keyPath: \.surface] = PlaybackSurfaceModel()
        _surface = Published(initialValue: PlaybackSurfaceModel())
        setValue(nil, forKey: "surfaceHistory")
        surfaceHistory = PlaybackSurfaceHistory()
        surfaceHistory.record(entry, atMs: 0, player: snapshot)
    }
}
