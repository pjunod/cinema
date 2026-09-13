package tv.plurx.app.livetv

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.TvTextButton

/**
 * The three lists behind the Recordings segment. Deliberately not persisted,
 * unlike the browse view: which of the three was last open is a step in one
 * errand, not a habit worth reopening the screen on.
 */
enum class DvrChip(val label: String) {
    Library("Library"),
    Scheduled("Scheduled"),
    Rules("Rules"),
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
    onStop: (String) -> Unit,
    onRestore: (String) -> Unit,
    onRuleEnabled: (String, Boolean) -> Unit,
    onRuleNewOnly: (String, Boolean) -> Unit,
    onRuleDelete: (String) -> Unit,
    onRuleMove: (String, Int) -> Unit,
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
                val label = if (choice == DvrChip.Scheduled && dvr.conflicts > 0) {
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
            DvrChip.Library -> DvrLibraryList(dvr, now, onOpenItem, onStop, Modifier.weight(1f))
            DvrChip.Scheduled -> DvrScheduleList(dvr, onStop, onRestore, Modifier.weight(1f))
            DvrChip.Rules -> DvrRulesList(
                dvr, onRuleEnabled, onRuleNewOnly, onRuleDelete, onRuleMove, Modifier.weight(1f),
            )
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
    onStop: (String) -> Unit,
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
                onClick = row.item_id?.takeIf { row.hasMedia }?.let { id -> { onOpenItem(id) } },
            ) {
                if (row.recording) {
                    TvTextButton(onClick = { onStop(row.id) }, compact = true) {
                        Text("Stop", style = type.primary)
                    }
                }
            }
        }
    }
}

@Composable
private fun DvrScheduleList(
    dvr: DvrScreenState,
    onStop: (String) -> Unit,
    onRestore: (String) -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    val rows = remember(dvr.schedule) { dvr.schedule.sortedBy { it.capture_start } }
    if (rows.isEmpty()) {
        DvrEmpty("Nothing scheduled.", modifier)
        return
    }
    LazyColumn(modifier, verticalArrangement = Arrangement.spacedBy(2.dp)) {
        items(rows, key = { it.id }) { row ->
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
                onClick = null,
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
            if (row.gap_s > 0) append(" · ${row.gap_s / 60} min gap")
            if (row.late_start_s > 0) append(" · started ${row.late_start_s} s late")
        }
        row.state == "done" -> append(" · ${row.bytes / 1_000_000} MB")
        else -> {
            append(" · ${row.state.replaceFirstChar { it.uppercase() }}")
            row.state_reason?.let { append(" · $it") }
        }
    }
}
