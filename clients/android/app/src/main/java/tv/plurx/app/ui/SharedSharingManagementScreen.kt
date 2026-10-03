package tv.plurx.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import tv.plurx.app.data.*
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.components.tvFocusRing

@Composable
internal fun SharedSharingManagementScreen(onBack: () -> Unit) {
    val secrets = remember { SharedSharingSecretDraft() }
    val draftRevision by secrets.invalidations.collectAsState()
    val draft = remember(draftRevision) { secrets.snapshot() }
    var libraries by remember { mutableStateOf<List<SharedSharingLocalLibrary>>(emptyList()) }
    var imports by remember { mutableStateOf<List<SharedSharingImport>>(emptyList()) }
    var exports by remember { mutableStateOf<List<SharedSharingExport>>(emptyList()) }
    var manifest by remember { mutableStateOf<SharedSharingEndpointManifest?>(null) }
    var selected by remember { mutableStateOf<Set<String>>(emptySet()) }
    var nextExport by remember { mutableStateOf<String?>(null) }
    val cursors = remember { mutableSetOf<String>() }
    var invitationId by remember { mutableStateOf<String?>(null) }
    var editingExport by remember { mutableStateOf<SharedSharingExport?>(null) }
    var confirmation by remember { mutableStateOf<Pair<String, suspend (SharedSharingManagementClient) -> Unit>?>(null) }
    var message by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    suspend fun run(operation: suspend (SharedSharingManagementClient) -> Unit) {
        if (secrets.snapshot() == null || busy) return
        busy = true
        try { operation(SharedSharingManagementClient.create()) }
        catch (failure: Exception) { if (failure is CancellationException) throw failure; message = "${failure.message ?: "Management request failed"}. Refresh current generations before retrying explicitly." }
        finally { busy = false }
    }
    suspend fun load() = run { client ->
        val local = client.libraries(); val imported = client.imports(); val page = client.exports(); val endpoints = client.endpoints()
        client.requireCurrent(); if (secrets.snapshot() != null) {
            libraries = local; imports = imported; exports = page.rows; nextExport = page.next; cursors.clear(); manifest = endpoints; message = "Current management snapshot loaded."
        }
    }
    DisposableEffect(secrets) { onDispose { secrets.leave() } }
    LaunchedEffect(draftRevision) {
        if (secrets.snapshot() == null) { imports = emptyList(); exports = emptyList(); invitationId = null; editingExport = null; confirmation = null }
    }
    LaunchedEffect(Unit) { load() }
    BackHandler { secrets.leave(); onBack() }
    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TextButton(onClick = { secrets.leave(); onBack() }) { Text("Back") }
        Text("Sharing management", style = MaterialTheme.typography.headlineMedium)
        if (draft == null) Text("This account changed. Leave and reopen Sharing management.")
        else {
            Text("Source invitations", style = MaterialTheme.typography.titleLarge)
            Text("Select this server's movie/show libraries. An invitation grants no access until the recipient's pairing code is explicitly approved.")
            libraries.forEach { library ->
                Row {
                    Checkbox(checked = library.id.toString() in selected, onCheckedChange = { value ->
                        selected = if (value) selected + library.id.toString() else selected - library.id.toString(); secrets.edit()
                    }, modifier = Modifier.tvFocusRing())
                    Text(library.name, Modifier.padding(top = 12.dp))
                }
            }
            Button(enabled = !busy && selected.isNotEmpty(), onClick = { scope.launch {
                val requested = secrets.snapshot() ?: return@launch; val selection = selected.sorted()
                run { client ->
                    val result = client.invite(selection); client.requireCurrent()
                    if (secrets.snapshot() != null) {
                        invitationId = result.id
                        if (secrets.accepts(requested.revision)) secrets.edit(invitation = result.invitation)
                        message = "Invitation created. Compare recipient pairing codes before approval. A newer edit is preserved."
                    }
                }
            } }) { Text("Create one-day invitation") }
            OutlinedTextField(keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false), value = draft.invitation, onValueChange = { secrets.edit(invitation = it) }, label = { Text("Invitation to import or copy") }, modifier = Modifier.fillMaxWidth())
            Text("Invitation material is transient and clears on leaving or account changes.")
            invitationId?.let { id -> TextButton(enabled = !busy, onClick = { confirmation = "Cancel invitation" to { client -> client.cancelInvitation(id); client.requireCurrent(); if (secrets.snapshot() != null) { secrets.snapshot()?.let { secrets.clear(it.revision) }; invitationId = null } } }) { Text("Cancel this invitation") } }
            Button(enabled = !busy && draft.invitation.isNotEmpty(), onClick = { scope.launch {
                val requested = secrets.snapshot() ?: return@launch
                run { client ->
                    val result = client.importSource(requested.invitation); client.requireCurrent()
                    if (secrets.snapshot() != null) { imports = imports.filterNot { it.`import`.id == result.`import`.id } + result; secrets.clear(requested.revision); message = "Source imported. Compare pairing code with the exporting server." }
                }
            } }) { Text("Import entered invitation") }
            Text("Imported Sources", style = MaterialTheme.typography.titleLarge)
            imports.forEach { row ->
                Text(row.`import`.source_name, style = MaterialTheme.typography.titleMedium)
                Text("${row.`import`.source_server_id} · ${row.`import`.catalogue_epoch}")
                Text("${row.`import`.state} · Pairing code: ${row.pairing_code}")
                Text("Compare this code with the exporting server before approval.")
                SharedSharingEndpointSummary(row.`import`.endpoints)
                Text("Lifecycle ${row.`import`.lifecycle_generation} · Assignments ${row.`import`.assignment_generation} · Endpoints ${row.`import`.endpoint_generation}")
                TextButton(enabled = !busy && row.`import`.state == "active", onClick = { scope.launch { run { client ->
                    val result = client.rotate(row.`import`); client.requireCurrent(); if (secrets.snapshot() != null) { imports = imports.filterNot { it.`import`.id == result.`import`.id } + result; message = "Credential rotation completed." }
                } } }) { Text("Rotate credential") }
                TextButton(enabled = !busy && draft.invitation.isNotEmpty(), onClick = { scope.launch {
                    val requested = secrets.snapshot() ?: return@launch
                    run { client ->
                        val result = client.rePair(row.`import`, requested.invitation); client.requireCurrent()
                        if (secrets.snapshot() != null) { imports = imports.filterNot { it.`import`.id == result.`import`.id } + result; secrets.clear(requested.revision); message = "Source re-paired. Compare current pairing code." }
                    }
                } }) { Text("Re-pair using entered invitation") }
                Text("Viewer assignments require a complete current assignment snapshot; an empty matrix is never inferred.")
                TextButton(enabled = !busy, onClick = { confirmation = "Disconnect ${row.`import`.source_name}" to { client -> client.disconnect(row.`import`.id); client.requireCurrent() } }) { Text("Disconnect Source") }
            }
            if (imports.isEmpty()) Text("No imported Sources.")
            Text("Exports", style = MaterialTheme.typography.titleLarge)
            exports.forEach { row ->
                TextButton(onClick = { editingExport = row }) { Text("${row.recipient_name} · ${row.grant.state}") }
                TextButton(enabled = !busy, onClick = { confirmation = "Revoke ${row.recipient_name}" to { client -> client.revoke(row.grant.id); client.requireCurrent() } }) { Text("Revoke export") }
            }
            nextExport?.let { cursor -> TextButton(enabled = !busy, onClick = { scope.launch { run { client ->
                require(cursor !in cursors); val page = client.exports(cursor); client.requireCurrent()
                if (secrets.snapshot() != null && nextExport == cursor) { cursors += cursor; val known = exports.map { it.grant.id }.toSet(); exports += page.rows.filter { it.grant.id !in known }; nextExport = page.next?.takeUnless { it in cursors } }
            } } }) { Text("Load more recipients") } }
            Text("This server's endpoints", style = MaterialTheme.typography.titleLarge)
            manifest?.let { Text("Revision ${it.revision}"); SharedSharingEndpointSummary(it.endpoints) } ?: Text("No configured endpoint manifest.")
            Text("Endpoint changes require current generations and explicit pin confirmation. This screen shows validated summaries.")
            TextButton(enabled = !busy, onClick = { scope.launch { load() } }) { Text("Refresh management") }
            Text(message)
        }
    }
    confirmation?.let { action -> AlertDialog(onDismissRequest = { confirmation = null }, title = { Text(action.first) }, text = { Text("This changes access for this recipient or Source. Sharing enablement is unchanged.") },
        confirmButton = { Button(enabled = !busy, onClick = { confirmation = null; scope.launch { run { client -> action.second(client); if (secrets.snapshot() != null) message = "Access change saved. Refresh management for current state." } } }) { Text("Confirm") } },
        dismissButton = { TextButton(onClick = { confirmation = null }) { Text("Cancel") } }) }
    editingExport?.let { row -> SharedSharingExportEditor(row, libraries, onDone = { editingExport = null }) }
}

@Composable private fun SharedSharingEndpointSummary(endpoints: List<SharedSharingEndpoint>) {
    endpoints.forEach { Text("${it.ts_fqdn}:${it.port} · ${it.ipv4}"); Text("TLS pin: ${it.spki_sha256}") }
}
@Composable private fun SharedSharingExportEditor(row: SharedSharingExport, libraries: List<SharedSharingLocalLibrary>, onDone: () -> Unit) {
    val secrets = remember(row.grant.id) { SharedSharingSecretDraft() }
    val revision by secrets.invalidations.collectAsState(); val draft = remember(revision) { secrets.snapshot() }
    var selected by remember(row.grant.id) { mutableStateOf(row.library_ids.toSet()) }
    var busy by remember { mutableStateOf(false) }; var message by remember { mutableStateOf("") }; val scope = rememberCoroutineScope()
    DisposableEffect(secrets) { onDispose { secrets.leave() } }
    LaunchedEffect(revision) { if (secrets.snapshot() == null) onDone() }
    suspend fun mutate(approve: Boolean) {
        val requested = secrets.snapshot() ?: return; if (busy) return; busy = true
        val selection = selected.sorted()
        try {
            val client = SharedSharingManagementClient.create()
            if (approve) client.approve(row, requested.pairingCode) else client.scope(row, selection)
            client.requireCurrent(); if (secrets.snapshot() != null) { if (approve) secrets.clear(requested.revision); message = "Saved. Return and refresh current generations before another change." }
        } catch (failure: Exception) { if (failure is CancellationException) throw failure; message = failure.message ?: "Management mutation failed" }
        finally { busy = false }
    }
    AlertDialog(onDismissRequest = onDone, title = { Text(row.recipient_name) }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(row.grant.recipient_server_id); Text("Mutation generation ${row.grant.mutation_generation}")
            if (draft != null) {
                Text("Read the importing server's code and enter it here. Approve only the intended recipient.")
                OutlinedTextField(keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false), value = draft.pairingCode, onValueChange = { secrets.edit(pairingCode = it) }, label = { Text("Recipient's 16-character code") })
                Button(enabled = !busy && row.grant.state == "pending", onClick = { scope.launch { mutate(true) } }) { Text("Approve entered code") }
                libraries.forEach { library -> Row {
                    Checkbox(checked = library.id.toString() in selected, onCheckedChange = { value -> selected = if (value) selected + library.id.toString() else selected - library.id.toString(); secrets.edit() }, modifier = Modifier.tvFocusRing())
                    Text(library.name, Modifier.padding(top = 12.dp))
                } }
                Button(enabled = !busy, onClick = { scope.launch { mutate(false) } }) { Text("Save selected scope") }
                Text(message)
            }
        }
    }, confirmButton = { TextButton(onClick = onDone) { Text("Done") } })
}
