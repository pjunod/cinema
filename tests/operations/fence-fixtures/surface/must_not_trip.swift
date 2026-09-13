final class Reader {
    @Published private(set) var failed = false
    @Published private(set) var playbackError: String? = nil
    @Published private(set) var surface = PlaybackSurfaceModel()
    private(set) var surfaceHistory = PlaybackSurfaceHistory()
    func describe() -> String {
        if failed == true { return "failed" }
        if playbackError != nil { return playbackError ?? "" }
        let errorStatusCode = 503
        return errorStatusCode == 503 ? "not yet" : "ok"
    }
    // Reads are never scanned: the view projects the surface, the ledger
    // renders its history, and a ladder keeps every raw error it has.
    func render() -> String {
        let surface = controller.surface.surface
        let history = controller.surfaceHistory.ledgerSummary
        if surface.kind == .blocking { return history }
        return surface.detail ?? surface.title ?? ""
    }
}
