package tv.plurx.app.diagnostics

import androidx.compose.foundation.layout.*
import androidx.compose.foundation.Image
import androidx.compose.ui.graphics.asImageBitmap
import android.graphics.Bitmap
import com.google.zxing.BarcodeFormat
import com.google.zxing.qrcode.QRCodeWriter
import androidx.compose.foundation.ScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import tv.plurx.app.player.isTelevision
import tv.plurx.app.remote.RemoteRuntime
import tv.plurx.app.ui.AuthTextField

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
            // A selected incident opens at its own coverage/privacy summary.
            val reportScroll = remember(state.draft?.id,state.selectionRevision) { ScrollState(0) }
            Column(Modifier.padding(if (television) 28.dp else 16.dp).verticalScroll(reportScroll),verticalArrangement=Arrangement.spacedBy(12.dp)) {
                Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
                    Text("Report a problem",style=MaterialTheme.typography.headlineSmall)
                    TextButton(onClick={ DiagnosticRuntime.close() }) { Text("Close") }
                }
                TextButton(onClick={ DiagnosticRuntime.open(newIncident=true) },enabled=!state.busy && !DiagnosticRuntime.recorder.recording) { Text("Capture a new incident") }
                Text(state.message,modifier=Modifier.testTag("diagnostics-status"))
                if (state.busy) LinearProgressIndicator(Modifier.fillMaxWidth())
                state.draft?.let { draft ->
                    DiagnosticIncidentFields(state,draft)
                    if (!television) {
                        Button(onClick={ DiagnosticRuntime.share() },enabled=!state.busy,modifier=Modifier.testTag("diagnostics-export")) { Text("Save / share diagnostic ZIP") }
                        Text("Attach the ZIP to your bug or issue report. Saving does not submit it.",style=MaterialTheme.typography.bodySmall)
                    } else {
                        Text("Use trusted Wi-Fi. This local download is unencrypted; anyone with its link can download the ZIP. It ends after one download or five minutes.",style=MaterialTheme.typography.bodySmall)
                        Button(onClick={ DiagnosticRuntime.shareOnLocalNetwork() },enabled=!state.busy,modifier=Modifier.testTag("diagnostics-export-tv")) { Text("Save TV ZIP on a phone") }
                        state.localExportURL?.let { url ->
                            val qr=remember(url) { diagnosticExportQR(url) }
                            Image(qr.asImageBitmap(),"Scan to save this TV's diagnostic ZIP",Modifier.size(256.dp))
                            Text(url,modifier=Modifier.testTag("diagnostics-export-url"),style=MaterialTheme.typography.bodySmall)
                            Text("Your phone must be on the same local network. Save the ZIP, then attach it to an issue.")
                            Button(onClick={ DiagnosticRuntime.stopLocalExport() },modifier=Modifier.testTag("diagnostics-stop-export")) { Text("Stop local download") }
                        }
                    }
                    if (DiagnosticRuntime.recorder.recording) Button(onClick={ DiagnosticRuntime.stopReproduction() }) { Text("Stop reproduction recording") }
                    else Button(onClick={ DiagnosticRuntime.startReproduction() },enabled=!draft.consented && !state.busy) { Text("Record reproduction (five minutes)") }
                    Text("Recording stops within five minutes or on background. Closing keeps this draft. Save the ZIP when you are ready to attach it to an issue.",style=MaterialTheme.typography.bodySmall)
                    Button(onClick={ DiagnosticRuntime.checks() },enabled=!draft.consented && !state.busy) { Text("Check this Cinema connection (15 seconds)") }
                    Text("Checks contact only the configured Cinema endpoint, with a total 2 MiB reply ceiling. No media transfer probe is started.",style=MaterialTheme.typography.bodySmall)
                    TextButton(onClick={ DiagnosticRuntime.systemSnapshot() },enabled=!draft.consented && !state.busy) { Text("Collect administrator system snapshot") }
                    Text("Requires a Cinema administrator account.",style=MaterialTheme.typography.bodySmall)
                }
                HorizontalDivider()
                Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
                    Text("Keep recent local history",Modifier.weight(1f))
                    Switch(checked=state.history,onCheckedChange={ DiagnosticRuntime.history(it) })
                }
                Text("Local captures expire within 24 hours. Clearing local data does not remove an already submitted support report.",style=MaterialTheme.typography.bodySmall)
                state.pending.forEach { pending ->
                    TextButton(onClick={ DiagnosticRuntime.resume(pending) },enabled=!state.busy,modifier=Modifier.testTag("diagnostics-resume-${pending.id}")) { Text("Resume ${pending.capture.capturedAt} · ${pending.state}") }
                    TextButton(onClick={ DiagnosticRuntime.deleteLocal(pending) }) { Text("Delete local ${pending.capture.capturedAt}") }
                }
            }
        }
    }
}

@Composable
private fun DiagnosticIncidentFields(state: DiagnosticUI,draft: DiagnosticDraft) {
    val titleFocus=remember(draft.id) { FocusRequester() }
    Text("Report ${draft.id}",style=MaterialTheme.typography.bodySmall)
    Row(horizontalArrangement=Arrangement.spacedBy(8.dp)) {
        listOf("playback" to "Playback","connection" to "Connection","interface" to "Interface","other" to "Other").forEach { (value,label) ->
            FilterChip(selected=draft.category == value,onClick={ DiagnosticRuntime.edit(category=value) },label={ Text(label) },enabled=!draft.consented && !state.busy)
        }
    }
    if (isTelevision(LocalContext.current) && !draft.consented && !state.busy) {
        // Reuse the TV select-to-edit field: D-pad traversal must reach privacy
        // controls instead of becoming cursor movement in a multiline editor.
        AuthTextField(value=draft.description,onValueChange={ DiagnosticRuntime.edit(description=it) },label="What happened? Optional user content",modifier=Modifier.fillMaxWidth().testTag("diagnostics-description"),downFocus=titleFocus)
    } else OutlinedTextField(value=draft.description,onValueChange={ DiagnosticRuntime.edit(description=it) },label={ Text("What happened? Optional user content") },enabled=!draft.consented && !state.busy,modifier=Modifier.fillMaxWidth().testTag("diagnostics-description"),minLines=2,maxLines=6)
    Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
        Text("Include the current title",Modifier.weight(1f))
        Switch(checked=state.includeTitle,onCheckedChange={ DiagnosticRuntime.includeTitle(it) },enabled=!draft.consented && !state.busy,modifier=Modifier.focusRequester(titleFocus).testTag("diagnostics-title-consent"))
    }
    Row(Modifier.fillMaxWidth(),horizontalArrangement=Arrangement.SpaceBetween) {
        Text("Include available platform diagnostics",Modifier.weight(1f))
        Switch(checked=state.includePlatform,onCheckedChange={ DiagnosticRuntime.includePlatform(it) },enabled=!draft.consented && !state.busy && draft.platform.isNotEmpty())
    }
    Text("No passwords, addresses, request bodies, media or raw stream identifiers are collected. A description and an optional title are private content.",style=MaterialTheme.typography.bodySmall)
    Text("Client: ${if (!draft.capture.historyEnabled) "history disabled" else if (draft.capture.dropped != "0") "partial (overflow)" else "bounded history"}; persistence: ${draft.capture.persistence}",style=MaterialTheme.typography.bodySmall)
    draft.components.forEach { Text("${it.name}: ${it.state} ${it.reason.orEmpty()}",style=MaterialTheme.typography.bodySmall) }
    Text("Platform evidence: ${draft.platformReason ?: "normalized evidence available"}. Clock alignment: unavailable.",style=MaterialTheme.typography.bodySmall)
    var preview by remember(draft.id) { mutableStateOf(false) }
    TextButton(onClick={ preview=!preview }) { Text(if (preview) "Hide safe event preview" else "Preview safe event fields") }
    if (preview) draft.capture.events.takeLast(30).forEach { event -> Text("${event.utc} ${event.domain}/${event.kind} ${event.facts}",style=MaterialTheme.typography.bodySmall) }
}

internal fun diagnosticExportQR(url: String): Bitmap {
    val matrix=QRCodeWriter().encode(url,BarcodeFormat.QR_CODE,256,256)
    val pixels=IntArray(matrix.width*matrix.height) { index -> if (matrix[index%matrix.width,index/matrix.width]) android.graphics.Color.BLACK else android.graphics.Color.WHITE }
    return Bitmap.createBitmap(pixels,matrix.width,matrix.height,Bitmap.Config.ARGB_8888)
}
