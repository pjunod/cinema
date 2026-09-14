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
import tv.plurx.app.data.DeveloperReadiness
import tv.plurx.app.ui.components.ChoicePicker
import tv.plurx.app.ui.components.safeDisplayInsets

/** Always reachable. Administrator authority and generation CAS are server-enforced. */
@Composable
fun LiveTvDeveloperScreen(origin: String, onBack: () -> Unit) {
    val scope = rememberCoroutineScope()
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    // `LiveTvApi` rejects an unusable origin by throwing, which out of
    // `remember` is an uncaught crash on entering Settings -> Developer rather
    // than the typed message every other Live TV surface shows.
    val token = Session.token.orEmpty()
    val api = remember(origin, token) { runCatching { LiveTvApi(origin, token) }.getOrNull() }
    var saved by remember { mutableStateOf<LiveTvSettings?>(null) }
    var readiness by remember { mutableStateOf<LiveTvReadiness?>(null) }
    var developerReadiness by remember { mutableStateOf<DeveloperReadiness?>(null) }
    var guideReadiness by remember { mutableStateOf<LiveTvGuideReadiness?>(null) }
    var ipv4 by remember { mutableStateOf("") }
    var owner by remember { mutableStateOf("") }
    var sessions by remember { mutableStateOf(2) }
    var height by remember { mutableStateOf(0) }
    var attested by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Administrator access is required.") }
    val dirty = saved?.let { ipv4 != it.live_tv_device_ipv4 || owner != it.live_tv_owner_node_id ||
        sessions != it.live_tv_max_sessions || height != it.live_tv_max_output_height } ?: false

    fun apply(settings: LiveTvSettings) {
        saved = settings
        ipv4 = settings.live_tv_device_ipv4
        owner = settings.live_tv_owner_node_id
        sessions = settings.live_tv_max_sessions
        height = settings.live_tv_max_output_height
        attested = false
        readiness = null
    }
    fun failure(error: Exception): String = (error as? LiveTvFailure)?.message ?: liveTvMessage("stream_failed")
    fun load() {
        val client = api ?: run { message = liveTvMessage("invalid_settings"); return }
        if (busy) return
        busy = true
        scope.launch {
            try {
                apply(client.settings())
                developerReadiness = client.developerReadiness()
                // Advisory in the strongest sense: a guide panel that cannot be
                // read must not cost the operator the settings form it sits
                // under, so its failure is not this load's failure.
                guideReadiness = runCatching { client.guideReadiness() }.getOrNull()
                message = "Settings loaded. Save, check readiness, then enable."
            }
            catch (error: Exception) { saved = null; message = failure(error) }
            finally { busy = false }
        }
    }
    fun write(change: LiveTvSettingsChange) {
        val previous = saved ?: return
        val client = api ?: run { message = liveTvMessage("invalid_settings"); return }
        if (busy) return
        busy = true
        scope.launch {
            try {
                val result = client.save(previous, change)
                apply(result)
                message = "Saved. Recording is ${if (result.dvr_enabled) "enabled" else "disabled"}; Library channels are ${if (result.library_channels_enabled) "enabled" else "disabled"}; Live TV is ${if (result.live_tv_enabled) "enabled" else "disabled"}."
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
        saved?.let { settings ->
            Text("Library channels · advisory enablement", style = MaterialTheme.typography.titleLarge)
            Text("Schedules use already-probed local movies and episodes. These facts explain whether the server looks ready; they never disable or override the explicit switch.")
            Row {
                Checkbox(
                    checked = settings.library_channels_enabled,
                    onCheckedChange = { write(LiveTvSettingsChange.LibraryChannelsEnabled(it)) },
                    enabled = !busy,
                    modifier = Modifier.tvFocusRing(),
                )
                Text("Enable Library channels", Modifier.padding(top = 12.dp))
            }
            developerReadiness?.items?.firstOrNull { it.id == "library_channels" }?.requirements?.forEach { requirement ->
                Text("${when (requirement.status) { "met" -> "Met"; "unmet" -> "Needs attention"; else -> "Not observable" }}: ${requirement.title}")
                Text(requirement.evidence, style = MaterialTheme.typography.bodySmall)
            }
            Button(enabled = !busy, onClick = {
                busy = true
                scope.launch {
                    try { developerReadiness = (api ?: throw LiveTvFailure("invalid_settings")).developerReadiness() }
                    catch (error: Exception) { message = failure(error) }
                    finally { busy = false }
                }
            }) { Text("Refresh Library channel readiness") }

            Text("Recording · advisory enablement", style = MaterialTheme.typography.titleLarge)
            Text("This switch remains available to the operator. The checks explain what safe capture needs; unmet or unobservable checks never disable or override it.")
            Row {
                Checkbox(
                    checked = settings.dvr_enabled,
                    onCheckedChange = { write(LiveTvSettingsChange.DvrEnabled(it)) },
                    enabled = !busy,
                    modifier = Modifier.tvFocusRing(),
                )
                Text("Enable recording", Modifier.padding(top = 12.dp))
            }
            Text("Recording root: ${settings.dvr_root.ifEmpty { "Not set" }}")
            Text("Reserved tuner slots: ${settings.dvr_tuner_reserve}")
            developerReadiness?.items?.firstOrNull { it.id == "dvr" }?.requirements?.forEach { requirement ->
                Text("${when (requirement.status) { "met" -> "Met"; "unmet" -> "Needs attention"; else -> "Not observable" }}: ${requirement.title}")
                Text(requirement.evidence, style = MaterialTheme.typography.bodySmall)
            } ?: Text("Readiness is unavailable. That does not gate the enable switch.")
            Button(enabled = !busy, onClick = {
                busy = true
                scope.launch {
                    try { developerReadiness = (api ?: throw LiveTvFailure("invalid_settings")).developerReadiness() }
                    catch (error: Exception) { message = failure(error) }
                    finally { busy = false }
                }
            }) { Text("Refresh recording readiness") }
        }
        Text("HDHomeRun Live TV · runtime enablement", style = MaterialTheme.typography.titleLarge)
        Text("No special build is needed. Finish the tuner channel scan and reserve a stable private IPv4 address. Choose one reachable, committed voter as tuner owner; keep every serving node on a compatible plurx version.")
        Text("The owner needs tuner network access and writable scratch space. Compatible broadcasts are copied without an encoder; conversion routes additionally need a working FFmpeg encoder and tone mapping when HDR must become SDR.")
        Text("ATSC 3.0 may need HEVC and AC-4 decoders your FFmpeg lacks. DRM, rewind and captions are unsupported. Unprotected channels can be scheduled or recorded manually. Readiness tests the output graph, not every broadcast codec, and never gates either switch.")
        saved?.let { settings ->
            Text(if (settings.live_tv_enabled) "Live TV is enabled" else "Live TV is disabled", style = MaterialTheme.typography.titleMedium)
            val editable = !settings.live_tv_enabled && !busy
            OutlinedTextField(ipv4, { ipv4 = it }, label = { Text("Tuner private IPv4") }, enabled = editable, singleLine = true, modifier = Modifier.fillMaxWidth().tvFocusRing())
            OutlinedTextField(owner, { owner = it }, label = { Text("Tuner-owner node ID") }, enabled = editable, singleLine = true, modifier = Modifier.fillMaxWidth().tvFocusRing())
            Text("Copy the node ID from the server's Settings → Cluster page.")
            if (editable) {
                ChoicePicker("Maximum sessions", sessions, listOf(1, 2, 3, 4), { it.toString() }, { sessions = it })
                ChoicePicker("Maximum quality", height, listOf(0, 480, 720, 1080, 2160),
                    { if (it == 0) "Original / Auto" else "${it}p ceiling" }, { height = it })
            } else Text("Maximum sessions: $sessions · quality: ${if (height == 0) "Original / Auto" else "${height}p ceiling"}")
            Button(enabled = editable && dirty, onClick = { write(LiveTvSettingsChange.Configure(ipv4.trim(), owner.trim(), sessions, 720, height)) }) {
                Text("Save configuration while disabled")
            }
            Button(enabled = !busy && !dirty, onClick = {
                busy = true
                scope.launch {
                    try {
                        val result = (api ?: throw LiveTvFailure("invalid_settings")).readiness()
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
                    write(LiveTvSettingsChange.Configure(settings.live_tv_device_ipv4, settings.live_tv_owner_node_id,
                        settings.live_tv_max_sessions, settings.live_tv_output_height, settings.live_tv_max_output_height))
                }) { Text("Retry authenticated cleanup while disabled") }
            }
        }
        // Every row the server sends, in the order it sent them — never a
        // hand-written subset. `start_recovery` arrived this way without this
        // screen being told about it, and the next row will too.
        readiness?.checks?.forEach { Text("${if (it.ready) "Met" else "Needs attention"}: ${it.message}") }

        Text("Programme guide · advisory enablement", style = MaterialTheme.typography.titleLarge)
        Text("What the guide needs in order to populate, to come back after a deploy, and to say why it last failed — and whether each part is met right now. Every row below is advisory: none of them refuses a save or an enable, and a guide that will not load still leaves a working screen with number and callsign rows.")
        guideReadiness?.let { guide ->
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
        Button(enabled = !busy, onClick = {
            busy = true
            scope.launch {
                try { guideReadiness = (api ?: throw LiveTvFailure("invalid_settings")).guideReadiness() }
                catch (error: Exception) { message = failure(error) }
                finally { busy = false }
            }
        }) { Text("Refresh guide readiness") }

        Text(message)
        TextButton(enabled = !busy, onClick = ::load) { Text("Reload server settings") }
    }
}
