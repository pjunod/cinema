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
 * line that says what it waits on. The Enable Live TV switch and the tuner,
 * guide, recording and Library channel cards graduated to Settings → Live TV
 * ([LiveTvSettingsScreen]).
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
        Text("Remote diagnostics · advisory readiness",style=MaterialTheme.typography.titleMedium)
        tv.plurx.app.diagnostics.DiagnosticReportButton()
        Text("Awaiting privacy/resource qualification, signed-release evidence and physical phone/TV approval-flow acceptance. This advisory leaves Developer after those checks pass; local history remains a privacy choice in Help.")
        tv.plurx.app.ui.SharedSharingDeveloperCard()
        tv.plurx.app.remote.RemoteDeveloperCard()
        Text("Enable Live TV and the tuner, guide, recording and Library channel settings are in Settings → Live TV.")
        LiveTvSettingsPlacement.developer.forEach { card ->
            when (card) {
                LiveTvSettingsCard.DisplayCadence -> DisplayCadenceCard(state)
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
