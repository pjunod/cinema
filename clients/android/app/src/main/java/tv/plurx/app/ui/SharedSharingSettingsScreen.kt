package tv.plurx.app.ui

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Checkbox
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import kotlinx.serialization.json.*
import tv.plurx.app.data.SharedLibraryClient
import tv.plurx.app.data.SharedSharingDraft
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.components.tvFocusRing

@Composable
fun SharedSharingSettingsScreen(onBack: () -> Unit, onLibraries: () -> Unit) {
    var management by remember { mutableStateOf(false) }
    if (management) { SharedSharingManagementScreen(onBack = { management = false }); return }
    var summaries by remember { mutableStateOf<List<Pair<String, String>>>(emptyList()) }
    var errors by remember { mutableStateOf<List<String>>(emptyList()) }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    suspend fun load() {
        busy = true; summaries = emptyList(); errors = emptyList()
        try {
            val client = SharedLibraryClient.create()
            for (kind in listOf("status", "imports", "exports")) {
                try {
                    val wire = client.management(kind); client.requireCurrent()
                    if (kind == "status") {
                        summaries = summaries + listOf("listener", "serve", "outbound", "topology_qualification").map { it.replace('_', ' ') to (wire[it]?.jsonPrimitive?.contentOrNull ?: "Unknown") } + ("Observation" to "This server node")
                    } else {
                        val rows = wire[kind]?.jsonArray ?: continue
                        summaries = summaries + rows.map { entry ->
                            val value = entry.jsonObject
                            val summary = value[if (kind == "imports") "import" else "grant"]?.jsonObject ?: value
                            (if (kind == "imports") summary["source_name"]?.jsonPrimitive?.contentOrNull ?: "Shared Source" else "Export") to (summary["state"]?.jsonPrimitive?.contentOrNull ?: "Unknown")
                        }
                        if (rows.isEmpty()) summaries = summaries + (kind to "None")
                    }
                } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; errors = errors + "$kind: ${failure.message ?: "Unavailable"}" }
            }
        } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; errors = errors + (failure.message ?: "Sharing management unavailable") }
        finally { busy = false }
    }
    LaunchedEffect(Unit) { load() }
    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
        TextButton(onClick = onBack) { Text("Back") }
        Text("Sharing", style = MaterialTheme.typography.headlineMedium)
        SharedLibrariesEntry(onLibraries)
        SharedSharingEnableSection()
        summaries.forEach { (label, value) -> Text("$label: $value") }
        errors.forEach { Text(it) }
        TextButton(enabled = !busy, onClick = { scope.launch { load() } }) { Text("Refresh status") }
        TextButton(onClick = { management = true }) { Text("Manage invitations and Sources") }
    }
}

@Composable
fun SharedSharingEnableSection() {
    var draft by remember { mutableStateOf(SharedSharingDraft()) }
    var saved by remember { mutableStateOf<Boolean?>(null) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Loading saved sharing choice…") }
    val scope = rememberCoroutineScope()
    suspend fun load() {
        busy = true
        val revision = draft.revision
        try {
            val client = SharedLibraryClient.create(); val value = client.settings(); client.requireCurrent()
            saved = value; draft = draft.received(value, revision); message = "Saved choice loaded."
        } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; message = "Saved choice unavailable: ${failure.message}. Choose a value and Save to set it explicitly." }
        finally { busy = false }
    }
    suspend fun save() {
        busy = true
        val value = draft.enabled; val revision = draft.revision
        try {
            val client = SharedLibraryClient.create(); val result = client.save(value); client.requireCurrent()
            saved = result; draft = draft.received(result, revision); message = "Sharing choice saved."
        } catch (failure: Exception) { if (failure is kotlinx.coroutines.CancellationException) throw failure; message = failure.message ?: "Sharing choice could not be saved." }
        finally { busy = false }
    }
    LaunchedEffect(Unit) { load() }
    Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
        Text("Shared libraries", style = MaterialTheme.typography.titleLarge)
        Row {
            Checkbox(checked = draft.enabled, onCheckedChange = { draft = draft.choose(it) }, modifier = Modifier.tvFocusRing())
            Text("Enable Shared libraries", Modifier.padding(top = 12.dp))
        }
        saved?.let { Text(if (it) "Saved choice: Enabled" else "Saved choice: Disabled") }
        tv.plurx.app.ui.components.Disclosure("Details") {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("Source-labelled browsing and Shared playback are implemented. Readiness never changes your selection or prevents Save.")
                Text(message)
                TextButton(enabled = !busy, onClick = { scope.launch { load() } }) { Text("Reload saved choice") }
            }
        }
        Button(enabled = !busy, onClick = { scope.launch { save() } }) { Text("Save sharing choice") }
    }
}
