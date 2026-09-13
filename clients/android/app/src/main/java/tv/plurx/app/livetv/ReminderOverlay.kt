package tv.plurx.app.livetv

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.widthIn
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.TvTextButton

/**
 * "Kitchen Table starts in four minutes."
 *
 * Three buttons and a bar, lower-left, over whatever is on screen. It is
 * **buttons only** — no key handler, no focus trap, no change to the Live TV
 * input table: on a television the three are ordinary focusables and Back
 * dismisses through the platform, exactly as it does for every other panel.
 * Adding a keydown listener here would put a second author on a contract
 * `LiveTvInputPolicy` exists to keep single.
 *
 * It goes away on its own when the programme starts; a viewer who presses
 * *Dismiss* acks it instead, and no other device shows it again.
 */
@Composable
fun ReminderOverlay(
    reminder: DvrReminder,
    now: Long,
    onWatch: () -> Unit,
    onRecord: () -> Unit,
    onDismiss: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    Column(
        modifier
            .widthIn(max = 340.dp)
            .background(MaterialTheme.colorScheme.surface, MaterialTheme.shapes.medium)
            .padding(12.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Text(
            "STARTING SOON · ${reminder.guide_number}",
            style = type.eyebrow,
            color = MaterialTheme.colorScheme.primary,
        )
        Text(
            reminder.title,
            style = type.primary,
            maxLines = 2,
            overflow = TextOverflow.Ellipsis,
        )
        Text(
            reminderCountdown(reminder, now),
            style = type.secondary,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
        )
        LinearProgressIndicator(
            progress = { reminderProgress(reminder, now) },
            modifier = Modifier.fillMaxWidth().height(3.dp),
        )
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            TvTextButton(onClick = onWatch, compact = true) { Text("Watch", style = type.primary) }
            if (reminder.covered_by_recording) {
                // The server already knows a recording covers this airing, so
                // offering to start one would be offering a second row for one
                // programme — which the server would answer with the first.
                Text(
                    "Recording",
                    style = type.secondary,
                    color = MaterialTheme.colorScheme.primary,
                    modifier = Modifier.padding(horizontal = 8.dp, vertical = 6.dp),
                )
            } else {
                TvTextButton(onClick = onRecord, compact = true) {
                    Text("Record", style = type.primary)
                }
            }
            TvTextButton(onClick = onDismiss, compact = true) {
                Text("Dismiss", style = type.primary)
            }
        }
    }
}

/**
 * How far through the lead the reminder is, from the moment it was meant to
 * fire to the moment the programme starts. A reminder with no lead at all is
 * already at its start, so it reads full rather than dividing by zero.
 */
internal fun reminderProgress(reminder: DvrReminder, now: Long): Float {
    val lead = reminder.airing_start - reminder.fireAt
    if (lead <= 0) return 1f
    return ((now - reminder.fireAt).toFloat() / lead).coerceIn(0f, 1f)
}

internal fun reminderCountdown(reminder: DvrReminder, now: Long): String {
    val seconds = reminder.airing_start - now
    return when {
        seconds <= 0 -> "Starting now on ${reminder.guide_number}"
        seconds < 60 -> "Starts in under a minute · ${liveTvTime(reminder.airing_start)}"
        else -> "Starts in ${seconds / 60} min · ${liveTvTime(reminder.airing_start)}"
    }
}
