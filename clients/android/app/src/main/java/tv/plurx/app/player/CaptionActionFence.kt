package tv.plurx.app.player

import java.util.UUID
import tv.plurx.app.data.refusalStatusOf

/** One mutation owner, with exact request identity retained only across uncertain outcomes. */
internal class CaptionActionFence(private val newId: () -> String = { UUID.randomUUID().toString() }) {
    internal data class Ticket(val attempt: Long, val key: String, val requestId: String, val stopRevision: Long, val replay: Boolean)
    private var serial = 0L
    private var pending: Ticket? = null
    private var active: Ticket? = null
    var stopRevision: Long = 0; private set
    val inFlight: Boolean get() = active != null
    val hasPending: Boolean get() = pending != null
    fun begin(key: String): Ticket? {
        if (inFlight) return null
        val previous = pending?.takeIf { it.key == key }
        val ticket = Ticket(++serial, key, previous?.requestId ?: newId(), previous?.stopRevision ?: stopRevision, previous != null)
        pending = ticket; active = ticket
        return ticket
    }
    fun owns(ticket: Ticket): Boolean = active === ticket
    fun stoppedSince(ticket: Ticket): Boolean = stopRevision != ticket.stopRevision
    fun stop() { stopRevision++ }
    fun finish(ticket: Ticket, ambiguous: Boolean = false): Boolean {
        if (!owns(ticket)) return false
        active = null
        if (!ambiguous) pending = null
        return true
    }
    fun invalidate(keepPending: Boolean = false) { active = null; if (!keepPending) pending = null }
}
internal fun captionActionAmbiguous(error: Exception): Boolean {
    val status = refusalStatusOf(error)
    return status == null || status == 408 || status >= 500
}
