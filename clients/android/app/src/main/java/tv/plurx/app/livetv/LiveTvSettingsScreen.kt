package tv.plurx.app.livetv

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.windowInsetsPadding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.unit.dp
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.components.tvFocusRing
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton

/**
 * Settings → Live TV: the tuner, the programme guide, recording and Library
 * channels, in the web's order. These used to sit in Developer; the web moved
 * them here first and Android follows. The Live TV enable switch itself still
 * waits in Developer (see [LiveTvSettingsPlacement.ENABLE_LIVE_TV_GRADUATION]).
 *
 * Always reachable. Administrator authority and generation CAS are
 * server-enforced; every readiness row below is advisory and none of them
 * reaches an `enabled =`.
 */
@Composable
fun LiveTvSettingsScreen(origin: String, onBack: () -> Unit) {
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    val state = rememberLiveTvAdminState(origin, readsGuide = true)
    val busy = state.busy
    LaunchedEffect(state) { state.load() }

    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)) {
        TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
        Text("Live TV", style = MaterialTheme.typography.headlineMedium)
        Text("Manage your tuner, programme guide, recordings and library channels.")

        LiveTvSettingsPlacement.liveTvSettings.forEach { card ->
            when (card) {
                LiveTvSettingsCard.HdHomeRun -> HdHomeRunCard(state)
                LiveTvSettingsCard.ProgrammeGuide -> ProgrammeGuideCard(state)
                LiveTvSettingsCard.Recording -> RecordingCard(state)
                LiveTvSettingsCard.LibraryChannels -> LibraryChannelsCard(state)
                else -> Unit
            }
        }

        Text(state.message)
        TextButton(enabled = !busy, onClick = state::load) { Text("Reload server settings") }
    }
}

@Composable
private fun HdHomeRunCard(state: LiveTvAdminState) {
    val busy = state.busy
    Text(LiveTvSettingsCard.HdHomeRun.title, style = MaterialTheme.typography.titleLarge)
    Text("One network device shared by eligible cluster servers. Viewers and recordings on the same channel share one tuner stream; a session stays on its selected server and the tuner itself belongs to the cluster.")
    state.saved?.let { settings ->
        Text(if (settings.live_tv_enabled) "Live TV is enabled" else "Live TV is disabled", style = MaterialTheme.typography.titleMedium)
        val editable = !busy
        OutlinedTextField(state.ipv4, { state.ipv4 = it }, label = { Text("Tuner private IPv4") }, enabled = editable, singleLine = true, modifier = Modifier.fillMaxWidth().tvFocusRing())
        Text("Copy the node ID from the server's Settings → Cluster page.")
        if (editable) {
            ChoicePicker("Maximum channel streams", state.sessions, listOf(1, 2, 3, 4), { it.toString() }, { state.sessions = it })
            ChoicePicker("Maximum quality", state.height, listOf(0, 480, 720, 1080, 2160),
                { if (it == 0) "Original / Auto" else "${it}p ceiling" }, { state.height = it })
        } else Text("Maximum channel streams: ${state.sessions} · quality: ${if (state.height == 0) "Original / Auto" else "${state.height}p ceiling"}")
        Button(enabled = editable && state.dirty, onClick = {
            state.write(LiveTvSettingsChange.Configure(state.ipv4.trim(), state.owner.trim(), state.sessions, 720, state.height))
        }) {
            Text("Save configuration")
        }
        Button(enabled = !busy && !state.dirty, onClick = state::checkSavedConfiguration) { Text("Check saved configuration") }
        Text("Saving preserves enablement and ends streams using the previous configuration. Enablement and its advisory prerequisites are in Settings → Developer → Enable Live TV.")
    }
    // Every row the server sends, in the order it sent them — never a
    // hand-written subset. `start_recovery` arrived this way without this
    // screen being told about it, and the next row will too.
    state.readiness?.checks?.forEach { Text("${if (it.ready) "Met" else "Needs attention"}: ${it.message}") }
}

@Composable
private fun ProgrammeGuideCard(state: LiveTvAdminState) {
    Text(LiveTvSettingsCard.ProgrammeGuide.title, style = MaterialTheme.typography.titleLarge)
    Text("What the guide needs in order to populate, to come back after a deploy, and to say why it last failed — and whether each part is met right now. Every row below is advisory: none of them refuses a save or an enable, and a guide that will not load still leaves a working screen with number and callsign rows.")
    state.guideReadiness?.let { guide ->
        Text(
            "Source ${guide.source} · ${guide.freshness} · ${guide.matched_channels} of " +
                "${guide.lineup_channels} channels matched · ${guide.programmes} programmes · " +
                "${guide.guide_hours}-hour horizon · refreshed every ${guide.refresh_interval_seconds}s",
            style = MaterialTheme.typography.bodySmall,
        )
        guide.checks.forEach { Text("${if (it.ready) "Met" else "Needs attention"}: ${it.message}") }
        if (guide.checks.isEmpty()) Text("This server sent no guide rows.", style = MaterialTheme.typography.bodySmall)
    } ?: Text(
        "Guide readiness has not been read from this server. It is a read of the owner's cache and never triggers a fetch.",
        style = MaterialTheme.typography.bodySmall,
    )
    Button(enabled = !state.busy, onClick = state::refreshGuideReadiness) { Text("Refresh guide readiness") }
}

@Composable
private fun RecordingCard(state: LiveTvAdminState) {
    val settings = state.saved ?: return
    Text(LiveTvSettingsCard.Recording.title, style = MaterialTheme.typography.titleLarge)
    Text("This switch remains available to the operator. The checks explain what safe capture needs; unmet or unobservable checks never disable or override it.")
    Row {
        Checkbox(
            checked = settings.dvr_enabled,
            onCheckedChange = { state.write(LiveTvSettingsChange.DvrEnabled(it)) },
            enabled = !state.busy,
            modifier = Modifier.tvFocusRing(),
        )
        Text("Enable recording", Modifier.padding(top = 12.dp))
    }
    Text("Recording root: ${settings.dvr_root.ifEmpty { "Not set" }}")
    Text("Reserved tuner slots: ${settings.dvr_tuner_reserve}")
    state.developerReadiness?.items?.firstOrNull { it.id == "dvr" }?.requirements?.forEach { requirement ->
        Text("${when (requirement.status) { "met" -> "Met"; "unmet" -> "Needs attention"; else -> "Not observable" }}: ${requirement.title}")
        Text(requirement.evidence, style = MaterialTheme.typography.bodySmall)
    } ?: Text("Readiness is unavailable. That does not gate the enable switch.")
    Button(enabled = !state.busy, onClick = state::refreshDeveloperReadiness) { Text("Refresh recording readiness") }
}

@Composable
private fun LibraryChannelsCard(state: LiveTvAdminState) {
    val settings = state.saved ?: return
    Text(LiveTvSettingsCard.LibraryChannels.title, style = MaterialTheme.typography.titleLarge)
    Text("Schedules use already-probed local movies and episodes. These facts explain whether the server looks ready; they never disable or override the explicit switch.")
    Row {
        Checkbox(
            checked = settings.library_channels_enabled,
            onCheckedChange = { state.write(LiveTvSettingsChange.LibraryChannelsEnabled(it)) },
            enabled = !state.busy,
            modifier = Modifier.tvFocusRing(),
        )
        Text("Enable Library channels", Modifier.padding(top = 12.dp))
    }
    state.developerReadiness?.items?.firstOrNull { it.id == "library_channels" }?.requirements?.forEach { requirement ->
        Text("${when (requirement.status) { "met" -> "Met"; "unmet" -> "Needs attention"; else -> "Not observable" }}: ${requirement.title}")
        Text(requirement.evidence, style = MaterialTheme.typography.bodySmall)
    }
    Button(enabled = !state.busy, onClick = state::refreshDeveloperReadiness) { Text("Refresh Library channel readiness") }
}
