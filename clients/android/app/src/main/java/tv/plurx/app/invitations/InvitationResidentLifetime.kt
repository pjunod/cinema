package tv.plurx.app.invitations

/** One explicit run owns one selected Wi-Fi network; retired callbacks cannot re-arm it. */
internal class InvitationResidentLifetime {
    data class Run(val profile: String, val installation: String, val id: String)
    private var active: Run? = null
    private var network: Any? = null
    @Synchronized fun begin(run: Run): Boolean {
        if (active != null) return false
        active = run; network = null; return true
    }
    @Synchronized fun owns(run: Run) = active == run
    @Synchronized fun available(run: Run, offered: Any): Boolean {
        if (active != run || network != null) return false
        network = offered; return true
    }
    @Synchronized fun lost(run: Run, offered: Any) = active == run && network == offered
    @Synchronized fun end(run: Run): Boolean {
        if (active != run) return false
        active = null; network = null; return true
    }
}
