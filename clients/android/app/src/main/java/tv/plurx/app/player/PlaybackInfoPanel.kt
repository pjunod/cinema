@file:OptIn(androidx.compose.ui.ExperimentalComposeUiApi::class)

package tv.plurx.app.player

import android.content.res.Configuration
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.focusGroup
import androidx.compose.foundation.focusable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusProperties
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalConfiguration
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.stateDescription
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.shape.RoundedCornerShape
import tv.plurx.app.ui.components.tvFocusRing

internal data class PlaybackInfoFact(
    val id: String,
    val label: String,
    val value: String,
    val note: String? = null,
    val group: String = "Picture & sound",
    val diagnosticOnly: Boolean = false,
)

/** Shared visual presentation for library, downloaded and Live TV players. */
@Composable
internal fun PlaybackInfoPanel(
    title: String,
    facts: List<PlaybackInfoFact>,
    mode: PlaybackStatsMode,
    onMode: (PlaybackStatsMode) -> Unit,
    onClose: () -> Unit,
    modifier: Modifier = Modifier,
    isLive: Boolean = false,
) {
    val tv = LocalConfiguration.current.uiMode and Configuration.UI_MODE_TYPE_MASK == Configuration.UI_MODE_TYPE_TELEVISION
    val close = remember { FocusRequester() }
    LaunchedEffect(Unit) { close.requestFocus() }
    val shape = RoundedCornerShape(20.dp)
    val muted = Color(0xFFACB7C9)
    val bodySize = if (tv) 20.sp else 16.sp
    val labelSize = if (tv) 16.sp else 13.sp
    var expanded by remember { mutableStateOf(setOf("Picture & sound")) }
    fun value(id: String) = facts.firstOrNull { it.id == id }?.value ?: "Not reported"
    fun note(id: String) = facts.firstOrNull { it.id == id }?.note
    Column(
        modifier.clip(shape).background(Color(0xF714171E)).border(1.dp, Color(0xFF343D4D), shape)
            .focusGroup().focusProperties { onExit = { if (mode != PlaybackStatsMode.Mini) cancelFocusChange() } },
    ) {
        Row(Modifier.fillMaxWidth().padding(20.dp), verticalAlignment = Alignment.Top) {
            Column(Modifier.weight(1f), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                Text("Playback info", color = Color.White, fontSize = if (tv) 26.sp else 22.sp, fontWeight = FontWeight.SemiBold)
                Text(title, color = muted, fontSize = labelSize)
            }
            InfoAction(if (mode == PlaybackStatsMode.Mini) "Expand" else "Compact") {
                onMode(if (mode == PlaybackStatsMode.Mini) PlaybackStatsMode.Standard else PlaybackStatsMode.Mini)
            }
            InfoAction("Close", Modifier.focusRequester(close), onClick = onClose)
        }
        if (mode != PlaybackStatsMode.Mini) {
            Row(Modifier.fillMaxWidth().padding(horizontal = 12.dp), horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                listOf(PlaybackStatsMode.Standard, PlaybackStatsMode.Details, PlaybackStatsMode.Debug).forEach { candidate ->
                    InfoAction(candidate.label, Modifier.weight(1f).semantics { selected = candidate == mode }, selected = candidate == mode) { onMode(candidate) }
                }
            }
            HorizontalDivider(Modifier.padding(top = 12.dp), color = Color(0xFF343D4D))
        }
        BoxWithConstraints(Modifier.weight(1f, fill = false)) {
            val wide = maxWidth >= 560.dp
            Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(20.dp)) {
                if (mode == PlaybackStatsMode.Mini) {
                    InfoPair(wide,
                        { InfoSummary("Playing resolution", value("decode_resolution"), tv = tv) },
                        { InfoSummary("Playback", value("player_state"), tv = tv) })
                    InfoSummary("Buffered on device", value("client_loaded"), tv = tv)
                } else if (mode == PlaybackStatsMode.Standard) {
                    Text(value("player_state") + if (isLive) " · Live TV" else "", color = Color(0xFF8BB8FF), fontSize = bodySize, fontWeight = FontWeight.SemiBold)
                    InfoPair(wide,
                        { InfoSummary("Playing resolution", value("decode_resolution"), "Size reported by the attached player.", tv, hero = true) },
                        { InfoSummary(if (isLive) "Broadcast source" else "Original file", value("source_resolution"), facts.firstOrNull { it.id == "source_video" }?.value, tv) })
                    Column(Modifier.fillMaxWidth().background(Color(0xFF1E232D), RoundedCornerShape(10.dp)).tvFocusRing(RoundedCornerShape(10.dp), focusedScale = 1f).focusable(tv).padding(16.dp), verticalArrangement = Arrangement.spacedBy(5.dp)) {
                        Text(value("method"), color = Color.White, fontWeight = FontWeight.SemiBold, fontSize = bodySize)
                        Text(facts.firstOrNull { it.id == "reason" }?.value ?: playbackMethodExplanation(value("method")), color = muted, fontSize = labelSize)
                    }
                    InfoPair(wide,
                        { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                            InfoSummary("Stream audio track", value("decode_audio"), "Track metadata; device output is not reported.", tv)
                            if (facts.none { it.id == "decode_audio" }) facts.firstOrNull { it.id == "source_audio" }?.let {
                                InfoSummary("Original audio track", it.value, tv = tv)
                            }
                        } },
                        { InfoSummary("Subtitles", value("subtitles"), note("subtitles"), tv) })
                    HorizontalDivider(color = Color(0xFF343D4D))
                    InfoPair(wide,
                        { InfoSummary("Buffered on this device", value("client_loaded"), "Contiguous media loaded ahead of your position.", tv) },
                        { InfoSummary(if (isLive) "Behind stream live edge" else "Buffering interruptions", value(if (isLive) "live_edge" else "stalls"), if (isLive) "Behind latest available media; not broadcast delay." else "Player interruptions; intentional pauses excluded.", tv) })
                    if (isLive) InfoSummary("Tuner reception", value("reception"), tv = tv)
                } else {
                    Text("Source, stream and player observations are separate. Unavailable is not zero.", color = muted, fontSize = labelSize)
                    listOf("Picture & sound", "Buffer & delivery", "Live stream & reception", "Server work", "Session & history").forEach { group ->
                        val rows = facts.filter { it.group == group && (mode == PlaybackStatsMode.Debug || !it.diagnosticOnly) }
                        if (rows.isNotEmpty()) {
                            Column {
                                InfoAction(group + if (group in expanded) "  −" else "  +", Modifier.fillMaxWidth()) {
                                    expanded = if (group in expanded) expanded - group else expanded + group
                                }
                                if (group in expanded) rows.forEach { row ->
                                    Column(Modifier.fillMaxWidth().tvFocusRing(RoundedCornerShape(8.dp), focusedScale = 1f).focusable(tv).padding(vertical = 12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                                        HorizontalDivider(color = Color(0xFF343D4D))
                                        if (wide) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(20.dp)) {
                                            Text(row.label, color = muted, fontSize = bodySize, modifier = Modifier.weight(1f))
                                            Text(row.value, color = Color.White, fontSize = bodySize, modifier = Modifier.weight(1f))
                                        } else {
                                            Text(row.label, color = muted, fontSize = labelSize)
                                            Text(row.value, color = Color.White, fontSize = bodySize)
                                        }
                                        row.note?.let { Text(it, color = muted, fontSize = labelSize) }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun InfoAction(label: String, modifier: Modifier = Modifier, selected: Boolean = false, onClick: () -> Unit) {
    val shape = RoundedCornerShape(9.dp)
    Text(label, color = if (selected) Color(0xFF8BB8FF) else Color.White,
        fontSize = 14.sp, fontWeight = FontWeight.SemiBold,
        modifier = modifier.clip(shape).background(if (selected) Color(0xFF203450) else Color.Transparent)
            .tvFocusRing(shape, focusedScale = 1f).clickable(onClick = onClick)
            .heightIn(min = 44.dp).padding(horizontal = 10.dp, vertical = 12.dp))
}

@Composable
private fun InfoPair(wide: Boolean, first: @Composable () -> Unit, second: @Composable () -> Unit) {
    if (wide) Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(24.dp)) {
        Column(Modifier.weight(1f)) { first() }
        Column(Modifier.weight(1f)) { second() }
    } else Column(verticalArrangement = Arrangement.spacedBy(18.dp)) { first(); second() }
}

@Composable
private fun InfoSummary(label: String, value: String, note: String? = null, tv: Boolean, hero: Boolean = false) {
    Column(Modifier.fillMaxWidth().tvFocusRing(RoundedCornerShape(8.dp), focusedScale = 1f).focusable(tv), verticalArrangement = Arrangement.spacedBy(6.dp)) {
        Text(label, color = Color(0xFFACB7C9), fontSize = if (tv) 16.sp else 13.sp)
        Text(value, color = Color.White, fontSize = if (hero) if (tv) 44.sp else 34.sp else if (tv) 24.sp else 19.sp, fontWeight = FontWeight.SemiBold)
        note?.let { Text(it, color = Color(0xFFACB7C9), fontSize = if (tv) 16.sp else 13.sp) }
    }
}

internal fun playbackMethodExplanation(method: String): String = when {
    method.contains("download", true) -> "Playing the copy stored on this device. No server delivery is needed."
    method.contains("cache", true) -> "Playing a prepared copy. Original and player picture sizes are reported separately."
    method.contains("direct", true) -> "Original media is delivered without server conversion."
    method.contains("remux", true) -> "Media is repackaged for this player; video is not re-encoded."
    else -> "The server selected this delivery method. No further reason was reported."
}

internal fun playbackInfoGroup(section: String): String = when (section) {
    "SOURCE", "NOW DECODING" -> "Picture & sound"
    "BUFFERING / DELIVERY", "NETWORK" -> "Buffer & delivery"
    "SERVER" -> "Server work"
    else -> "Session & history"
}

internal fun playbackInfoExplanation(id: String): String? = when (id) {
    "decode_resolution" -> "Attached player measurement; never inferred from source size."
    "source_resolution" -> "Original file metadata."
    "decode_audio" -> "Selected stream track; not the device's audio output."
    "client_loaded" -> "Contiguous media loaded ahead on this device."
    "server_ready" -> "Complete media ahead on the server; separate from the device buffer."
    "delivery_rate" -> "Server-completed responses; not confirmed client receipt."
    "observed_rate" -> "Player estimate during transfers; bursty by design."
    "stream_rate" -> "Stream bitrate; not connection speed."
    "delivered" -> "Server-completed response bytes, not proof of playback."
    "stalls" -> "Player interruptions for this playback; intentional pauses excluded."
    "status" -> "Server work state; separate from whether the picture is playing."
    "status_age" -> "Age of the latest server response."
    "http_wait" -> "Server responses waiting for publication; not player stalls."
    "production_actual" -> "Encoder progress ahead of demand; not loaded video."
    "production_target" -> "Pacing policy, not a measurement."
    else -> null
}
