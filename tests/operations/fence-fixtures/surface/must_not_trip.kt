internal class Reader {
    var playFailure: String? by mutableStateOf(null)
    var playbackNotice: String? by mutableStateOf(null)
    fun state(): String {
        if (playFailure != null) return "failed"
        if (playbackNotice == null) return "quiet"
        val code = error.errorCode
        return if (code == 1002) "behind" else "other"
    }
}
