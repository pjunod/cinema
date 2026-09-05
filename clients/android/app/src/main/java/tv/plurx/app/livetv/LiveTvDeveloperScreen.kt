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
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.tvFocusRing
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import tv.plurx.app.data.Session
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.safeDisplayInsets

/** Always reachable. Administrator authority and generation CAS are server-enforced. */
@Composable
fun LiveTvDeveloperScreen(origin: String, onBack: () -> Unit) {
    val scope = rememberCoroutineScope()
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    val api = remember(origin) { LiveTvApi(origin, Session.token.orEmpty()) }
    var saved by remember { mutableStateOf<LiveTvSettings?>(null) }
    var readiness by remember { mutableStateOf<LiveTvReadiness?>(null) }
    var ipv4 by remember { mutableStateOf("") }
    var owner by remember { mutableStateOf("") }
    var sessions by remember { mutableStateOf(2) }
    var height by remember { mutableStateOf(720) }
    var attested by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Administrator access is required.") }
    val dirty = saved?.let { ipv4 != it.live_tv_device_ipv4 || owner != it.live_tv_owner_node_id ||
        sessions != it.live_tv_max_sessions || height != it.live_tv_output_height } ?: false

    fun apply(settings: LiveTvSettings) {
        saved = settings
        ipv4 = settings.live_tv_device_ipv4
        owner = settings.live_tv_owner_node_id
        sessions = settings.live_tv_max_sessions
        height = settings.live_tv_output_height
        attested = false
        readiness = null
    }
    fun failure(error: Exception): String = (error as? LiveTvFailure)?.message ?: liveTvMessage("stream_failed")
    fun load() {
        if (busy) return
        busy = true
        scope.launch {
            try { apply(api.settings()); message = "Settings loaded. Save, check readiness, then enable." }
            catch (error: Exception) { saved = null; message = failure(error) }
            finally { busy = false }
        }
    }
    fun write(change: LiveTvSettingsChange) {
        val previous = saved ?: return
        if (busy) return
        busy = true
        scope.launch {
            try {
                val result = api.save(previous, change)
                apply(result)
                message = "Saved. Live TV is ${if (result.live_tv_enabled) "enabled" else "disabled"}."
            } catch (error: Exception) {
                // Never retry an uncertain mutation with stale generation/CAS.
                saved = null; readiness = null
                message = failure(error) + " Reload settings before trying again."
            } finally { busy = false }
        }
    }
    LaunchedEffect(api) { load() }

    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp)) {
        TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
        Text("Developer", style = MaterialTheme.typography.headlineMedium)
        Text("HDHomeRun Live TV · runtime enablement", style = MaterialTheme.typography.titleLarge)
        Text("No special build is needed. Finish the tuner channel scan and reserve a stable private IPv4 address. Choose one reachable, committed voter as tuner owner; keep every serving node on a compatible plurx version.")
        Text("The owner needs tuner network access, writable scratch space, and FFmpeg H.264/AAC encoding. Each viewer uses one physical tuner and one encoder slot. Start with 720p and two sessions.")
        Text("ATSC 3.0 may need HEVC and AC-4 decoders your FFmpeg lacks. DRM, DVR, rewind, captions and guide scheduling are unsupported. Readiness tests the output graph, not every broadcast codec.")
        saved?.let { settings ->
            Text(if (settings.live_tv_enabled) "Live TV is enabled" else "Live TV is disabled", style = MaterialTheme.typography.titleMedium)
            val editable = !settings.live_tv_enabled && !busy
            OutlinedTextField(ipv4, { ipv4 = it }, label = { Text("Tuner private IPv4") }, enabled = editable, singleLine = true, modifier = Modifier.fillMaxWidth().tvFocusRing())
            OutlinedTextField(owner, { owner = it }, label = { Text("Tuner-owner node ID") }, enabled = editable, singleLine = true, modifier = Modifier.fillMaxWidth().tvFocusRing())
            Text("Copy the node ID from the server's Settings → Cluster page.")
            if (editable) {
                ChoicePicker("Maximum sessions", sessions, listOf(1, 2, 3, 4), { it.toString() }, { sessions = it })
                ChoicePicker("Output height", height, listOf(720, 1080), { "${it}p" }, { height = it })
            } else Text("Maximum sessions: $sessions · output: ${height}p")
            Button(enabled = editable && dirty, onClick = { write(LiveTvSettingsChange.Configure(ipv4.trim(), owner.trim(), sessions, height)) }) {
                Text("Save configuration while disabled")
            }
            Button(enabled = !busy && !dirty, onClick = {
                busy = true
                scope.launch {
                    try {
                        val result = api.readiness()
                        if (result.generation != settings.live_tv_config_generation) throw LiveTvFailure("settings_conflict")
                        readiness = result
                        message = if (result.ready) "Ready. Enable is a separate action." else "Resolve the failed checks before enabling."
                    } catch (error: Exception) { readiness = null; message = failure(error) }
                    finally { busy = false }
                }
            }) { Text("Check saved configuration") }
            Button(enabled = !busy && !dirty, onClick = { write(LiveTvSettingsChange.Enabled(!settings.live_tv_enabled)) }) {
                Text(if (settings.live_tv_enabled) "Disable Live TV and drain sessions" else "Enable Live TV")
            }
            Text("Saving never enables playback. Enable rechecks readiness on the server. Disable before changing owner or tuner.")
            if (settings.live_tv_transition_from_owner_node_id.isNotEmpty()) {
                Text("Previous owner cleanup is unconfirmed", style = MaterialTheme.typography.titleMedium)
                Text("Previous owner: ${settings.live_tv_transition_from_owner_node_id} · generations before ${settings.live_tv_transition_drain_before}. An unreachable node is not proof that its tuner process stopped.")
                Text("Restore the old node's connection and retry authenticated cleanup first. Physical recovery is only safe after actually stopping that node and preventing its restart until it synchronizes current settings.")
                Row {
                    Checkbox(attested, onCheckedChange = { attested = it }, enabled = editable, modifier = Modifier.tvFocusRing())
                    Text("I have stopped or powered off this previous owner and prevented it from restarting until it can synchronize the current configuration.")
                }
                Button(enabled = editable && attested && !dirty, onClick = {
                    write(LiveTvSettingsChange.FencedOwner(settings.live_tv_transition_from_owner_node_id, settings.live_tv_transition_drain_before))
                }) { Text("Record physical fencing of this exact previous owner") }
                Button(enabled = editable && !dirty, onClick = {
                    write(LiveTvSettingsChange.Configure(settings.live_tv_device_ipv4, settings.live_tv_owner_node_id, settings.live_tv_max_sessions, settings.live_tv_output_height))
                }) { Text("Retry authenticated cleanup while disabled") }
            }
        }
        readiness?.checks?.forEach { Text("${if (it.ready) "Ready" else "Needs attention"}: ${it.message}") }
        Text(message)
        TextButton(enabled = !busy, onClick = ::load) { Text("Reload server settings") }
    }
}
