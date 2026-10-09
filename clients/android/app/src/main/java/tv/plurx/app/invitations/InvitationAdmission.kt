package tv.plurx.app.invitations

/** Synchronous local visibility gate shared by resident and FCM owners; no HTTP precedes display. */
internal class InvitationAdmission(private val now: () -> Long = { System.currentTimeMillis() / 1000 }) {
    data class State(val installation: String?, val loginCurrent: Boolean, val permission: Boolean,
        val channelEnabled: Boolean, val fcmEnabled: Boolean, val residentActive: Boolean)
    private val seen = LinkedHashMap<String, Long>()
    private var lastClock = 0L
    @Synchronized fun admit(state: State, id: String, transport: String, expires: Long? = null): Boolean {
        val time = now()
        if (time < lastClock || time !in 1..(tv.plurx.app.remote.REMOTE_MAX_INTEGER - 86400)) return false
        lastClock = time
        seen.entries.removeAll { it.value <= time }
        if (!state.loginCurrent || !state.permission || !state.channelEnabled || state.installation == null) return false
        if (transport == "fcm" && !state.fcmEnabled || transport == "android_resident" && !state.residentActive || transport !in setOf("fcm", "android_resident")) return false
        if (runCatching { InvitationWire.installation(id) }.getOrNull() != state.installation || expires != null && expires <= time || id in seen) return false
        // Never evict an unexpired dedupe entry to admit more: bounded saturation is advisory unavailable.
        if (seen.size >= 256) return false
        seen[id] = expires?.coerceAtMost(time + 86400) ?: (time + 86400)
        return true
    }
    @Synchronized fun restore(entries: Map<String, Long>, clock: Long = 0) {
        InvitationWire.counter(clock, true); lastClock = clock
        require(entries.size <= 256)
        entries.forEach { (id, deadline) -> InvitationWire.invitation(id); InvitationWire.counter(deadline) }
        seen.clear(); seen.putAll(entries.filterValues { it > now() })
    }
    @Synchronized fun validClock(): Boolean = now().let { it >= lastClock && it in 1..(tv.plurx.app.remote.REMOTE_MAX_INTEGER - 86400) }
    @Synchronized fun clock(): Long = lastClock
    @Synchronized fun snapshot(): Map<String, Long> = seen.toMap()
}
