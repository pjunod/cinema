internal class Owner(private val onError: (String) -> Unit) {
    var playFailure: String? = null
    fun stop(message: String) {
        onError(message)
        playbackNotice = message
        playFailure = message
    }
}
internal fun appendNotice(extra: String) {
    playFailure += extra
}
