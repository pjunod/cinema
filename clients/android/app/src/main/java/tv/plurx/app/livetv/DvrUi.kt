package tv.plurx.app.livetv

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.RequestInitialFocus
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import kotlinx.coroutines.delay

/**
 * The three lists behind the Recordings segment. Deliberately not persisted,
 * unlike the browse view: which of the three was last open is a step in one
 * errand, not a habit worth reopening the screen on.
 */
enum class DvrChip(val label: String) {
    Saved("Saved"),
    Upcoming("Upcoming"),
    Attention("Needs attention"),
    Rules("Series rules"),
    Manual("Manual recording"),
    Skipped("Skipped"),
    Reminders("Reminders"),
}

/**
 * Record · Record series · Remind me, beside Watch.
 *
 * One press records, two make a series — the second press is the rule editor,
 * which the server fills from the cell so there is no form to complete first.
 * Watch on a programme that has not started yet is not a tune at all: it says
 * when, and setting a reminder is the only thing it can honestly do.
 */
@Composable
fun DvrCellActions(
    channel: LiveTvChannel,
    programme: LiveTvProgramme,
    dvr: DvrScreenState,
    now: Long,
    onWatch: () -> Unit,
    onRecord: () -> Unit,
    onRecordSeries: () -> Unit,
    onRemind: () -> Unit,
    onStop: (String) -> Unit,
    onForgetReminder: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    val airing = programme.start <= now && now < programme.end
    val row = dvr.marks.recording(channel.id, programme.start)
    val reminder = dvr.reminders.firstOrNull {
        it.channel_id == channel.id && it.airing_start == programme.start && it.state == "armed"
    }
    Column(modifier, verticalArrangement = Arrangement.spacedBy(4.dp)) {
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            // On a future programme Watch and Remind me are deliberately the
            // same press. A viewer who reads "Watch at 8:00" and a viewer who
            // reads "Remind me" both want the same thing, and one reminder per
            // airing is what the server stores whichever they press.
            TvTextButton(
                onClick = if (airing) onWatch else onRemind,
                enabled = channel.watchable || !airing,
                compact = true,
            ) {
                Text(
                    if (airing) "Watch" else "Watch at ${liveTvTime(programme.start)}",
                    style = type.primary,
                )
            }
            when {
                row != null && row.recording -> TvTextButton(
                    onClick = { onStop(row.id) },
                    compact = true,
                ) { Text("Stop recording", style = type.primary) }
                row != null && row.pending -> TvTextButton(
                    onClick = { onStop(row.id) },
                    compact = true,
                ) { Text("Don't record", style = type.primary) }
                else -> TvTextButton(onClick = onRecord, compact = true) {
                    Text("Record", style = type.primary)
                }
            }
            TvTextButton(onClick = onRecordSeries, compact = true) {
                Text("Record series", style = type.primary)
            }
            if (reminder == null) {
                TvTextButton(onClick = onRemind, compact = true) {
                    Text("Remind me", style = type.primary)
                }
            } else {
                TvTextButton(onClick = { onForgetReminder(reminder.id) }, compact = true) {
                    Text("Forget reminder", style = type.primary)
                }
            }
        }
        Text(
            dvrTunerLine(dvr, programme),
            style = type.tertiary,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        dvrRecordingContext(dvr, channel, programme, now)?.let { context ->
            Text(
                "● $context",
                style = type.tertiary,
                color = MaterialTheme.colorScheme.primary,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        // Why, for every state that is not plainly scheduled. A conflict the
        // viewer cannot see the reason for is one they cannot act on.
        if (row != null && row.state != "scheduled" && row.state != "recording") {
            row.state_reason?.let {
                Text(
                    it,
                    style = type.tertiary,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                )
            }
        }
        dvr.message.takeIf { it.isNotEmpty() }?.let {
            Text(it, style = type.tertiary, maxLines = 2, overflow = TextOverflow.Ellipsis)
        }
        dvr.holders.forEach { holder ->
            Text(
                dvrHolderLine(holder),
                style = type.tertiary,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

/** Exact airing for badges; same-channel overlap is padding context only. */
internal fun dvrRecordingContext(
    dvr: DvrScreenState,
    channel: LiveTvChannel,
    programme: LiveTvProgramme?,
    now: Long,
): String? {
    if (programme != null) {
        dvr.overview?.active?.firstOrNull {
            it.channel_id == channel.id && it.airing_start == programme.start
        }?.let { return it.display_state }
        dvr.marks.recording(channel.id, programme.start)?.let {
            return if (it.stop_requested_at_ms != null) "Stopping" else
                it.state.replaceFirstChar { value -> value.uppercase() }
        }
    }
    val active = dvr.overview?.active?.firstOrNull {
        it.channel_id == channel.id && it.capture_start <= now && now < it.capture_end
    } ?: return null
    return when {
        now < active.airing_start -> "Recording early padding for ${active.title}"
        now >= active.airing_end ->
            "Recording ${active.title} · end padding · until ${liveTvTime(active.capture_end)}"
        else -> null
    }
}

/**
 * What the tuners look like at this programme's hour, computed here rather
 * than asked for: the server already told the client how many slots there are
 * and what is planned, and a route for the arithmetic would be a second answer
 * that could disagree with the schedule on screen.
 */
internal fun dvrTunerLine(dvr: DvrScreenState, programme: LiveTvProgramme): String {
    val status = dvr.status ?: return "Recording status unavailable"
    if (!status.enabled) return "Recording is switched off"
    val busy = dvr.schedule.count { row ->
        (row.state == "scheduled" || row.recording) &&
            row.capture_start < programme.end && programme.start < row.capture_end
    }
    val free = (status.slots.max - busy).coerceAtLeast(0)
    return buildString {
        append("${status.slots.max} tuners")
        append(" · $busy recording then")
        append(if (free == 0) " · none free" else " · $free free")
        if (status.slots.reserve > 0) append(" · ${status.slots.reserve} kept for viewers")
    }
}

/** "5.1 WNYW · Kitchen Table until 9:00" — who to stop, and what stopping costs. */
internal fun dvrHolderLine(holder: DvrHolder): String {
    val head = "${holder.guide_number} ${holder.channel_name}".trim()
    val sinks = holder.sinks.joinToString(", ") { "${it.title} until ${liveTvTime(it.ends_at)}" }
    return listOf(head, sinks).filter { it.isNotEmpty() }.joinToString(" · ")
}

/**
 * The Recordings segment: the library it produced, the plan it is working to,
 * and the rules behind the plan.
 */
@Composable
fun DvrRecordingsPanel(
    dvr: DvrScreenState,
    now: Long,
    chip: DvrChip,
    onChip: (DvrChip) -> Unit,
    onOpenItem: (Long) -> Unit,
    onOpenRecording: (String) -> Unit,
    onMoreLibrary: () -> Unit = {},
    onMoreAttention: () -> Unit = {},
    onMoreUpcoming: () -> Unit = {},
    onStop: (String) -> Unit,
    onRestore: (String) -> Unit,
    onRuleEnabled: (String, Boolean) -> Unit,
    onRuleNewOnly: (String, Boolean) -> Unit,
    onRuleDelete: (String) -> Unit,
    onRuleMove: (String, Int) -> Unit,
    onForgetReminder: (String) -> Unit = {},
    onManual: (String, Long, Long, String) -> Unit = { _, _, _, _ -> },
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    Column(modifier) {
        Row(
            Modifier.fillMaxWidth().heightIn(min = 36.dp).padding(horizontal = 12.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            DvrChip.entries.forEach { choice ->
                val label = if (choice == DvrChip.Upcoming && dvr.conflicts > 0) {
                    "${choice.label} · ${dvr.conflicts}"
                } else {
                    choice.label
                }
                TvTextButton(onClick = { onChip(choice) }, compact = true) {
                    Text(
                        label,
                        style = type.primary,
                        color = if (choice == chip) {
                            MaterialTheme.colorScheme.primary
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            }
            Spacer(Modifier.weight(1f))
            Text(
                dvrStatusLine(dvr),
                style = type.tertiary,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        dvr.message.takeIf { it.isNotEmpty() }?.let {
            Text(
                it,
                style = type.secondary,
                modifier = Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 2.dp),
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
        when (chip) {
            DvrChip.Saved -> DvrLibraryList(
                dvr, now, onOpenItem, onOpenRecording, onStop, onMoreLibrary, Modifier.weight(1f),
            )
            DvrChip.Upcoming -> DvrScheduleList(
                dvr.schedule.filter { it.state in setOf("scheduled", "conflict", "withdrawn", "stale") }, onOpenRecording,
                onStop, onRestore, Modifier.weight(1f),
                hasMore = dvr.scheduleNext != null,
                onMore = onMoreUpcoming,
            )
            DvrChip.Attention -> DvrScheduleList(
                dvr.attention.map { it.recording }, onOpenRecording,
                onStop, onRestore, Modifier.weight(1f),
                hasMore = dvr.attentionNext != null,
                onMore = onMoreAttention,
            )
            DvrChip.Rules -> DvrRulesList(
                dvr, onRuleEnabled, onRuleNewOnly, onRuleDelete, onRuleMove, Modifier.weight(1f),
            )
            DvrChip.Manual -> DvrManualRecording(onManual, Modifier.weight(1f))
            DvrChip.Skipped -> DvrScheduleList(
                dvr.schedule.filter { it.cancelled }, onOpenRecording,
                onStop, onRestore, Modifier.weight(1f),
            )
            DvrChip.Reminders -> DvrReminderList(dvr, onForgetReminder, Modifier.weight(1f))
        }
    }
}

internal fun dvrStatusLine(dvr: DvrScreenState): String {
    val status = dvr.status ?: return "Recording status unavailable"
    if (!status.enabled) return "Recording is switched off"
    val free = status.free_bytes
    return buildString {
        append("${status.slots.recording} of ${status.slots.max} tuners recording")
        if (free != null) append(" · ${free / 1_000_000_000} GB free")
        status.next_start?.let { append(" · next ${liveTvTime(it)}") }
    }
}

@Composable
private fun DvrLibraryList(
    dvr: DvrScreenState,
    now: Long,
    onOpenItem: (Long) -> Unit,
    onOpenRecording: (String) -> Unit,
    onStop: (String) -> Unit,
    onMore: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    if (dvr.library.isEmpty()) {
        DvrEmpty("Nothing recorded yet.", modifier)
        return
    }
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(dvr.library, key = { it.id }) { row ->
            DvrRow(
                title = row.title,
                // A finished capture opens exactly where every other item in
                // the library opens; a recording has no item yet, because the
                // scan has not seen a file that is still being written.
                detail = dvrRecordingDetail(row, now),
                onClick = { onOpenRecording(row.id) },
            ) {
                row.item_id?.takeIf { row.hasMedia }?.let { id ->
                    TvTextButton(onClick = { onOpenItem(id) }, compact = true) {
                        Text("Play", style = type.primary)
                    }
                }
                if (row.recording) {
                    TvTextButton(onClick = { onStop(row.id) }, compact = true) {
                        Text("Stop", style = type.primary)
                    }
                }
            }
        }
        if (dvr.libraryNext != null) {
            item(key = "load-more") {
                TvTextButton(onClick = onMore) { Text("Load more") }
            }
        }
    }
}

@Composable
private fun DvrScheduleList(
    rows: List<DvrRecording>,
    onOpenRecording: (String) -> Unit,
    onStop: (String) -> Unit,
    onRestore: (String) -> Unit,
    modifier: Modifier = Modifier,
    hasMore: Boolean = false,
    onMore: () -> Unit = {},
) {
    val type = LiveTvTypography.current()
    val sorted = remember(rows) { rows.sortedWith(compareBy<DvrRecording> { it.capture_start }.thenBy { it.id }) }
    if (sorted.isEmpty()) {
        DvrEmpty("Nothing scheduled.", modifier)
        return
    }
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(sorted, key = { it.id }) { row ->
            DvrRow(
                title = row.title,
                detail = listOfNotNull(
                    "${row.guide_number} · ${liveTvTime(row.airing_start)}",
                    row.state.replaceFirstChar { it.uppercase() },
                    // A conflict is the one state where the reason is the whole
                    // point: "no tuner free" without which recording took it is
                    // not something a viewer can act on.
                    row.state_reason.takeIf { row.state != "scheduled" },
                ).joinToString(" · "),
                onClick = { onOpenRecording(row.id) },
            ) {
                when {
                    row.cancelled -> TvTextButton(
                        onClick = { onRestore(row.id) },
                        compact = true,
                    ) { Text("Restore", style = type.primary) }
                    row.conflict -> TvTextButton(
                        onClick = { onStop(row.id) },
                        compact = true,
                    ) { Text("Skip", style = type.primary) }
                    row.recording -> TvTextButton(
                        onClick = { onStop(row.id) },
                        compact = true,
                    ) { Text("Stop", style = type.primary) }
                    else -> TvTextButton(
                        onClick = { onStop(row.id) },
                        compact = true,
                    ) { Text("Cancel", style = type.primary) }
                }
            }
        }
        if (hasMore) {
            item(key = "load-more") {
                TvTextButton(onClick = onMore) { Text("Load more") }
            }
        }
    }
}

@Composable
private fun DvrRulesList(
    dvr: DvrScreenState,
    onEnabled: (String, Boolean) -> Unit,
    onNewOnly: (String, Boolean) -> Unit,
    onDelete: (String) -> Unit,
    onMove: (String, Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    if (dvr.rules.isEmpty()) {
        DvrEmpty("No series rules. Record series on a guide cell makes one.", modifier)
        return
    }
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(dvr.rules, key = { it.id }) { rule ->
            var menuOpen by remember(rule.id) { mutableStateOf(false) }
            DvrRow(
                title = rule.name,
                detail = listOf(
                    rule.matchSummary,
                    if (rule.new_only) "New episodes only" else "Every episode",
                    if (rule.enabled) "Enabled" else "Disabled",
                ).joinToString(" · "),
                onClick = null,
            ) {
                Box {
                    TvTextButton(onClick = { menuOpen = true }, compact = true) {
                        Text("Edit", style = type.primary)
                    }
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        DropdownMenuItem(
                            text = { Text(if (rule.enabled) "Disable" else "Enable") },
                            onClick = { menuOpen = false; onEnabled(rule.id, !rule.enabled) },
                        )
                        DropdownMenuItem(
                            text = {
                                Text(if (rule.new_only) "Record every episode" else "New episodes only")
                            },
                            onClick = { menuOpen = false; onNewOnly(rule.id, !rule.new_only) },
                        )
                        // Priority is a per-server order, so an ordinary viewer
                        // sees the refusal rather than a hidden control: what
                        // they cannot do is worth knowing.
                        DropdownMenuItem(
                            text = { Text("Higher priority") },
                            onClick = { menuOpen = false; onMove(rule.id, -1) },
                        )
                        DropdownMenuItem(
                            text = { Text("Lower priority") },
                            onClick = { menuOpen = false; onMove(rule.id, 1) },
                        )
                        DropdownMenuItem(
                            text = { Text("Delete rule") },
                            onClick = { menuOpen = false; onDelete(rule.id) },
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun DvrManualRecording(
    onSchedule: (String, Long, Long, String) -> Unit,
    modifier: Modifier = Modifier,
) {
    var channel by remember { mutableStateOf("") }
    var title by remember { mutableStateOf("") }
    var delayMinutes by remember { mutableStateOf(5) }
    var durationMinutes by remember { mutableStateOf(60) }
    Column(modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Record one channel for an exact time range, even beyond the guide horizon.")
        OutlinedTextField(
            value = channel,
            onValueChange = { channel = it },
            label = { Text("Channel ID") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        OutlinedTextField(
            value = title,
            onValueChange = { title = it },
            label = { Text("Title (optional)") },
            singleLine = true,
            modifier = Modifier.fillMaxWidth(),
        )
        ChoicePicker("Starts", delayMinutes, listOf(0, 5, 15),
            { if (it == 0) "Now" else "In $it minutes" }, { delayMinutes = it })
        ChoicePicker("Duration", durationMinutes, listOf(30, 60, 90, 120),
            { "$it minutes" }, { durationMinutes = it })
        TvTextButton(onClick = {
            val start = System.currentTimeMillis() / 1_000 + delayMinutes * 60
            onSchedule(channel.trim(), start, start + durationMinutes * 60, title.trim())
        }, enabled = channel.isNotBlank()) { Text("Schedule recording") }
    }
}

@Composable
private fun DvrReminderList(
    dvr: DvrScreenState,
    onForget: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    if (dvr.reminders.isEmpty()) {
        DvrEmpty("No reminders. Add one from a guide airing.", modifier)
        return
    }
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(dvr.reminders, key = { it.id }) { reminder ->
            DvrRow(
                title = reminder.title,
                detail = "${reminder.guide_number} · ${liveTvTime(reminder.airing_start)} · ${reminder.state}",
                onClick = null,
            ) {
                TvTextButton(onClick = { onForget(reminder.id) }, compact = true) { Text("Remove") }
            }
        }
    }
}

/** Permanent root destination; Live TV embeds the same panel and callbacks. */
@Composable
fun DvrRecordingsScreen(
    controller: DvrController?,
    onOpenItem: (Long) -> Unit,
    onOpenRecording: (String) -> Unit,
    onOpenActivity: () -> Unit,
    onBack: () -> Unit,
) {
    if (controller == null) {
        DvrUnavailable(onBack)
        return
    }
    val state by controller.state.collectAsStateWithLifecycle()
    var chip by remember { mutableStateOf(DvrChip.Upcoming) }
    var stopRecordingFor by remember { mutableStateOf<Pair<String, String>?>(null) }
    var deleteFileFor by remember { mutableStateOf<String?>(null) }
    var now by remember { mutableStateOf(System.currentTimeMillis() / 1_000) }
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    DisposableEffect(controller) {
        controller.setHighFrequency("recordings", true)
        onDispose { controller.setHighFrequency("recordings", false) }
    }
    LaunchedEffect(controller) {
        controller.load()
        controller.refreshMarks()
        controller.refreshLibrary()
        controller.refreshRules()
        controller.refreshAttention()
        while (true) { delay(30_000); now = System.currentTimeMillis() / 1_000 }
    }
    LaunchedEffect(chip) {
        when (chip) {
            DvrChip.Saved -> controller.refreshLibrary()
            DvrChip.Attention -> controller.refreshAttention()
            DvrChip.Rules -> controller.refreshRules()
            else -> controller.refreshMarks()
        }
    }
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TvTextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text("Recordings", style = MaterialTheme.typography.headlineMedium, modifier = Modifier.weight(1f))
            state.indicatorLabel()?.let { label ->
                TvTextButton(onClick = onOpenActivity) { Text("● $label") }
            }
        }
        if (state.overviewIsFresh() && state.indicatorLabel() == null) {
            Text("No active recordings", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        state.overviewError?.takeIf { state.overview != null }?.let {
            Text("Showing the last recording status · $it", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        DvrRecordingsPanel(
            dvr = state,
            now = now,
            chip = chip,
            onChip = { chip = it },
            onOpenItem = onOpenItem,
            onOpenRecording = onOpenRecording,
            onMoreLibrary = { controller.refreshLibrary(more = true) },
            onMoreAttention = { controller.refreshAttention(more = true) },
            onMoreUpcoming = { controller.refreshSchedule(more = true) },
            onStop = { id ->
                val row = (state.schedule + state.library).firstOrNull { it.id == id }
                val activeTitle = row?.takeIf { it.recording }?.title
                    ?: state.overview?.active?.firstOrNull { it.recording_id == id }?.title
                if (activeTitle != null) stopRecordingFor = id to activeTitle
                else controller.stop(id, onConfirmFile = { deleteFileFor = it })
            },
            onRestore = controller::restore,
            onRuleEnabled = controller::setRuleEnabled,
            onRuleNewOnly = controller::setRuleNewOnly,
            onRuleDelete = controller::deleteRule,
            onRuleMove = controller::moveRule,
            onForgetReminder = controller::forgetReminder,
            onManual = controller::recordManual,
            modifier = Modifier.weight(1f),
        )
    }
    stopRecordingFor?.let { (id, title) ->
        AlertDialog(
            onDismissRequest = { stopRecordingFor = null },
            title = { Text("Stop recording $title?") },
            text = { Text("Any captured portion will be kept. Watching continues.") },
            confirmButton = {
                TextButton(onClick = {
                    stopRecordingFor = null
                    controller.stop(id, onConfirmFile = { deleteFileFor = it })
                }) { Text("Stop recording") }
            },
            dismissButton = {
                TextButton(onClick = { stopRecordingFor = null }) { Text("Keep recording") }
            },
        )
    }
    deleteFileFor?.let { id ->
        AlertDialog(
            onDismissRequest = { deleteFileFor = null },
            title = { Text("Delete this recording?") },
            text = { Text("The recording and its file are removed. This cannot be undone.") },
            confirmButton = {
                TextButton(onClick = {
                    deleteFileFor = null
                    controller.stop(id, deleteFile = true)
                }) { Text("Delete file") }
            },
            dismissButton = {
                TextButton(onClick = { deleteFileFor = null }) { Text("Keep") }
            },
        )
    }
}

@Composable
fun DvrCaptureActivityScreen(
    controller: DvrController?,
    onOpenRecording: (String) -> Unit,
    onBack: () -> Unit,
) {
    if (controller == null) { DvrUnavailable(onBack); return }
    val state by controller.state.collectAsStateWithLifecycle()
    var now by remember { mutableStateOf(System.currentTimeMillis() / 1_000) }
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    DisposableEffect(controller) {
        controller.setHighFrequency("recording-activity", true)
        onDispose { controller.setHighFrequency("recording-activity", false) }
    }
    LaunchedEffect(Unit) { while (true) { delay(5_000); now = System.currentTimeMillis() / 1_000 } }
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TvTextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text(state.indicatorLabel() ?: if (state.overviewIsFresh()) "No active recordings" else "Status unavailable",
                style = MaterialTheme.typography.headlineMedium)
        }
        state.overview?.observation_age_ms?.let {
            Text("DVR observed ${it / 1_000} seconds ago", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        LazyColumn(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(state.overview?.active.orEmpty(), key = { it.recording_id }) { row ->
                DvrRow(
                    title = row.title,
                    detail = buildList {
                        add(row.display_state)
                        if (row.display_detail.isNotEmpty()) add(row.display_detail)
                        row.total_bytes_written?.let { add("${it / 1_000_000} MB written") }
                        row.observation?.last_write_age_ms?.let { add("data written ${it / 1_000} s ago") }
                    }.joinToString(" · "),
                    onClick = { onOpenRecording(row.recording_id) },
                ) {
                    LinearProgressIndicator(progress = { row.captureProgress(now) }, Modifier.fillMaxWidth(0.22f))
                }
            }
        }
    }
}

@Composable
fun DvrRecordingDetailScreen(
    controller: DvrController?,
    recordingId: String,
    onOpenItem: (Long) -> Unit,
    onBack: () -> Unit,
) {
    if (controller == null) { DvrUnavailable(onBack); return }
    val state by controller.state.collectAsStateWithLifecycle()
    val active = state.overview?.active?.any { it.recording_id == recordingId } == true
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    LaunchedEffect(recordingId) { controller.selectRecording(recordingId) }
    LaunchedEffect(recordingId, active) {
        while (active) { delay(5_000); controller.refreshSelectedEvents() }
    }
    Column(Modifier.fillMaxSize().padding(16.dp)) {
        Row(verticalAlignment = Alignment.CenterVertically) {
            TvTextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text("Recording details", style = MaterialTheme.typography.headlineMedium)
        }
        val row = state.selectedRecording?.takeIf { it.id == recordingId }
        if (row == null) {
            Text("Loading recording…")
        } else {
            Text(row.title, style = MaterialTheme.typography.titleLarge)
            Text(
                "${row.guide_number} ${row.channel_name} · Programme ${liveTvTime(row.airing_start)}–${liveTvTime(row.airing_end)} · Capture ${liveTvTime(row.capture_start)}–${liveTvTime(row.capture_end)} · ${row.state}",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                row.item_id?.takeIf { row.hasMedia }?.let { id ->
                    TvTextButton(onClick = { onOpenItem(id) }) { Text("Play") }
                }
                when {
                    row.recording -> TvTextButton(onClick = { controller.stop(row.id) }) { Text("Stop") }
                    row.cancelled -> TvTextButton(onClick = { controller.restore(row.id) }) { Text("Restore") }
                }
                if (state.attention.any { it.recording.id == row.id }) {
                    TvTextButton(onClick = controller::acknowledgeSelectedAttention) { Text("Mark reviewed") }
                }
            }
            Text("History", style = MaterialTheme.typography.titleMedium)
            if (!state.selectedHistoryComplete) {
                Text(
                    if (state.selectedHistoryTruncatedBefore == null)
                        "Detailed history was not collected for the whole recording."
                    else "Older history expired under the retention limit.",
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            LazyColumn(Modifier.weight(1f)) {
                items(state.selectedEvents, key = { it.event_id }) { event ->
                    DvrRow(
                        title = event.kind.replace('_', ' ').replaceFirstChar { it.uppercase() },
                        detail = listOfNotNull("sequence ${event.sequence}", event.attempt?.let { "attempt $it" },
                            event.reason_code?.replace('_', ' ')).joinToString(" · "),
                        onClick = null,
                    ) {}
                }
                if (state.selectedEventsNext != null) {
                    item { TvTextButton(onClick = controller::loadOlderSelectedEvents) { Text("Load older history") } }
                }
            }
        }
    }
}

@Composable
private fun DvrUnavailable(onBack: () -> Unit) {
    Column(Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TvTextButton(onClick = onBack) { Text("Back") }
        Text("Recording status unavailable", style = MaterialTheme.typography.headlineMedium)
        Text("Reconnect this profile and try again.")
    }
}

/** One 44 dp row: what it is, what it is doing, and the one thing to do to it. */
@Composable
private fun DvrRow(
    title: String,
    detail: String,
    onClick: (() -> Unit)?,
    action: @Composable () -> Unit,
) {
    val type = LiveTvTypography.current()
    Row(
        Modifier
            .fillMaxWidth()
            .heightIn(min = 44.dp)
            .then(if (onClick == null) Modifier else Modifier.clickable(onClick = onClick))
            .padding(horizontal = 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text(title, style = type.primary, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text(
                detail,
                style = type.tertiary,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        action()
    }
}

@Composable
private fun DvrEmpty(message: String, modifier: Modifier = Modifier) {
    Box(modifier.fillMaxWidth(), contentAlignment = Alignment.Center) {
        Text(
            message,
            style = LiveTvTypography.current().secondary,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * What a library row says under its title. A partial capture never presents as
 * a clean one: the gap and the late start are the whole difference between
 * `done` and `partial`, and hiding them would make the two states the same
 * word to a viewer.
 */
internal fun dvrRecordingDetail(row: DvrRecording, now: Long): String = buildString {
    append("${row.guide_number} · ${liveTvTime(row.airing_start)}")
    when {
        row.recording -> {
            append(" · Recording")
            val left = (row.capture_end - now) / 60
            if (left > 0) append(" · $left min left")
        }
        row.state == "partial" -> {
            append(" · Partial")
            if (row.stopped_by_user_id != null) append(" · Stopped early")
            if (row.gap_s > 0) append(" · ${row.gap_s / 60} min gap")
            if (row.late_start_s > 0) append(" · started ${row.late_start_s} s late")
        }
        row.state == "done" -> {
            append(if (row.stopped_by_user_id != null) " · Stopped early" else " · Recorded")
            if (row.bytes > 0) append(" · ${row.bytes / 1_000_000} MB")
            if (row.item_id == null || row.file_id == null) append(" · preparing playback")
        }
        else -> {
            append(" · ${row.state.replaceFirstChar { it.uppercase() }}")
            row.state_reason?.let { append(" · $it") }
        }
    }
}
