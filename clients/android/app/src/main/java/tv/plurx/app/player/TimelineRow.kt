package tv.plurx.app.player

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.focusable
import androidx.compose.foundation.gestures.detectDragGestures
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.onFocusChanged
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.formatTime
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.theme.Accent

internal const val TimelineElapsedTag = "player-timeline-elapsed"

internal fun timelinePositionMs(x: Float, width: Int, durationMs: Long): Long {
    if (width <= 0 || durationMs <= 0) return 0L
    return (x / width.toFloat()).coerceIn(0f, 1f).times(durationMs).toLong()
}

/** One focusable bar between two passive time labels. Key routing stays at the player root. */
@Composable
internal fun TimelineRow(
    positionMs: Long,
    durationMs: Long,
    pendingMs: Long?,
    focusRequester: FocusRequester,
    upRequester: FocusRequester = FocusRequester.Cancel,
    downRequester: FocusRequester = FocusRequester.Cancel,
    onFocusChanged: (Boolean) -> Unit,
    onTouchPreview: (Long) -> Unit,
    onTouchCommit: () -> Unit,
    modifier: Modifier = Modifier,
) {
    val shownMs = pendingMs ?: positionMs
    val accent = Accent
    val safeDuration = durationMs.coerceAtLeast(0L)
    val elapsedFraction = if (safeDuration > 0L) {
        positionMs.coerceIn(0L, safeDuration).toFloat() / safeDuration
    } else {
        0f
    }
    val previewFraction = pendingMs?.let {
        if (safeDuration > 0L) it.coerceIn(0L, safeDuration).toFloat() / safeDuration else 0f
    }
    Row(
        modifier.fillMaxWidth(),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Text(
            formatTime(shownMs),
            color = Color.White,
            style = MaterialTheme.typography.labelMedium,
            modifier = Modifier.testTag(TimelineElapsedTag),
        )
        Box(
            Modifier
                .weight(1f)
                .height(24.dp)
                .focusRequester(focusRequester)
                .focusProperties {
                    up = upRequester
                    down = downRequester
                }
                .onFocusChanged { onFocusChanged(it.isFocused) }
                .tvFocusRing(MaterialTheme.shapes.extraLarge, focusedScale = 1f)
                .focusable()
                .semantics {
                    contentDescription = "Playback position"
                    stateDescription = formatTime(shownMs)
                }
                .pointerInput(safeDuration) {
                    detectDragGestures(
                        onDragStart = { point ->
                            onTouchPreview(timelinePositionMs(point.x, size.width, safeDuration))
                        },
                        onDragEnd = onTouchCommit,
                        onDragCancel = onTouchCommit,
                    ) { change, _ ->
                        change.consume()
                        onTouchPreview(
                            timelinePositionMs(change.position.x, size.width, safeDuration),
                        )
                    }
                }
                .pointerInput(safeDuration) {
                    detectTapGestures { point ->
                        onTouchPreview(timelinePositionMs(point.x, size.width, safeDuration))
                        onTouchCommit()
                    }
                },
        ) {
            Canvas(Modifier.matchParentSize()) {
                val trackHeight = 4.dp.toPx()
                val trackTop = (size.height - trackHeight) / 2f
                drawRoundRect(
                    color = Color.White.copy(alpha = 0.34f),
                    topLeft = Offset(0f, trackTop),
                    size = Size(size.width, trackHeight),
                )
                drawRoundRect(
                    color = accent,
                    topLeft = Offset(0f, trackTop),
                    size = Size(size.width * elapsedFraction, trackHeight),
                )
                previewFraction?.let { fraction ->
                    drawRoundRect(
                        color = accent.copy(alpha = 0.5f),
                        topLeft = Offset(0f, trackTop),
                        size = Size(size.width * fraction, trackHeight),
                    )
                }
                val thumbFraction = previewFraction ?: elapsedFraction
                drawCircle(
                    color = accent,
                    radius = 6.dp.toPx(),
                    center = Offset(size.width * thumbFraction, size.height / 2f),
                )
            }
        }
        Text(
            formatTime(safeDuration),
            color = Color.White,
            style = MaterialTheme.typography.labelMedium,
        )
    }
}
