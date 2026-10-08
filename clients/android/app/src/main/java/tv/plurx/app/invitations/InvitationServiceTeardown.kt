package tv.plurx.app.invitations

/** A Service instance never admits replacement until Android destroys its retired sockets. */
internal class InvitationServiceTeardown {
    private var owner: InvitationResidentLifetime.Run? = null
    private var retiring = false
    private var complete = false
    private var latestStart = 0
    @Synchronized fun observe(startId: Int) { latestStart = maxOf(latestStart, startId) }
    @Synchronized fun start(run: InvitationResidentLifetime.Run, startId: Int): Boolean {
        latestStart = maxOf(latestStart, startId)
        if (owner != null || retiring) return false
        owner = run; return true
    }
    @Synchronized fun retire(run: InvitationResidentLifetime.Run): Boolean {
        if (owner != run || retiring) return false
        retiring = true; return true
    }
    @Synchronized fun completed(run: InvitationResidentLifetime.Run): Int? {
        if (owner != run || !retiring) return null
        complete = true; return latestStart
    }
    @Synchronized fun rejectedStopId(): Int? = if (retiring && complete) latestStart else null
}
