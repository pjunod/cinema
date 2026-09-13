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

// Reading a surface is never a write. Neither is naming its types, collecting
// the flow the presenter publishes, or the ladder's own error handling — the
// contract leaves reading alone (PLAYBACK-SURFACE-CONTRACT.md §4).
internal class SurfaceReader {
    fun render(surface: PlaybackSurface, controller: Controller) {
        val fault: PlaybackFault? = surface.fault
        val failed = surface.entersFailedRouting
        val collected = controller.surface.collectAsStateWithLifecycle()
        val banner = surface as? PlaybackSurface.Banner
        val kind = when (surface) {
            is PlaybackSurface.None -> "none"
            is PlaybackSurface.Indicator -> "indicator"
            is PlaybackSurface.Banner -> "banner"
            is PlaybackSurface.Blocking -> "blocking"
        }
        val title = fault?.title
        if (_surface.value is PlaybackSurface.Blocking) return
    }

    fun ladder(error: PlaybackException, status: Int?) {
        val transport = isTransportPlaybackError(error.errorCode)
        val detail = error.errorCodeName
        val retry = status == 503 || error.errorCode == 1002
        var pending: String? = null
        var label by mutableStateOf<String>("idle")
        var count by mutableStateOf<Int?>(null)
    }
}
