package tv.plurx.app.remote

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/** Completion cleans its own busy marker even after semantic retirement. */
internal class RemoteOwnedAttempt {
    private var serial = 0L
    private var busyTicket by mutableStateOf<Long?>(null)
    val busy get() = busyTicket != null
    fun begin(markBusy: Boolean = true): Long = (++serial).also { if (markBusy) busyTicket = it }
    fun accepts(ticket: Long) = serial == ticket
    fun retire() { serial++ }
    fun finish(ticket: Long) { if (busyTicket == ticket) busyTicket = null }
}
