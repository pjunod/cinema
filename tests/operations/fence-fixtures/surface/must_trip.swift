extension PlayerController {
    func fail(_ error: Error) {
        failed = true
        self.playbackError = error.localizedDescription
        playbackFailureTitle = "Playback stopped."
        playbackNotice = "Dolby Vision did not start. Retrying…"
    }
}
extension PlayerController {
    func append(_ extra: String) {
        playbackNotice += extra
    }
}
