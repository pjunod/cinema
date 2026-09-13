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

// The six bypasses the M3 review drove through the pre-M3 Kotlin arm, which
// guarded only `onError`, `playFailure` and `playbackNotice` — all three of
// which M3 deleted, so the arm guarded nothing that existed.
internal class Bypasses {
    private val _surface = MutableStateFlow<PlaybackSurface>(PlaybackSurface.None)

    // 1. The publish, outside the anchors.
    fun publishDirectly(value: PlaybackSurface) {
        _surface.value = value
    }

    // 2. The same write in every other spelling the flow offers.
    fun publishByEmit(value: PlaybackSurface) {
        _surface.tryEmit(value)
        _surface
            .emit(value)
        _surface.update { value }
    }

    // 3. A surface built outside the presenter.
    fun buildSurface(fault: PlaybackFault): PlaybackSurface {
        val blocking = PlaybackSurface.Blocking(fault)
        val banner = PlaybackSurface.Banner(fault)
        return PlaybackSurface.Indicator(fault)
    }

    // 4. A fault built by hand instead of raised through the owner.
    fun buildFault(): PlaybackFault = PlaybackFault(
        cls = SurfaceClass.Stopped,
        source = "owner_stopped",
        attached = 0,
        intent = null,
        raisedAtMs = 0,
    )

    // 5. A site reaching for a generic raise instead of adding a named one —
    //    including the spelling that drops the owner's stop.
    fun raiseGenerically(surfaceOwner: PlaybackSurfaceOwner) {
        surfaceOwner.raise(source = "owner_exhausted", context = SurfaceContext.Attached, attached = 1)
        surfaceOwner.stopAndRaise(source = "owner_stopped", context = SurfaceContext.Attached, attached = 1)
    }

    // 6. The imperative channel coming back under a new name.
    var surfaceMessage by mutableStateOf<String?>(null)
    private val onSurfaceMessage: (String) -> Unit = { surfaceMessage = it }
}
