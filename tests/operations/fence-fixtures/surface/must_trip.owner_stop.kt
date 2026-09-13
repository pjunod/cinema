// Blocking sites that forgot the owner's one obligation. `owner_stop_failures`
// must name every one of them; the file's own doc comment is what they break.
package tv.plurx.app.player

internal class Forgetful(private val player: SurfaceOwnerPlayer) {
    // 1. No stop at all.
    fun exhausted(attached: Long) {
        raiseBlocking(source = "owner_exhausted", context = SurfaceContext.Start, attached = attached)
    }

    // 2. A stop that is in a DIFFERENT function, which is the shape a refactor
    //    produces when someone "hoists the stop".
    fun stopSomewhereElse() {
        player.playbackRequested = false
    }

    fun stopped(attached: Long) {
        raiseBlocking(source = "owner_stopped", context = SurfaceContext.Attached, attached = attached)
    }

    // 3. A stop that is AFTER the raise, which is the ordering the contract's
    //    whole §3.4 exists to forbid.
    fun afterwards(attached: Long) {
        raiseBlocking(source = "owner_stopped", context = SurfaceContext.Attached, attached = attached)
        player.playbackRequested = false
    }

    private fun raiseBlocking(source: String, context: SurfaceContext, attached: Long) {}
}
