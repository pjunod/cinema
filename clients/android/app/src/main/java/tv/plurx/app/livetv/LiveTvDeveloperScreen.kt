package tv.plurx.app.livetv

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.tvFocusRing
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.safeDisplayInsets

/**
 * Settings → Developer: only what is still waiting on evidence, each with the
 * line that says what it waits on. The tuner, guide, recording and Library
 * channel cards graduated to Settings → Live TV ([LiveTvSettingsScreen]).
 *
 * Always reachable. Administrator authority and generation CAS are server-enforced.
 */
@Composable
fun LiveTvDeveloperScreen(origin: String, onBack: () -> Unit) {
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    val state = rememberLiveTvAdminState(origin, LiveTvAdminSurface.Developer)
    val busy = state.busy
    LaunchedEffect(state) { state.load() }

    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)) {
        TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
        Text("Developer", style = MaterialTheme.typography.headlineMedium)
        tv.plurx.app.ui.SharedSharingDeveloperCard()
        tv.plurx.app.remote.RemoteDeveloperCard()
        Text("Tuner, guide, recording and Library channel settings are in Settings → Live TV.")
        LiveTvSettingsPlacement.developer.forEach { card ->
            when (card) {
                LiveTvSettingsCard.DisplayCadence -> DisplayCadenceCard(state)
                LiveTvSettingsCard.EnableLiveTv -> EnableLiveTvCard(state)
                else -> Unit
            }
        }
        Text(state.message)
        TextButton(enabled = !busy, onClick = state::load) { Text("Reload server settings") }
    }
}

@Composable
private fun DisplayCadenceCard(state: LiveTvAdminState) {
    val settings = state.saved ?: return
    Text(LiveTvSettingsCard.DisplayCadence.title, style = MaterialTheme.typography.titleLarge)
    Text("Requests only a same-resolution refresh rate matching the delivered cadence. A physical television and HDMI sink must verify the result; the observations below never disable or override the switch.")
    Row {
        Checkbox(
            checked = settings.playback_display_mode_match,
            onCheckedChange = { state.write(LiveTvSettingsChange.DisplayModeMatch(it)) },
            enabled = !state.busy,
            modifier = Modifier.tvFocusRing(),
        )
        Text("Match television refresh rate", Modifier.padding(top = 12.dp))
    }
    state.developerReadiness?.items?.firstOrNull { it.id == "android_display_mode_match" }
        ?.requirements?.forEach { requirement ->
            Text("${when (requirement.status) { "met" -> "Met"; "unmet" -> "Needs attention"; else -> "Not observable" }}: ${requirement.title}")
            Text(requirement.evidence, style = MaterialTheme.typography.bodySmall)
        } ?: Text("Readiness is unavailable. That does not gate the enable switch.")
    Text(LiveTvSettingsPlacement.DISPLAY_CADENCE_GRADUATION, style = MaterialTheme.typography.bodySmall)
}

@Composable
private fun EnableLiveTvCard(state: LiveTvAdminState) {
    Text(LiveTvSettingsCard.EnableLiveTv.title, style = MaterialTheme.typography.titleLarge)
    Text("No special build is needed. Finish the tuner channel scan and reserve a stable private IPv4 address. Keep cluster servers upgraded and clocks synchronized. The tuner is shared by eligible servers.")
    Text("At least one eligible server needs tuner network access and writable scratch space. Viewers and recordings share channel transports. Compatible broadcasts are copied without an encoder; conversion routes additionally need a working FFmpeg encoder and tone mapping when HDR must become SDR.")
    Text("ATSC 3.0 may need HEVC and AC-4 decoders your FFmpeg lacks. DRM, rewind and captions are unsupported. Unprotected channels can be scheduled or recorded manually. Readiness tests the output graph, not every broadcast codec, and never gates either switch.")
    state.saved?.let { settings ->
        Text(if (settings.live_tv_enabled) "Live TV is enabled" else "Live TV is disabled", style = MaterialTheme.typography.titleMedium)
        Button(enabled = !state.busy, onClick = { state.write(LiveTvSettingsChange.Enabled(!settings.live_tv_enabled)) }) {
            Text(if (settings.live_tv_enabled) "Disable Live TV and drain sessions" else "Enable Live TV")
        }
    }
    // The prerequisites as the server last observed them, every row it sent
    // in the order it sent them — the web card's `/live-tv/readiness` read.
    // Advisory: no row reaches the button above.
    state.prerequisites?.let { prerequisites ->
        prerequisites.checks.forEach { Text("${if (it.ready) "Met" else "Not met"}: ${it.message}") }
        Text("Advisory only. Unknown or unmet requirements do not prevent enabling.", style = MaterialTheme.typography.bodySmall)
    } ?: state.prerequisitesError?.let {
        Text("Readiness unavailable: $it. You can still enable Live TV.")
    }
    Button(enabled = !state.busy, onClick = state::refreshPrerequisites) { Text("Refresh Live TV prerequisites") }
    Text("The tuner address, stream limit and quality ceiling are set in Settings → Live TV.", style = MaterialTheme.typography.bodySmall)
    Text(LiveTvSettingsPlacement.ENABLE_LIVE_TV_GRADUATION, style = MaterialTheme.typography.bodySmall)
}
