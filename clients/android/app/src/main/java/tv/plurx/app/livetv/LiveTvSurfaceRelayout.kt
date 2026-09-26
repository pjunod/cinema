package tv.plurx.app.livetv

/**
 * When the moved PlayerView subtree must be measured again.
 *
 * The surface is one View subtree shared by the inline picture box and the
 * fullscreen box through `movableContentOf`. A moved View keeps its last
 * measured size, so the decision to force a fresh layout pass follows the
 * host box, not the View: a real, changed, non-empty host size is the only
 * signal that means "the box you sit in is a different box now".
 */
object LiveTvSurfaceRelayout {
    fun hostResized(previousWidth: Int, previousHeight: Int, width: Int, height: Int): Boolean {
        if (width <= 0 || height <= 0) return false
        return width != previousWidth || height != previousHeight
    }
}
