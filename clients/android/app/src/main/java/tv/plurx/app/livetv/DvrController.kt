package tv.plurx.app.livetv

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * Everything the Live TV screen knows about the DVR, in one place.
 *
 * It deliberately has two cadences. The lightweight **overview** follows the
 * foreground five/30-second policy. The schedule/reminder **marks** rebuild
 * only when the active identity changes, after a mutation, when a guide loads,
 * or after the 30-second ceiling; guide cells never create their own timers.
 * Due reminders retain their separate bounded poll because their whole job is
 * to notice a moment passing while the viewer sits still.
 */
data class DvrScreenState(
    val status: DvrStatus? = null,
    val overview: DvrOverview? = null,
    val overviewError: String? = null,
    val marks: DvrGuideMarks = DvrGuideMarks.EMPTY,
    /** The plan, cancelled rows included, so Scheduled can offer Restore. */
    val schedule: List<DvrRecording> = emptyList(),
    val conflicts: Int = 0,
    val library: List<DvrRecording> = emptyList(),
    val rules: List<DvrRule> = emptyList(),
    val attention: List<DvrAttentionProjection> = emptyList(),
    val attentionTotal: Int = 0,
    val attentionNext: String? = null,
    val selectedRecording: DvrRecording? = null,
    val selectedEvents: List<DvrEvent> = emptyList(),
    val selectedEventsNext: String? = null,
    val selectedHistoryComplete: Boolean = false,
    val selectedHistoryTruncatedBefore: Long? = null,
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

internal fun DvrScreenState.indicatorLabel(): String? {
    val sample = overview ?: return null
    if (!sample.fresh) return if (sample.counts.active > 0) "Status unavailable" else null
    return buildList {
        fun append(count: Int?, label: String) { if (count != null && count > 0) add("$count $label") }
        append(sample.counts.recording, "recording")
        append(sample.counts.starting, "starting")
        append(sample.counts.reconnecting, "reconnecting")
        append(sample.counts.finishing, "finishing")
        append(sample.counts.unconfirmed, "unconfirmed")
    }.takeIf { it.isNotEmpty() }?.joinToString(" · ")
}

class DvrController(
    private val api: DvrApi,
    private val scope: CoroutineScope,
) {
    private val mutableState = MutableStateFlow(DvrScreenState())
    val state = mutableState.asStateFlow()
    private var duePoll: Job? = null
    private var observationPoll: Job? = null
    private val overviewMutex = Mutex()
    private val highFrequencySurfaces = mutableSetOf<String>()
    private var observationFailures = 0
    private var lastActiveIds = emptySet<String>()
    private var lastMarksAt = 0L

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
            refreshOverviewNow()
        }
    }

    /** One application-scope foreground loop; screens only express cadence. */
    fun startObservation() {
        if (observationPoll?.isActive == true) return
        observationPoll = scope.launch {
            while (true) {
                refreshOverviewNow()
                val snapshot = mutableState.value.overview
                val active = (snapshot?.counts?.active ?: 0) > 0 ||
                    snapshot?.next_capture_start?.let { it <= System.currentTimeMillis() / 1000 + 30 } == true
                val base = if (highFrequencySurfaces.isNotEmpty() || active) 5_000L else 30_000L
                val backedOff = if (observationFailures == 0) base else
                    (5_000L shl (observationFailures - 1).coerceAtMost(3)).coerceAtMost(30_000L)
                delay(maxOf(base, backedOff))
            }
        }
    }

    fun stopObservation() {
        observationPoll?.cancel()
        observationPoll = null
        duePoll?.cancel()
        duePoll = null
    }

    fun setHighFrequency(surface: String, visible: Boolean) {
        if (visible) highFrequencySurfaces += surface else highFrequencySurfaces -= surface
    }

    private suspend fun refreshOverviewNow() = overviewMutex.withLock {
        try {
            val overview = api.overview()
            val identity = overview.active.mapTo(mutableSetOf()) { it.recording_id }
            mutableState.value = mutableState.value.copy(overview = overview, overviewError = null)
            observationFailures = 0
            val now = System.currentTimeMillis()
            if (identity != lastActiveIds || now - lastMarksAt >= MARK_REFRESH_MS) {
                lastActiveIds = identity
                lastMarksAt = now
                refreshMarks()
            }
        } catch (cancelled: CancellationException) {
            throw cancelled
        } catch (error: Exception) {
            observationFailures += 1
            mutableState.value = mutableState.value.copy(
                overviewError = error.message ?: dvrMessage("dvr_unreachable"),
            )
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

    fun refreshAttention(more: Boolean = false) {
        scope.launch {
            try {
                val current = mutableState.value
                val page = api.attention(if (more) current.attentionNext else null)
                mutableState.value = mutableState.value.copy(
                    attention = if (more) current.attention + page.rows else page.rows,
                    attentionTotal = page.total,
                    attentionNext = page.next,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    fun selectRecording(id: String) {
        scope.launch {
            try {
                val row = api.recording(id)
                val page = api.events(id)
                mutableState.value = mutableState.value.copy(
                    selectedRecording = row,
                    selectedEvents = page.rows,
                    selectedEventsNext = page.next,
                    selectedHistoryComplete = page.history_complete,
                    selectedHistoryTruncatedBefore = page.truncated_before_sequence,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    fun refreshSelectedEvents() {
        val selected = mutableState.value.selectedRecording ?: return
        val latest = mutableState.value.selectedEvents.maxOfOrNull { it.sequence } ?: return
        scope.launch {
            try {
                val row = api.recording(selected.id)
                val page = api.events(selected.id, after = latest.toString())
                val known = mutableState.value.selectedEvents.mapTo(mutableSetOf()) { it.event_id }
                mutableState.value = mutableState.value.copy(
                    selectedRecording = row,
                    selectedEvents = page.rows.asReversed().filterNot { it.event_id in known } +
                        mutableState.value.selectedEvents,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                mutableState.value = mutableState.value.copy(overviewError = error.message)
            }
        }
    }

    fun loadOlderSelectedEvents() {
        val selected = mutableState.value.selectedRecording ?: return
        val before = mutableState.value.selectedEventsNext ?: return
        scope.launch {
            try {
                val page = api.events(selected.id, before = before)
                mutableState.value = mutableState.value.copy(
                    selectedEvents = mutableState.value.selectedEvents + page.rows,
                    selectedEventsNext = page.next,
                    selectedHistoryComplete = page.history_complete,
                    selectedHistoryTruncatedBefore = page.truncated_before_sequence,
                )
            } catch (cancelled: CancellationException) {
                throw cancelled
            } catch (error: Exception) {
                report(error)
            }
        }
    }

    fun acknowledgeSelectedAttention() {
        val selected = mutableState.value.selectedRecording ?: return
        val latest = mutableState.value.selectedEvents.maxOfOrNull { it.sequence } ?: return
        scope.launch {
            try {
                api.acknowledgeAttention(selected.id, latest)
                refreshAttention()
                mutableState.value = mutableState.value.copy(message = "Marked ${selected.title} reviewed.")
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

    fun recordManual(channelId: String, start: Long, end: Long, title: String) =
        mutate("Channel recording scheduled.") { api.recordManual(channelId.trim(), start, end, title) }

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
            refreshOverviewNow()
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
            refreshOverviewNow()
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
        const val MARK_REFRESH_MS: Long = 30_000
    }
}
