@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.media3.common.C
import androidx.media3.common.Format
import androidx.media3.common.MimeTypes
import androidx.media3.common.Player
import androidx.media3.common.TrackGroup
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.TrackSelectionParameters
import androidx.media3.common.Tracks
import tv.plurx.app.player.subLabel
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.TvTextButton as TextButton

internal data class LiveTvCaptionChoice(
    val group: TrackGroup,
    val index: Int,
    val label: String,
    val supported: Boolean,
    val selected: Boolean,
)

/** Name only services that Media3 actually discovered in this attachment. */
internal fun liveTvCaptionChoices(tracks: Tracks): List<LiveTvCaptionChoice> =
    tracks.groups.filter { it.type == C.TRACK_TYPE_TEXT }.flatMap { group ->
        (0 until group.length).map { index ->
            val format = group.getTrackFormat(index)
            val service = when (format.sampleMimeType) {
                MimeTypes.APPLICATION_CEA608 -> format.accessibilityChannel
                    .takeIf { it != Format.NO_VALUE }?.let { "CC$it" }
                MimeTypes.APPLICATION_CEA708 -> format.accessibilityChannel
                    .takeIf { it != Format.NO_VALUE }?.let { "Service $it" }
                else -> null
            }
            LiveTvCaptionChoice(
                group.mediaTrackGroup,
                index,
                listOfNotNull(subLabel(format), service).distinct().joinToString(" · "),
                group.isTrackSupported(index),
                group.isTrackSelected(index),
            )
        }
    }

/** Off changes only text; an old menu row cannot select a replacement group. */
internal fun liveTvCaptionSelection(
    tracks: Tracks,
    parameters: TrackSelectionParameters,
    choice: LiveTvCaptionChoice?,
): TrackSelectionParameters? {
    if (choice != null) {
        val current = tracks.groups.firstOrNull {
            it.type == C.TRACK_TYPE_TEXT && it.mediaTrackGroup === choice.group
        } ?: return null
        if (choice.index !in 0 until current.length || !current.isTrackSupported(choice.index)) return null
    }
    return parameters.buildUpon()
        .clearOverridesOfType(C.TRACK_TYPE_TEXT)
        .setTrackTypeDisabled(C.TRACK_TYPE_TEXT, choice == null)
        .apply { choice?.let { setOverrideForType(TrackSelectionOverride(it.group, it.index)) } }
        .build()
}

/** Cue delivery and a selected track are observations, not proof of drawing. */
internal fun liveTvCaptionSummary(tracks: Tracks, cuesPresent: Boolean): String {
    val selected = liveTvCaptionChoices(tracks).filter { it.selected }
    if (selected.isEmpty()) return "Off"
    return selected.joinToString { it.label } +
        if (cuesPresent) " · text cues present" else " · no current text cues"
}

@Composable
internal fun LiveTvCaptionDialog(
    player: Player,
    isCurrentPlayer: () -> Boolean,
    onDismiss: () -> Unit,
) {
    var tracks by remember(player) { mutableStateOf(player.currentTracks) }
    var parameters by remember(player) { mutableStateOf(player.trackSelectionParameters) }
    var changed by remember(player) { mutableStateOf(false) }
    val firstFocus = remember { FocusRequester() }
    DisposableEffect(player) {
        val listener = object : Player.Listener {
            override fun onTracksChanged(current: Tracks) { tracks = current }
            override fun onTrackSelectionParametersChanged(current: TrackSelectionParameters) {
                parameters = current
            }
        }
        player.addListener(listener)
        onDispose { player.removeListener(listener) }
    }
    fun select(choice: LiveTvCaptionChoice?) {
        if (!isCurrentPlayer()) { onDismiss(); return }
        val next = liveTvCaptionSelection(player.currentTracks, player.trackSelectionParameters, choice)
        if (next == null) {
            tracks = player.currentTracks
            changed = true
            return
        }
        player.trackSelectionParameters = next
        onDismiss()
    }
    val choices = liveTvCaptionChoices(tracks)
    val off = C.TRACK_TYPE_TEXT in parameters.disabledTrackTypes || choices.none { it.selected }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Live TV captions") },
        text = {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                TextButton(
                    onClick = { select(null) },
                    modifier = Modifier.fillMaxWidth().focusRequester(firstFocus),
                ) { Text("Off" + if (off) " · Selected" else "") }
                choices.forEach { choice ->
                    TextButton(
                        onClick = { select(choice) },
                        enabled = choice.supported,
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        Text(choice.label + when {
                            !choice.supported -> " · Unsupported"
                            choice.selected && !off -> " · Selected"
                            else -> ""
                        })
                    }
                }
                if (choices.isEmpty()) Text("No caption tracks reported by this stream.")
                if (changed) Text("Tracks changed. Select a current caption track.")
            }
        },
        confirmButton = { TextButton(onClick = onDismiss) { Text("Close") } },
    )
    RequestInitialFocus(firstFocus)
}
