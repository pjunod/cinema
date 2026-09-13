internal class Owner(private val onError: (String) -> Unit) {
    var playFailure: String? = null
    fun stop(message: String) {
        onError(message)
        playbackNotice = message
        playFailure = message
    }
    fun appendNotice(extra: String) {
        playFailure += extra
    }
    fun sneaky(message: String) {
        onError.invoke(message)
        val cb = onError
        onError
            (message)
        playFailure
            = message
        cb(message)
    }
}
