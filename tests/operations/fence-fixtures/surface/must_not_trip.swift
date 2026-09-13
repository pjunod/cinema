final class Reader {
    @Published private(set) var failed = false
    @Published private(set) var playbackError: String? = nil
    func describe() -> String {
        if failed == true { return "failed" }
        if playbackError != nil { return playbackError ?? "" }
        let errorStatusCode = 503
        return errorStatusCode == 503 ? "not yet" : "ok"
    }
}
