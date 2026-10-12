package tv.plurx.app.diagnostics

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import tv.plurx.app.player.isTelevision
import tv.plurx.app.remote.RemoteRuntime

@Composable
internal fun DiagnosticReportButton(category: String = "other", positiveTitle: String? = null, generation: Long? = null) {
    TextButton(onClick = {
        val owned = generation != null && generation == tv.plurx.app.data.Session.playbackAuthorization().generation
        DiagnosticRuntime.open(category,if (owned) positiveTitle else null)
    },modifier=Modifier.testTag("diagnostics-open")) { Text("Report a problem") }
}

/** A native focusable dialog owns only report actions. Cinema companion
 * navigation is blocked before it can select or approve a phone. */
@Composable
internal fun DiagnosticReportHost() {
    val state by DiagnosticRuntime.state.collectAsStateWithLifecycle()
    val context=LocalContext.current; val remote=remember(context) { RemoteRuntime.get(context) }
    DisposableEffect(state.visible,remote) {
        remote.diagnosticRestricted(state.visible)
        onDispose { remote.diagnosticRestricted(false) }
    }
    if (!state.visible) return
    val television=isTelevision(context)
    Dialog(onDismissRequest={ DiagnosticRuntime.close() },properties=DialogProperties(usePlatformDefaultWidth=false)) {
        Surface(Modifier.fillMaxWidth(if (television) 0.82f else 0.96f).fillMaxHeight(0.92f),shape=MaterialTheme.shapes.large) {
            Column(Modifier.padding(if (television) 28.dp else 16.dp).verticalScroll(rememberScrollState()),verticalArrangement=Arrangement.spacedBy(12.dp)) {
                Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
                    Text("Report a problem",style=MaterialTheme.typography.headlineSmall)
                    TextButton(onClick={ DiagnosticRuntime.close() }) { Text("Close") }
                }
                Text(state.message,modifier=Modifier.testTag("diagnostics-status"))
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.draft?.let { draft ->
                    DiagnosticIncidentFields(state,draft)
                    if (!television) Button(onClick={ DiagnosticRuntime.share() },enabled=!state.busy) { Text("Save / share local report") }
                    if (DiagnosticRuntime.recorder.recording) Button(onClick={ DiagnosticRuntime.stopReproduction() }) { Text("Stop reproduction recording") }
                    else Button(onClick={ DiagnosticRuntime.startReproduction() },enabled=!draft.consented && !state.busy) { Text("Record reproduction (five minutes)") }
                    Text("Recording stops within five minutes or on background. Closing keeps this draft. It does not upload until you choose Send.",style=MaterialTheme.typography.bodySmall)
                    Button(onClick={ DiagnosticRuntime.checks() },enabled=!draft.consented && !state.busy) { Text("Check this Cinema connection (15 seconds)") }
                    Text("Checks contact only the configured Cinema endpoint, with a total 2 MiB reply ceiling. No media transfer probe is started.",style=MaterialTheme.typography.bodySmall)
                    TextButton(onClick={ DiagnosticRuntime.systemSnapshot() },enabled=!draft.consented && !state.busy) { Text("Collect administrator system snapshot") }
                    Text("Requires a Cinema administrator account.",style=MaterialTheme.typography.bodySmall)
                    DiagnosticDeliveryFields(state,draft)
                }
                HorizontalDivider()
                Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
                    Text("Keep recent local history",Modifier.weight(1f))
                    Switch(checked=state.history,onCheckedChange={ DiagnosticRuntime.history(it) })
                }
                Text("Local captures expire within 24 hours. Clearing local data does not remove an already submitted support report.",style=MaterialTheme.typography.bodySmall)
                state.pending.forEach { pending ->
                    TextButton(onClick={ DiagnosticRuntime.resume(pending) }) { Text("Resume ${pending.capture.capturedAt} · ${pending.state}") }
                    TextButton(onClick={ DiagnosticRuntime.deleteLocal(pending) }) { Text("Delete local ${pending.capture.capturedAt}") }
                }
            }
        }
    }
}

@Composable
private fun DiagnosticIncidentFields(state: DiagnosticUI,draft: DiagnosticDraft) {
    Text("Report ${draft.id}",style=MaterialTheme.typography.bodySmall)
    Row(horizontalArrangement=Arrangement.spacedBy(8.dp)) {
        listOf("playback" to "Playback","connection" to "Connection","interface" to "Interface","other" to "Other").forEach { (value,label) ->
            FilterChip(selected=draft.category == value,onClick={ DiagnosticRuntime.edit(category=value) },label={ Text(label) },enabled=!draft.consented)
        }
    }
    OutlinedTextField(value=draft.description,onValueChange={ DiagnosticRuntime.edit(description=it) },label={ Text("What happened? Optional user content") },enabled=!draft.consented,modifier=Modifier.fillMaxWidth(),minLines=2,maxLines=6)
    Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
        Text("Include the current title",Modifier.weight(1f))
        Switch(checked=state.includeTitle,onCheckedChange={ DiagnosticRuntime.includeTitle(it) },enabled=!draft.consented)
    }
    Text("No passwords, addresses, request bodies, media or raw stream identifiers are collected. A description and an optional title are private content.",style=MaterialTheme.typography.bodySmall)
    Text("Client: ${if (!draft.capture.historyEnabled) "history disabled" else if (draft.capture.dropped != "0") "partial (overflow)" else "bounded history"}; persistence: ${draft.capture.persistence}",style=MaterialTheme.typography.bodySmall)
    draft.components.forEach { Text("${it.name}: ${it.state} ${it.reason.orEmpty()}",style=MaterialTheme.typography.bodySmall) }
    Text("Platform evidence: ${draft.platformReason ?: "normalized evidence available"}. Clock alignment: unavailable.",style=MaterialTheme.typography.bodySmall)
    var preview by remember(draft.id) { mutableStateOf(false) }
    TextButton(onClick={ preview=!preview }) { Text(if (preview) "Hide safe event preview" else "Preview safe event fields") }
    if (preview) draft.capture.events.takeLast(30).forEach { event -> Text("${event.utc} ${event.domain}/${event.kind} ${event.facts}",style=MaterialTheme.typography.bodySmall) }
}

@Composable
private fun DiagnosticDeliveryFields(state: DiagnosticUI,draft: DiagnosticDraft) {
    HorizontalDivider(); Text("Send to support",style=MaterialTheme.typography.titleMedium)
    OutlinedTextField(value=state.destination,onValueChange={ DiagnosticRuntime.destination(it) },label={ Text("HTTPS support origin") },enabled=!draft.consented,modifier=Modifier.fillMaxWidth(),singleLine=true)
    Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
        Text("I approve uploading to ${state.destination.ifEmpty { "this destination" }}",Modifier.weight(1f))
        Switch(checked=state.consent,onCheckedChange={ DiagnosticRuntime.consent(it) })
    }
    Text("Upload is separate from Cinema. A disconnected enrollment expires within 24 hours; a new offline installation needs a one-use support invitation.",style=MaterialTheme.typography.bodySmall)
    OutlinedTextField(value=state.invitation,onValueChange={ DiagnosticRuntime.invitation(it) },label={ Text("Optional one-use invitation") },enabled=!draft.consented,singleLine=true)
    TextButton(onClick={ DiagnosticRuntime.redeem() },enabled=!state.busy && state.consent && state.invitation.isNotEmpty()) { Text("Use invitation") }
    Button(onClick={ DiagnosticRuntime.send() },enabled=!state.busy && state.consent) { Text("Send report") }
    Button(onClick={ DiagnosticRuntime.send(phone=true) },enabled=!state.busy && state.consent) { Text("Finish on a phone") }
    draft.handoff?.let { DiagnosticHandoff(state,draft,it) }
    TextButton(onClick={ DiagnosticRuntime.discard() },enabled=!state.busy) { Text("Cancel handoff / delete staged draft") }
    if (draft.handoff != null) TextButton(onClick={ DiagnosticRuntime.discard(keep=true) },enabled=!state.busy) { Text("Cancel handoff and keep staged draft") }
}

@Composable
private fun DiagnosticHandoff(state: DiagnosticUI,draft: DiagnosticDraft,offer: DiagnosticOffer) {
    val bitmap=remember(offer.id) {
        runCatching {
            require(offer.url.length <= 1024); val grid=QRCodeWriter().encode(offer.url,BarcodeFormat.QR_CODE,256,256)
            android.graphics.Bitmap.createBitmap(256,256,android.graphics.Bitmap.Config.ARGB_8888).also { image ->
                val pixels=IntArray(65536) { index -> if (grid[index%256,index/256]) android.graphics.Color.BLACK else android.graphics.Color.WHITE }
                image.setPixels(pixels,0,256,0,0,256,256)
            }
        }.getOrNull()
    }
    if (bitmap != null) Image(bitmap.asImageBitmap(),"Scan to claim this report on your phone",Modifier.size(256.dp))
    Text("Open ${draft.origin} on the phone. Manual code: ${offer.manual_code}",style=MaterialTheme.typography.titleMedium)
    Text("Claim expires in ten minutes. Keep this screen open and compare the phrase on both devices.")
    Text("Handoff: ${state.handoffState}")
    state.comparison?.let { phrase ->
        Text("Compare on phone: $phrase",style=MaterialTheme.typography.headlineMedium)
        Button(onClick={ DiagnosticRuntime.approve() },enabled=!state.busy,modifier=Modifier.testTag("diagnostics-approve-phone")) { Text("The phrase matches — approve phone") }
    }
    TextButton(onClick={ DiagnosticRuntime.pollHandoff() },enabled=!state.busy) { Text("Refresh handoff status") }
}
