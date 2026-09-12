@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateMapOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import tv.plurx.app.ui.FormFactor
import tv.plurx.app.ui.components.TvTextButton
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.currentFormFactor
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

/**
 * One grid's drawn geometry. Passed in rather than read from the object, so
 * the slot width can follow the screen on a television without the phone's
 * fixed columns moving.
 */
data class LiveTvGridDimensions(
    val slotWidth: Dp,
    val rowHeight: Dp,
    val channelColumnWidth: Dp,
)

/** Half-hour geometry, shared by the grid composable and its tests. */
object LiveTvGridMetrics {
    const val SLOT_SECONDS: Long = LiveTvGuideReducer.SLOT_SECONDS
    val slotWidth: Dp = 160.dp
    val rowHeight: Dp = 56.dp
    val channelColumnWidth: Dp = 132.dp
    /** The grid's now rule. Red because every guide's is. */
    val nowLine: Color = Color(0xFFE23A2E)

    /** Two hours on screen. The paging chips move by exactly this much. */
    const val TELEVISION_VISIBLE_SLOTS: Int = 4
    val televisionChannelColumnWidth: Dp = 100.dp
    val televisionRowHeight: Dp = 37.dp

    /** The phone scrolls horizontally, so its columns are a fixed size. */
    val phone: LiveTvGridDimensions =
        LiveTvGridDimensions(slotWidth, rowHeight, channelColumnWidth)

    /**
     * Slot width follows the screen. One shared 160 dp column meant 320 px on
     * a television — the grid showed barely an hour and the 56 dp rows drew
     * 112 px tall, so six channels filled the screen.
     */
    fun forTelevision(contentWidth: Dp): LiveTvGridDimensions = LiveTvGridDimensions(
        slotWidth = ((contentWidth - televisionChannelColumnWidth) / TELEVISION_VISIBLE_SLOTS)
            .coerceAtLeast(60.dp),
        rowHeight = televisionRowHeight,
        channelColumnWidth = televisionChannelColumnWidth,
    )
}

/**
 * One explicit scale for the ten-foot surface. Material's semantic styles are
 * sized for a phone held at arm's length; on a television `titleLarge` is a
 * banner and `bodyMedium` a headline, which is why the rows were three feet
 * long. The phone keeps exactly the styles it drew before.
 */
data class LiveTvTypeScale(
    val title: TextStyle,
    val primary: TextStyle,
    val cell: TextStyle,
    val secondary: TextStyle,
    val tertiary: TextStyle,
    val badge: TextStyle,
    val eyebrow: TextStyle,
)

object LiveTvTypography {
    val television = LiveTvTypeScale(
        title = TextStyle(fontSize = 15.sp, fontWeight = FontWeight.SemiBold),
        primary = TextStyle(fontSize = 11.sp, fontWeight = FontWeight.SemiBold),
        cell = TextStyle(fontSize = 11.sp),
        secondary = TextStyle(fontSize = 10.sp),
        tertiary = TextStyle(fontSize = 9.sp),
        badge = TextStyle(fontSize = 8.sp, fontWeight = FontWeight.Bold),
        eyebrow = TextStyle(fontSize = 7.sp, fontWeight = FontWeight.Bold),
    )

    @Composable
    fun current(): LiveTvTypeScale =
        if (currentFormFactor() == FormFactor.Television) television else phone()

    @Composable
    private fun phone(): LiveTvTypeScale {
        val typography = MaterialTheme.typography
        return remember(typography) { phoneScale(typography) }
    }

    private fun phoneScale(typography: androidx.compose.material3.Typography) = LiveTvTypeScale(
        title = typography.titleMedium,
        primary = typography.titleSmall,
        cell = typography.labelSmall,
        secondary = typography.bodyMedium,
        tertiary = typography.labelSmall,
        badge = typography.labelSmall,
        eyebrow = typography.labelSmall,
    )
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
                style = LiveTvTypography.current().badge,
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
 * end, and when it ends. A protected channel is dimmed, never hidden.
 *
 * A fixed three-column grid — chip · text · star — at a fixed 78 dp, so five
 * of them fit a phone under a playing picture. The old row was a
 * `fillMaxWidth` TextButton wrapping a five-line Column: two fit.
 */
@Composable
fun LiveTvChannelRow(
    channel: LiveTvChannel,
    airing: LiveTvAiring,
    selected: Boolean,
    onWatch: () -> Unit,
) {
    val type = LiveTvTypography.current()
    Row(
        Modifier
            .fillMaxWidth()
            .height(78.dp)
            .clickable(enabled = channel.watchable, onClick = onWatch)
            .tvFocusRing()
            .padding(horizontal = 10.dp)
            .semantics {
                contentDescription = buildString {
                    append(channel.title)
                    airing.now?.let { append(", ${it.title}") }
                    if (!channel.watchable) append(", protected channel")
                }
            },
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalAlignment = androidx.compose.ui.Alignment.CenterVertically,
    ) {
        Text(
            channel.guide_name.take(5),
            style = type.badge,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
            textAlign = androidx.compose.ui.text.style.TextAlign.Center,
            modifier = Modifier
                .size(width = 52.dp, height = 32.dp)
                .background(MaterialTheme.colorScheme.surfaceVariant, RoundedCornerShape(6.dp))
                .wrapContentHeight(),
        )
        Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(2.dp)) {
            Row(horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                Text(
                    channel.title,
                    style = type.primary,
                    maxLines = 1,
                    overflow = TextOverflow.Ellipsis,
                )
                if (selected) {
                    Text("● LIVE", style = type.eyebrow, color = MaterialTheme.colorScheme.primary)
                }
                LiveTvFormatBadges(channel)
            }
            when {
                !channel.watchable -> Text("Protected · not playable", style = type.secondary)
                airing.now != null -> {
                    Text(
                        airing.now.title,
                        style = type.secondary,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                    LinearProgressIndicator(
                        progress = { airing.progress ?: 0f },
                        modifier = Modifier.fillMaxWidth().height(3.dp),
                    )
                    Text(
                        listOfNotNull(
                            "until ${liveTvTime(airing.now.end)}",
                            airing.next?.let { "Next: ${it.title}" },
                        ).joinToString(" · "),
                        style = type.tertiary,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                else -> Text("No programme information", style = type.secondary)
            }
        }
        if (channel.favorite) {
            Text("★", style = type.primary, color = MaterialTheme.colorScheme.primary)
        }
    }
}

/** Earlier / Now / Later, drawn as chips inside the grid's own header. */
data class LiveTvGuidePaging(
    val canEarlier: Boolean,
    val canLater: Boolean,
    val onEarlier: () -> Unit,
    val onNow: () -> Unit,
    val onLater: () -> Unit,
)

@Composable
private fun LiveTvPagingChip(
    label: String,
    enabled: Boolean,
    onClick: () -> Unit,
    type: LiveTvTypeScale,
) {
    TvTextButton(onClick = onClick, enabled = enabled, compact = true) {
        Text(label, style = type.badge)
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
    dimensions: LiveTvGridDimensions = LiveTvGridMetrics.phone,
    paging: LiveTvGuidePaging? = null,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    dpadNavigation: Boolean = false,
    navigationTarget: LiveTvGuideFocusTarget? = null,
    navigationAnchorTime: Long? = null,
    onNavigationState: (LiveTvGuideFocusTarget, Long?) -> Unit = { _, _ -> },
    onFocus: (LiveTvChannel, LiveTvProgramme?) -> Unit = { _, _ -> },
    onToolbarBoundary: () -> Unit = {},
    modifier: Modifier = Modifier,
) {
    val scroll = rememberScrollState()
    val rows = rememberLazyListState()
    val requesters = remember { mutableStateMapOf<LiveTvGuideFocusTarget, FocusRequester>() }
    var focused by remember { mutableStateOf(navigationTarget) }
    var pending by remember { mutableStateOf<LiveTvGuideFocusTarget?>(null) }
    var preserveAnchorFor by remember { mutableStateOf<LiveTvGuideFocusTarget?>(null) }
    var anchorTime by remember { mutableStateOf(navigationAnchorTime) }
    val pendingRequester = pending?.let { requesters[it] }

    LaunchedEffect(dpadNavigation, navigationTarget, navigationAnchorTime) {
        val restored = navigationTarget ?: return@LaunchedEffect
        anchorTime = navigationAnchorTime
        if (dpadNavigation && focused != restored) {
            focused = restored
            pending = restored
            preserveAnchorFor = restored
        }
    }

    LaunchedEffect(layout) {
        val current = focused ?: return@LaunchedEffect
        val row = layout.rows.firstOrNull { it.channel.id == current.channelId }
        if (row == null) {
            focused = null
            pending = null
        } else if (current.programmeStart != null && row.cells.none { it.programme.start == current.programmeStart }) {
            val replacement = row.cells.firstOrNull {
                val at = anchorTime ?: current.programmeStart
                it.programme.start <= at && at < it.programme.end
            } ?: row.cells.minByOrNull {
                kotlin.math.abs((it.programme.start + it.programme.end) / 2 - (anchorTime ?: current.programmeStart))
            }
            pending = replacement?.let { LiveTvGuideFocusTarget(row.channel.id, it.programme.start) }
                ?: LiveTvGuideFocusTarget(row.channel.id)
            preserveAnchorFor = pending
            pending?.let { onNavigationState(it, anchorTime) }
        }
    }
    LaunchedEffect(pending, pendingRequester) {
        val target = pending ?: return@LaunchedEffect
        val rowIndex = layout.rows.indexOfFirst { it.channel.id == target.channelId }
        if (rowIndex < 0) {
            pending = null
            return@LaunchedEffect
        }
        if (pendingRequester == null) {
            rows.scrollToItem(rowIndex)
            return@LaunchedEffect
        }
        pendingRequester.requestFocus()
        pending = null
    }

    fun receiveFocus(target: LiveTvGuideFocusTarget, channel: LiveTvChannel) {
        focused = target
        if (preserveAnchorFor == target) {
            preserveAnchorFor = null
        } else if (target.programmeStart != null) {
            val cell = layout.rows.firstOrNull { it.channel.id == target.channelId }
                ?.cells?.firstOrNull { it.programme.start == target.programmeStart }
            anchorTime = cell?.programme?.let { it.start + (it.end - it.start).coerceAtLeast(1) / 2 }
        }
        val programme = target.programmeStart?.let { start ->
            layout.rows.firstOrNull { it.channel.id == target.channelId }
                ?.cells?.firstOrNull { it.programme.start == start }?.programme
        }
        onNavigationState(target, anchorTime)
        onFocus(channel, programme)
    }

    fun move(direction: LiveTvGuideFocusDirection) {
        val current = focused ?: return
        val outcome = LiveTvGuideReducer.moveGuideFocus(layout, current, anchorTime, direction)
        if (outcome.toolbarBoundary) {
            onToolbarBoundary()
            return
        }
        val target = outcome.target ?: return
        if (direction == LiveTvGuideFocusDirection.Up || direction == LiveTvGuideFocusDirection.Down) {
            preserveAnchorFor = target
        }
        anchorTime = outcome.anchorTime
        onNavigationState(target, anchorTime)
        if (target != current) pending = target
    }

    val type = LiveTvTypography.current()
    Column(modifier) {
        // 24 dp, not 17: `TvTextButton(compact = true)` has a 24 dp minimum
        // and 5 dp of vertical padding, so a 17 dp parent clipped the chips.
        Row(Modifier.heightIn(min = 24.dp)) {
            // Earlier / Now / Later live in the header's channel column
            // rather than as three more full-height buttons in the toolbar.
            Row(
                Modifier.width(dimensions.channelColumnWidth),
                horizontalArrangement = Arrangement.spacedBy(3.dp),
            ) {
                paging?.let {
                    LiveTvPagingChip("\u2039", it.canEarlier, it.onEarlier, type)
                    LiveTvPagingChip("Now", true, it.onNow, type)
                    LiveTvPagingChip("\u203a", it.canLater, it.onLater, type)
                }
            }
            Row(Modifier.horizontalScroll(scroll)) {
                slots.forEach { at ->
                    Text(
                        liveTvTime(at),
                        style = type.tertiary,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.width(dimensions.slotWidth).padding(start = 3.dp),
                    )
                }
            }
        }
        // The red now line. It was computed by the reducer and drawn by nobody,
        // while ANDROID-CLIENT-PARITY.md said it shipped — so the grid gave a
        // viewer no way to tell where the present was in a four-hour window.
        Box(Modifier.weight(1f)) {
        LazyColumn(state = rows) {
            items(layout.rows, key = { it.channel.id }) { row ->
                Row(Modifier.height(dimensions.rowHeight)) {
                    val channelTarget = LiveTvGuideFocusTarget(row.channel.id, channelHeader = true)
                    TvTextButton(
                        onClick = { if (row.channel.watchable) onAiring(row.channel) },
                        modifier = Modifier
                            .width(dimensions.channelColumnWidth)
                            .liveTvGuideFocusTarget(channelTarget, requesters) {
                                receiveFocus(channelTarget, row.channel)
                            }
                            .liveTvGuideNavigation(dpadNavigation, ::move),
                    ) {
                        Column {
                            Text(row.channel.guide_number, style = type.secondary)
                            Text(
                                row.channel.guide_name,
                                style = type.badge,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                            LiveTvFormatBadges(row.channel)
                        }
                    }
                    Box(Modifier.horizontalScroll(scroll)) {
                        // An empty row is a channel the guide has no data for,
                        // not a channel that went away.
                        Box(
                            Modifier
                                .width(dimensions.slotWidth * slots.size)
                                .height(dimensions.rowHeight),
                        )
                        // `left` and `width` arrive already in Dp units: the
                        // caller hands the reducer `slotWidth.value` as its
                        // pixels-per-slot, so no second conversion is needed —
                        // and a second one is exactly how a grid drifts out of
                        // step with its own time header.
                        row.cells.forEach { cell ->
                            val target = LiveTvGuideFocusTarget(row.channel.id, cell.programme.start)
                            LiveTvGridCellButton(
                                cell = cell,
                                channel = row.channel,
                                dimensions = dimensions,
                                playing = playingChannelId == row.channel.id && cell.airing,
                                onAiring = onAiring,
                                onFuture = onFuture,
                                modifier = Modifier
                                    .liveTvGuideFocusTarget(target, requesters) {
                                        receiveFocus(target, row.channel)
                                    }
                                    .liveTvGuideNavigation(dpadNavigation, ::move),
                            )
                        }
                        if (row.cells.isEmpty()) {
                            val emptyTarget = LiveTvGuideFocusTarget(row.channel.id)
                            TvTextButton(
                                onClick = { if (row.channel.watchable) onAiring(row.channel) },
                                modifier = Modifier
                                    .width(dimensions.slotWidth * slots.size)
                                    .height(dimensions.rowHeight)
                                    .liveTvGuideFocusTarget(emptyTarget, requesters) {
                                        receiveFocus(emptyTarget, row.channel)
                                    }
                                    .liveTvGuideNavigation(dpadNavigation, ::move),
                            ) {
                                Text(
                                    if (row.channel.watchable) "No programme information · Watch live"
                                    else "Protected channel · unavailable",
                                    style = type.cell,
                                )
                            }
                        }
                    }
                }
            }
        }
        layout.nowX?.let { x ->
            val offset = dimensions.channelColumnWidth + x.dp - scroll.value.dp
            if (offset >= dimensions.channelColumnWidth) {
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
    dimensions: LiveTvGridDimensions,
    playing: Boolean,
    onAiring: (LiveTvChannel) -> Unit,
    onFuture: (LiveTvChannel, LiveTvProgramme) -> Unit,
    modifier: Modifier = Modifier,
) {
    val type = LiveTvTypography.current()
    TvTextButton(
        onClick = { if (cell.airing) onAiring(channel) else onFuture(channel, cell.programme) },
        compact = true,
        modifier = modifier
            .offset(x = (cell.left + 3f).dp, y = 3.dp)
            .width((cell.width - 6f).coerceAtLeast(28f).dp)
            .height((dimensions.rowHeight.value - 6f).coerceAtLeast(18f).dp)
            .background(if (playing) Color(0x33FFFFFF) else Color.Transparent),
    ) {
        Text(
            cell.programme.title,
            style = type.cell,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
    }
}

@Composable
private fun Modifier.liveTvGuideFocusTarget(
    target: LiveTvGuideFocusTarget,
    requesters: MutableMap<LiveTvGuideFocusTarget, FocusRequester>,
    onFocused: () -> Unit,
): Modifier {
    val requester = remember(target) { FocusRequester() }
    DisposableEffect(target, requester) {
        requesters[target] = requester
        onDispose { if (requesters[target] === requester) requesters.remove(target) }
    }
    return focusRequester(requester).onFocusChanged { if (it.isFocused) onFocused() }
}
