// Every line below is a write the `kotlin_owner` patterns must catch inside
// `PlaybackSurfaceOwner.kt`. It gets the whole Kotlin set EXCEPT the generic
// raise — those are the file's own private helpers — so the generic raise is
// proved instead by `owner_stop_failures` and its own fixture below.
package tv.plurx.app.player

class Bypasses {
    // 1. The imperative channel coming back inside the owner.
    fun onError(message: String) {}
    private val channel: (String) -> Unit = { onError(it) }
    var playFailure: String? = null
    var playbackNotice: String? = null

    fun writeTheDeletedFields(message: String) {
        playFailure = message
        playbackNotice = message
    }

    // 2. Publishing the surface anywhere but the presenter's one assignment.
    fun publish(surface: PlaybackSurface) {
        _surface.value = surface
        _surface.tryEmit(surface)
        _surface.update { surface }
    }

    // 3. Building a surface or a fault by hand instead of raising one.
    fun build(fault: PlaybackFault): PlaybackSurface {
        val blocking = PlaybackSurface.Blocking(fault)
        val banner = PlaybackSurface.Banner(fault)
        return PlaybackSurface.Indicator(fault)
    }

    fun forge(): PlaybackFault = PlaybackFault(
        cls = SurfaceClass.Stopped,
        source = "owner_stopped",
        attached = 0,
        intent = null,
        raisedAtMs = 0,
    )

    // 4. A screen-held failure string, in the owner this time.
    var surfaceMessage by mutableStateOf<String?>(null)
}
