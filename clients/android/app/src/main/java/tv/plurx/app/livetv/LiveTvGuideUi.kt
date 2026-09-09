@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import androidx.compose.foundation.background
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.TvButton
import tv.plurx.app.ui.components.TvTextButton
import java.text.SimpleDateFormat
import java.util.Date
import java.util.Locale

/**
 * How the browse region is drawn. Persisted, because the choice between
 * surfing a list and planning off a grid is a habit rather than a mode.
 */
enum class LiveTvBrowseView(val storage: String, val label: String) {
    List("list", "On now"),
    Guide("guide", "Guide"),
    ;

    companion object {
        fun fromStorage(value: String?): LiveTvBrowseView =
            entries.firstOrNull { it.storage == value } ?: List
    }
}

/** Half-hour geometry, shared by the grid composable and its tests. */
object LiveTvGridMetrics {
    const val SLOT_SECONDS: Long = LiveTvGuideReducer.SLOT_SECONDS
    val slotWidth: Dp = 160.dp
    val rowHeight: Dp = 56.dp
    val channelColumnWidth: Dp = 132.dp
    /** The grid's now rule. Red because every guide's is. */
    val nowLine: Color = Color(0xFFE23A2E)
}

private val liveTvClock = SimpleDateFormat("h:mm a", Locale.getDefault())

fun liveTvTime(unixSeconds: Long): String = liveTvClock.format(Date(unixSeconds * 1000))

/** Small lineup facts that fit both a list row and the grid's channel column. */
@Composable
fun LiveTvFormatBadges(channel: LiveTvChannel) {
    if (channel.formatBadges.isEmpty()) return
    Row(
        horizontalArrangement = Arrangement.spacedBy(4.dp),
        modifier = Modifier.semantics {
            contentDescription = "Source format: ${channel.formatBadges.joinToString(", ")}"
        },
    ) {
        channel.formatBadges.forEach { badge ->
            Text(
                badge,
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier
                    .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(12.dp))
                    .padding(horizontal = 5.dp, vertical = 1.dp),
            )
        }
    }
}

/**
 * One channel row: chip, number and callsign, what is on with a bar to its
 * end, and what is next. A protected channel is dimmed, never hidden.
 */
@Composable
fun LiveTvChannelRow(
    channel: LiveTvChannel,
    airing: LiveTvAiring,
    selected: Boolean,
    onWatch: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxWidth()
            .padding(vertical = 8.dp)
            .semantics {
                contentDescription = buildString {
                    append(channel.title)
                    airing.now?.let { append(", ${it.title}") }
                    if (!channel.watchable) append(", protected channel")
                }
            },
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
            Text(channel.title, style = MaterialTheme.typography.titleSmall)
            LiveTvFormatBadges(channel)
        }
        when {
            !channel.watchable -> Text(
                "Protected channel · not playable",
                style = MaterialTheme.typography.labelSmall,
            )
            airing.now != null -> {
                Text(
                    airing.now.title,
                    style = MaterialTheme.typography.bodyMedium,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                LinearProgressIndicator(
                    progress = { airing.progress ?: 0f },
                    modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
                )
                Text(
                    liveTvTime(airing.now.end) +
                        (airing.next?.let { " · Next: ${it.title}" } ?: ""),
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
            }
            else -> Text("No programme information", style = MaterialTheme.typography.labelSmall)
        }
        if (channel.favorite) Text("Favorite", style = MaterialTheme.typography.labelSmall)
        TvButton(onClick = onWatch, enabled = channel.watchable) {
            Text(
                when {
                    !channel.watchable -> "DRM unsupported"
                    selected -> "Watching"
                    else -> "Watch live"
                },
            )
        }
    }
}

/**
 * The half-hour grid. One horizontal scroll state is shared by every row and
 * by the time header, so the channel column and the cells cannot drift apart.
 */
@Composable
fun LiveTvGuideGrid(
    layout: LiveTvGridLayout,
    slots: List<Long>,
    playingChannelId: String?,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    modifier: Modifier = Modifier,
) {
    val scroll = rememberScrollState()
    Column(modifier) {
        Row {
            Box(Modifier.width(LiveTvGridMetrics.channelColumnWidth))
            Row(Modifier.horizontalScroll(scroll)) {
                slots.forEach { at ->
                    Text(
                        liveTvTime(at),
                        style = MaterialTheme.typography.labelSmall,
                        modifier = Modifier.width(LiveTvGridMetrics.slotWidth),
                    )
                }
            }
        }
        // The red now line. It was computed by the reducer and drawn by nobody,
        // while ANDROID-CLIENT-PARITY.md said it shipped — so the grid gave a
        // viewer no way to tell where the present was in a four-hour window.
        Box(Modifier.weight(1f)) {
        LazyColumn {
            items(layout.rows, key = { it.channel.id }) { row ->
                Row(Modifier.height(LiveTvGridMetrics.rowHeight)) {
                    Column(Modifier.width(LiveTvGridMetrics.channelColumnWidth)) {
                        Text(row.channel.guide_number, style = MaterialTheme.typography.labelMedium)
                        Text(row.channel.guide_name, style = MaterialTheme.typography.labelSmall)
                        LiveTvFormatBadges(row.channel)
                    }
                    Box(Modifier.horizontalScroll(scroll)) {
                        // An empty row is a channel the guide has no data for,
                        // not a channel that went away.
                        Box(
                            Modifier
                                .width(LiveTvGridMetrics.slotWidth * slots.size)
                                .height(LiveTvGridMetrics.rowHeight),
                        )
                        // `left` and `width` arrive already in Dp units: the
                        // caller hands the reducer `slotWidth.value` as its
                        // pixels-per-slot, so no second conversion is needed —
                        // and a second one is exactly how a grid drifts out of
                        // step with its own time header.
                        row.cells.forEach { cell ->
                            LiveTvGridCellButton(
                                cell = cell,
                                channel = row.channel,
                                playing = playingChannelId == row.channel.id && cell.airing,
                                onAiring = onAiring,
                                onFuture = onFuture,
                            )
                        }
                    }
                }
            }
        }
        layout.nowX?.let { x ->
            val offset = LiveTvGridMetrics.channelColumnWidth + x.dp - scroll.value.dp
            if (offset >= LiveTvGridMetrics.channelColumnWidth) {
                Box(
                    Modifier
                        .offset(x = offset)
                        .fillMaxHeight()
                        .width(2.dp)
                        .background(LiveTvGridMetrics.nowLine),
                )
            }
        }
        }
    }
}

@Composable
private fun LiveTvGridCellButton(
    cell: LiveTvGridCell,
    channel: LiveTvChannel,
    playing: Boolean,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
) {
    TvTextButton(
        onClick = { if (cell.airing) onAiring(channel) else onFuture(channel, cell.programme) },
        modifier = Modifier
            .offset(x = cell.left.dp)
            .width((cell.width - 4f).coerceAtLeast(28f).dp)
            .background(if (playing) Color(0x33FFFFFF) else Color.Transparent),
    ) {
        Text(
            cell.programme.title,
            style = MaterialTheme.typography.labelSmall,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}
