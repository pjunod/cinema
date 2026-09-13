package tv.plurx.app.livetv

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/**
 * Everything the Live TV screen knows about the DVR, in one place.
 *
 * It deliberately has two cadences. The **marks** — the schedule and the
 * reminders behind the glyphs on a guide cell — are read once when the guide
 * loads and again after every mutation, and never on a timer: nothing changes
 * them but this viewer's own presses and the owner's fifteen-second loop, and
 * a guide-sized answer repeated every half minute would be work nobody asked
 * for. The **due reminders** are a different, one-row-at-most read, and that
 * one does poll, because its whole job is to notice a moment passing while the
 * viewer sits still.
 */
data class DvrScreenState(
    val status: DvrStatus? = null,
    val marks: DvrGuideMarks = DvrGuideMarks.EMPTY,
    /** The plan, cancelled rows included, so Scheduled can offer Restore. */
    val schedule: List<DvrRecording> = emptyList(),
    val conflicts: Int = 0,
    val library: List<DvrRecording> = emptyList(),
    val rules: List<DvrRule> = emptyList(),
    /** Every reminder this viewer holds — what the alarm mirror reconciles against. */
    val reminders: List<DvrReminder> = emptyList(),
    val due: List<DvrReminder> = emptyList(),
    val busy: Boolean = false,
    val message: String = "",
    /**
     * Who is holding the tuners, when a refusal said so. Kept beside the
     * message because "all slots are busy" without naming them leaves the
     * viewer to guess where their television went.
     */
    val holders: List<DvrHolder> = emptyList(),
)

class DvrController(
    private val api: DvrApi,
    private val scope: CoroutineScope,
) {
    private val mutableState = MutableStateFlow(DvrScreenState())
    val state = mutableState.asStateFlow()
    private var duePoll: Job? = null

    /**
     * The switch, the root and the slots. Deliberately not the schedule: the
     * marks are read by the guide's own effect, so entering the screen and
     * loading a guide are one read rather than two. A failure here is reported
     * and never fatal — a Live TV screen with an unreachable DVR is the screen
     * that existed before this feature, not a broken one.
     */
    fun load() {
        scope.launch {
            runCatching { api.status() }.getOrNull()?.let { status ->
                mutableState.value = mutableState.value.copy(status = status)
            }
        }
    }

    /** Read the schedule and the reminders, and rebuild the cell marks from them. */
    fun refreshMarks() {
        scope.launch {
            try {
                val schedule = api.schedule(cancelled = true)
                val reminders = api.reminders()
                mutableState.value = mutableState.value.copy(
                    schedule = schedule.rows,
                    conflicts = schedule.conflicts,
                    reminders = reminders,
                    marks = DvrGuideMarks(schedule.rows, reminders),
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    fun refreshLibrary() {
        scope.launch {
            try {
                mutableState.value = mutableState.value.copy(
                    library = api.recordings(listOf("recording", "done", "partial", "failed", "missed")),
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    fun refreshRules() {
        scope.launch {
            try {
                mutableState.value = mutableState.value.copy(rules = api.rules())
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    /**
     * Thirty seconds, matching every other client's overlay cadence. It runs
     * only while the screen is composed; a reminder missed because the app was
     * closed is the phone notification's job, not this loop's.
     */
    fun startDuePoll() {
        if (duePoll?.isActive == true) return
        duePoll = scope.launch {
            while (true) {
                runCatching { api.reminders(due = true) }.getOrNull()?.let { due ->
                    mutableState.value = mutableState.value.copy(due = due)
                }
                delay(DUE_POLL_MS)
            }
        }
    }

    fun record(channelId: String, airingStart: Long) = mutate("Recording scheduled.") {
        api.record(channelId, airingStart)
    }

    /**
     * Two presses, not a form: the server fills the rule from the cell, and
     * [onCreated] is the second press — the rules list, open on the rule that
     * was just made, where its channel lock and its keep policy can be changed
     * before the next episode airs.
     */
    fun recordSeries(channelId: String, airingStart: Long, onCreated: (DvrRule) -> Unit = {}) =
        mutate("Series rule created.") {
            val rule = api.createRule(channelId, airingStart)
            refreshRules()
            onCreated(rule)
        }

    fun remind(channelId: String, airingStart: Long) = mutate("Reminder set.") {
        api.remind(channelId, airingStart)
    }

    fun forgetReminder(id: String) = mutate("Reminder removed.") { api.deleteReminder(id) }

    /**
     * Cancel a plan, ask a capture to stop, or delete a finished recording.
     * The finished case needs [deleteFile] because "take this off my schedule"
     * and "delete the file" must never be the same press by accident.
     */
    fun stop(id: String, deleteFile: Boolean = false, onConfirmFile: (String) -> Unit = {}) {
        scope.launch {
            mutableState.value = mutableState.value.copy(busy = true, holders = emptyList())
            try {
                val outcome = api.stop(id, deleteFile)
                val message = when (outcome) {
                    is DvrStopOutcome.Cancelled -> "Removed."
                    is DvrStopOutcome.StopRequested ->
                        "Stopping. The owner closes the file on its next tick."
                    is DvrStopOutcome.FileConfirmationRequired -> ""
                }
                mutableState.value = mutableState.value.copy(busy = false, message = message)
                if (outcome == DvrStopOutcome.FileConfirmationRequired) onConfirmFile(id)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
            refreshMarks()
            refreshLibrary()
        }
    }

    fun restore(id: String) = mutate("Restored to the schedule.") { api.restore(id) }

    fun setRuleEnabled(id: String, enabled: Boolean) =
        mutate(if (enabled) "Rule enabled." else "Rule disabled.") {
            api.updateRule(id, DvrRuleChange.Enabled(enabled))
            refreshRules()
        }

    fun setRuleNewOnly(id: String, newOnly: Boolean) =
        mutate(if (newOnly) "New episodes only." else "Every episode.") {
            api.updateRule(id, DvrRuleChange.NewOnly(newOnly))
            refreshRules()
        }

    fun deleteRule(id: String) = mutate("Rule deleted. Its planned airings are withdrawn, not cancelled.") {
        api.deleteRule(id)
        refreshRules()
    }

    /** Admin only. A viewer without the right gets the typed refusal, said plainly. */
    fun moveRule(id: String, delta: Int) {
        val order = mutableState.value.rules.map { it.id }.toMutableList()
        val at = order.indexOf(id)
        val to = at + delta
        // Computed before the call rather than inside it: an edge rule asked to
        // move past the end has nothing to save, and reporting "order saved"
        // for a request that wrote nothing would be a lie.
        if (at < 0 || to !in order.indices) return
        order[at] = order[to]
        order[to] = id
        mutate("Rule order saved.") {
            mutableState.value = mutableState.value.copy(rules = api.reorderRules(order))
        }
    }

    /** Dismissing a due reminder acks it, so no other device shows it again. */
    fun acknowledge(id: String) {
        scope.launch {
            runCatching { api.ackReminder(id) }
            mutableState.value = mutableState.value.copy(
                due = mutableState.value.due.filterNot { it.id == id },
            )
            refreshMarks()
        }
    }

    /** A reminder whose programme has started is nobody's business any more. */
    fun expireDue(now: Long) {
        val remaining = mutableState.value.due.filter { it.airing_start > now }
        if (remaining.size != mutableState.value.due.size) {
            mutableState.value = mutableState.value.copy(due = remaining)
        }
    }

    private fun mutate(success: String, action: suspend () -> Unit) {
        scope.launch {
            mutableState.value = mutableState.value.copy(busy = true, holders = emptyList())
            try {
                action()
                mutableState.value = mutableState.value.copy(busy = false, message = success)
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
            // Every mutation is one of the two things a mark is made of, so the
            // marks are re-read here rather than by each call site remembering
            // to. This is the "and on every mutation" half of the contract.
            refreshMarks()
        }
    }

    private fun report(error: Exception) {
        val failure = error as? DvrFailure
        mutableState.value = mutableState.value.copy(
            busy = false,
            message = failure?.message ?: dvrMessage("dvr_unreachable"),
            holders = failure?.holders.orEmpty(),
        )
    }

    companion object {
        const val DUE_POLL_MS: Long = 30_000
    }
}
