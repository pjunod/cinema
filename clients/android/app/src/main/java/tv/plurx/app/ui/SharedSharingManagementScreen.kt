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
    var editingImport by remember { mutableStateOf<SharedSharingImport?>(null) }
    var editingEndpoints by remember { mutableStateOf<SharedSharingEndpointTarget?>(null) }
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
        if (secrets.snapshot() == null) { imports = emptyList(); exports = emptyList(); invitationId = null; editingExport = null; editingImport = null; editingEndpoints = null; confirmation = null }
    }
    LaunchedEffect(Unit) { load() }
    BackHandler { secrets.leave(); onBack() }
    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).verticalScroll(rememberScrollState()).padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TextButton(onClick = { secrets.leave(); onBack() }) { Text("Back") }
        Text("Sharing management", style = MaterialTheme.typography.headlineMedium)
        if (draft == null) Text("Sharing authorization changed or was refused. Leave and reopen management.")
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
                TextButton(enabled = !busy && row.`import`.state in listOf("claiming", "pending", "active"), onClick = { editingEndpoints = SharedSharingEndpointTarget.Source(row.`import`) }) { Text("Edit Source endpoints") }
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
                TextButton(enabled = !busy && row.`import`.state == "active", onClick = { editingImport = row }) { Text("Manage B viewer assignments") }
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
            TextButton(enabled = !busy, onClick = { editingEndpoints = SharedSharingEndpointTarget.Server }) { Text("Edit this server’s endpoints") }
            TextButton(enabled = !busy, onClick = { scope.launch { load() } }) { Text("Refresh management") }
            Text(message)
        }
    }
    confirmation?.let { action -> AlertDialog(onDismissRequest = { confirmation = null }, title = { Text(action.first) }, text = { Text("This changes access for this recipient or Source. Sharing enablement is unchanged.") },
        confirmButton = { Button(enabled = !busy, onClick = { confirmation = null; scope.launch { run { client -> action.second(client); if (secrets.snapshot() != null) message = "Access change saved. Refresh management for current state." } } }) { Text("Confirm") } },
        dismissButton = { TextButton(onClick = { confirmation = null }) { Text("Cancel") } }) }
    editingEndpoints?.let { target -> SharedSharingEndpointEditor(target, onDone = { editingEndpoints = null }) }
    editingImport?.let { row -> SharedSharingAssignmentEditor(row.`import`, onDone = { editingImport = null }) }
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

/** Complete authenticated admin reads are required before whole-replacement Save. */
@Composable private fun SharedSharingAssignmentEditor(source: SharedSharingImportSummary, onDone: () -> Unit) {
    val authority = remember(source.id) { SharedSharingSecretDraft() }
    val authorizationRevision by authority.invalidations.collectAsState()
    var current by remember { mutableStateOf<SharedSharingImportSummary?>(null) }
    var matrix by remember { mutableStateOf<SharedSharingAssignmentMatrix?>(null) }
    var editRevision by remember { mutableStateOf(0L) }
    var selectedLibrary by remember { mutableStateOf("") }
    var search by remember { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var saved by remember { mutableStateOf(false) }
    var readCurrent by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("") }
    val coroutine = rememberCoroutineScope()
    DisposableEffect(authority) { onDispose { authority.leave() } }
    LaunchedEffect(authorizationRevision) { if (authority.snapshot() == null) { matrix = null; current = null } }
    suspend fun load() {
        val requested = authority.snapshot() ?: return; if (busy) return; busy = true; readCurrent = false
        try {
            val client = SharedSharingManagementClient.create()
            val row = client.imports().firstOrNull { it.`import`.id == source.id }?.`import`
            require(row != null && row.source_server_id == source.source_server_id && row.catalogue_epoch == source.catalogue_epoch && row.state == "active")
            val assignments = client.assignments(row); val sourceScope = client.sourceLibraries(row); val viewers = client.viewers()
            val result = SharedSharingAssignmentMatrix(row, assignments, sourceScope, viewers)
            client.requireCurrent(); if (authority.accepts(requested.revision)) {
                current = row; matrix = result; selectedLibrary = result.libraries.firstOrNull()?.id.orEmpty(); editRevision = result.revision; saved = false; readCurrent = true; message = "Complete current matrix loaded."
            }
        } catch (failure: Exception) { if (failure is CancellationException) throw failure; message = "${failure.message}. Existing edits are retained; no empty matrix was inferred." }
        finally { busy = false }
    }
    fun edit(change: (SharedSharingAssignmentMatrix) -> Unit) {
        val draft = matrix ?: return; if (authority.snapshot() == null || busy || saved) return
        try { change(draft); editRevision = draft.revision; authority.edit() }
        catch (failure: Exception) { message = failure.message ?: "Invalid assignment edit" }
    }
    suspend fun save() {
        val requested = authority.snapshot() ?: return; val row = current ?: return; val draft = matrix ?: return
        if (busy || saved || !readCurrent) return; val requestedEdit = draft.revision; val groups = draft.groups.toList(); busy = true
        try {
            val client = SharedSharingManagementClient.create(); client.saveAssignments(draft.snapshot, row, groups)
            client.requireCurrent(); if (authority.accepts(requested.revision) && matrix?.accepts(requestedEdit) == true) { saved = true; message = "Assignments saved. Reload current generations before another change." }
        } catch (failure: Exception) { if (failure is CancellationException) throw failure; message = "${failure.message}. No automatic retry; refresh and review before saving again." }
        finally { busy = false }
    }
    LaunchedEffect(Unit) { load() }
    AlertDialog(onDismissRequest = { authority.leave(); matrix = null; onDone() }, title = { Text("${source.source_name} · Source viewers") }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text("${source.source_server_id} · ${source.catalogue_epoch}")
            val draft = matrix
            if (authority.snapshot() == null) Text("Sharing authorization changed or was refused. Close and reopen this Source.")
            else if (draft == null) Text("No replacement matrix is available until all current reads succeed.")
            else {
                Text("Assignment generation ${draft.snapshot.expected_assignment_generation}")
                Text("Libraries outside current Source scope and unavailable viewers remain assigned until explicitly removed.")
                draft.libraries.forEach { library -> TextButton(onClick = { selectedLibrary = library.id }) { Text(if (selectedLibrary == library.id) "Selected: ${library.name}" else library.name) } }
                draft.libraries.firstOrNull { it.id == selectedLibrary }?.let { library ->
                    Text("Source library ID: ${library.id}")
                    if (library.outsideScope) TextButton(enabled = !busy && !saved, onClick = { edit { it.removeOutsideScope(library.id) } }) { Text("Remove this outside-scope assignment group") }
                    OutlinedTextField(value = search, onValueChange = { search = it }, label = { Text("Find B viewer by name or ID") })
                    val visible = remember(draft, editRevision, search) { draft.viewers.filter { search.isEmpty() || it.username.contains(search, ignoreCase = true) || it.id.toString().contains(search) } }
                    visible.take(100).forEach { viewer -> Row {
                        Checkbox(checked = draft.contains(library.id, viewer.id), enabled = !busy && !saved, onCheckedChange = { enabled -> edit { it.set(library.id, viewer.id, enabled) } }, modifier = Modifier.tvFocusRing())
                        Text("${viewer.username} · ${viewer.id}", Modifier.padding(top = 12.dp))
                    } }
                    if (visible.size > 100) Text("Showing 100 of ${visible.size} matching viewers. Refine the search; hidden assignments remain in the complete matrix.")
                }
                if (draft.libraries.isEmpty()) Text("Source scope is explicitly empty and no saved assignment groups exist.")
                Button(enabled = !busy && !saved && readCurrent, onClick = { coroutine.launch { save() } }) { Text("Save complete viewer assignments") }
            }
            Text(message)
            TextButton(enabled = !busy, onClick = { coroutine.launch { load() } }) { Text("Reload current matrix") }
        }
    }, confirmButton = { TextButton(onClick = { authority.leave(); matrix = null; onDone() }) { Text("Done") } })
}

private sealed interface SharedSharingEndpointTarget {
    data object Server : SharedSharingEndpointTarget
    data class Source(val row: SharedSharingImportSummary) : SharedSharingEndpointTarget
}
@Composable private fun SharedSharingEndpointEditor(target: SharedSharingEndpointTarget, onDone: () -> Unit) {
    val authority = remember(target) { SharedSharingSecretDraft() }
    val authorizationRevision by authority.invalidations.collectAsState()
    var fields by remember { mutableStateOf<List<SharedSharingEndpointFields>>(emptyList()) }
    var source by remember { mutableStateOf<SharedSharingImportSummary?>(null) }
    var revision by remember { mutableStateOf(0L) }
    var confirmPins by remember { mutableStateOf(false) }
    var readCurrent by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var saved by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("") }
    val coroutine = rememberCoroutineScope()
    val endpoints = runCatching { fields.map { it.validated() } }.getOrNull()
    val oldPins = source?.endpoints?.map { it.spki_sha256 }?.toSet().orEmpty()
    val needsPinConfirmation = target is SharedSharingEndpointTarget.Server || endpoints == null || endpoints.any { it.spki_sha256 !in oldPins }
    fun leave() { authority.leave(); fields = emptyList(); source = null; confirmPins = false; onDone() }
    fun edited() { confirmPins = false; authority.edit() }
    DisposableEffect(authority) { onDispose { authority.leave() } }
    LaunchedEffect(authorizationRevision) { if (authority.snapshot() == null) { fields = emptyList(); source = null; confirmPins = false; readCurrent = false } }
    suspend fun load() {
        val request = authority.snapshot() ?: return; if (busy) return; busy = true; readCurrent = false
        try {
            val client = SharedSharingManagementClient.create()
            when (target) {
                SharedSharingEndpointTarget.Server -> {
                    val manifest = client.endpoints(); client.requireCurrent()
                    if (!authority.accepts(request.revision)) return
                    revision = manifest?.revision ?: 0; fields = manifest?.endpoints?.map(::SharedSharingEndpointFields) ?: listOf(SharedSharingEndpointFields())
                }
                is SharedSharingEndpointTarget.Source -> {
                    val row = client.imports().firstOrNull { it.`import`.id == target.row.id }?.`import`
                    require(row != null && row.source_server_id == target.row.source_server_id && row.catalogue_epoch == target.row.catalogue_epoch && row.state in listOf("claiming", "pending", "active"))
                    client.requireCurrent(); if (!authority.accepts(request.revision)) return
                    source = row; fields = row.endpoints.map(::SharedSharingEndpointFields)
                }
            }
            confirmPins = false; saved = false; readCurrent = true; message = "Current endpoint snapshot loaded. Review exact pins before saving."
        } catch (failure: Exception) { if (failure is CancellationException) throw failure; message = "${failure.message}. Draft retained; Save waits for a successful current read." }
        finally { busy = false }
    }
    suspend fun save() {
        val request = authority.snapshot() ?: return; if (busy || saved || !readCurrent) return; busy = true
        try {
            val values = fields.map { it.validated() }; require(!needsPinConfirmation || confirmPins)
            val client = SharedSharingManagementClient.create()
            when (target) {
                SharedSharingEndpointTarget.Server -> client.saveManifest(revision, values)
                is SharedSharingEndpointTarget.Source -> client.saveSourceEndpoints(requireNotNull(source), values, confirmPins)
            }
            client.requireCurrent(); if (authority.accepts(request.revision)) { saved = true; message = "Endpoints saved. Reload the current revision or generation before another change." }
        } catch (failure: Exception) { if (failure is CancellationException) throw failure; message = "${failure.message}. No automatic retry; reload and review before explicitly saving again." }
        finally { busy = false }
    }
    LaunchedEffect(Unit) { load() }
    AlertDialog(onDismissRequest = ::leave, title = { Text("Sharing endpoints") }, text = {
        Column(Modifier.verticalScroll(rememberScrollState()), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            if (authority.snapshot() == null) Text("Sharing authorization changed or was refused. Close and reopen endpoint management.")
            else {
                when (target) {
                    SharedSharingEndpointTarget.Server -> {
                        Text("This B server’s advertised private Tailnet endpoints · revision $revision")
                        Text("Use this server’s configured TLS certificate pin. Saving metadata does not change its certificate or network routing.")
                    }
                    is SharedSharingEndpointTarget.Source -> {
                        Text(target.row.source_name); Text("${target.row.source_server_id} · ${target.row.catalogue_epoch}")
                        Text("Current endpoint generation ${source?.endpoint_generation ?: target.row.endpoint_generation}")
                    }
                }
                Text("Enter one to four private Tailnet endpoints. Pins are exact 64-character lowercase SHA-256 SPKI fingerprints.")
                fields.forEach { field -> key(field.id) {
                    fun change(value: SharedSharingEndpointFields) { fields = fields.map { if (it.id == field.id) value else it }; edited() }
                    val enabled = !busy && !saved && readCurrent
                    SharedSharingEndpointField("Tailnet IPv4 (100.64.0.0/10)", field.ipv4, enabled) { change(field.copy(ipv4 = it)) }
                    SharedSharingEndpointField("Tailnet IPv6 (optional)", field.ipv6, enabled) { change(field.copy(ipv6 = it)) }
                    SharedSharingEndpointField("Machine.tailnet.ts.net", field.fqdn, enabled) { change(field.copy(fqdn = it)) }
                    SharedSharingEndpointField("Port", field.port, enabled) { change(field.copy(port = it)) }
                    SharedSharingEndpointField("TLS SPKI SHA-256", field.pin, enabled) { change(field.copy(pin = it)) }
                    Text("Exact pin: ${field.pin}")
                    TextButton(enabled = enabled && fields.size > 1, onClick = { fields = fields.filterNot { it.id == field.id }; edited() }) { Text("Remove endpoint") }
                } }
                TextButton(enabled = !busy && !saved && readCurrent && fields.size < 4, onClick = { fields = fields + SharedSharingEndpointFields(); edited() }) { Text("Add endpoint") }
                Row {
                    Checkbox(checked = confirmPins, enabled = !busy && !saved && readCurrent, onCheckedChange = { confirmPins = it; authority.edit() }, modifier = Modifier.tvFocusRing())
                    Text("I explicitly checked and confirm the exact TLS pins shown above.", Modifier.padding(top = 12.dp))
                }
                if (!needsPinConfirmation) Text("Pins match current Source pins; a new-pin confirmation is not required.")
                Button(enabled = !busy && !saved && readCurrent && (!needsPinConfirmation || confirmPins), onClick = { coroutine.launch { save() } }) { Text("Save endpoints") }
                TextButton(enabled = !busy, onClick = { coroutine.launch { load() } }) { Text("Reload current endpoints") }
                Text(message)
            }
        }
    }, confirmButton = { TextButton(onClick = ::leave) { Text("Done") } })
}
@Composable private fun SharedSharingEndpointField(label: String, value: String, enabled: Boolean, onChange: (String) -> Unit) {
    OutlinedTextField(value = value, onValueChange = onChange, enabled = enabled, label = { Text(label) },
        keyboardOptions = KeyboardOptions(capitalization = KeyboardCapitalization.None, autoCorrectEnabled = false), modifier = Modifier.fillMaxWidth())
}
