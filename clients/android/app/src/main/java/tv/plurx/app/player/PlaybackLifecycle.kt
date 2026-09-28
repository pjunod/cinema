package tv.plurx.app.player

/** One owner pause independent of the viewer's Play/Pause intent. */
internal enum class LifecyclePlaybackEffect { None, PauseVideo, ResumeVideo }

internal data class LifecyclePlaybackTransition(
    val ownerPaused: Boolean,
    val effect: LifecyclePlaybackEffect,
)

internal fun lifecyclePlaybackTransition(
    ownerPaused: Boolean,
    foreground: Boolean,
    inPictureInPicture: Boolean,
    audioOnly: Boolean,
    viewerRequested: Boolean,
): LifecyclePlaybackTransition {
    if (audioOnly) return LifecyclePlaybackTransition(false, LifecyclePlaybackEffect.None)
    val visible = foreground || inPictureInPicture
    if (!visible && viewerRequested && !ownerPaused) {
        return LifecyclePlaybackTransition(true, LifecyclePlaybackEffect.PauseVideo)
    }
    if (visible && ownerPaused) {
        return LifecyclePlaybackTransition(
            false,
            if (viewerRequested) LifecyclePlaybackEffect.ResumeVideo else LifecyclePlaybackEffect.None,
        )
    }
    return LifecyclePlaybackTransition(ownerPaused, LifecyclePlaybackEffect.None)
}
