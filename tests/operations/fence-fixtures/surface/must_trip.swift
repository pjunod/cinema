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
}
