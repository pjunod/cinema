// The owner as it is supposed to look: named methods, each blocking one doing
// its own stop, reading the player and the reducer freely. Nothing here is a
// write to a surface outside the presenter, and the fence must be silent.
package tv.plurx.app.player

internal class Owner(private val player: SurfaceOwnerPlayer) {
    var current: PlaybackSurface = PlaybackSurface.None
        private set

    // Reading the raw failure is explicitly left alone by the contract.
    fun classify(errorCode: Int, status: Int?, details: String?): String =
        if (status == 401 || status == 403) "auth_401_403" else "owner_stopped"

    fun exhaustedAfterReopenBudget(attached: Long, positionMs: Long) {
        player.playbackRequested = false
        raiseBlocking(
            source = SurfaceSources.OWNER_EXHAUSTED,
            context = SurfaceContext.Attached,
            attached = attached,
            positionMs = positionMs,
        )
    }

    fun controlHold(attached: Long, detail: String) =
        raiseNotice(SurfaceSources.CONTROL_HOLD, SurfaceContext.Attached, attached, detail = detail)

    private fun raiseNotice(
        source: String,
        context: SurfaceContext,
        attached: Long,
        detail: String? = null,
    ) = dispatch(source, context, attached, detail)

    private fun raiseBlocking(
        source: String,
        context: SurfaceContext,
        attached: Long,
        positionMs: Long? = null,
    ) = dispatch(source, context, attached, null)

    private fun dispatch(source: String, context: SurfaceContext, attached: Long, detail: String?) {}
}
