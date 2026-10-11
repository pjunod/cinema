package tv.plurx.app.invitations

import android.Manifest
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.platform.LocalContext
import tv.plurx.app.remote.RemoteClientModel

@Composable internal fun InvitationSettings(remote: RemoteClientModel, showChoices: Boolean = true) {
    val context = LocalContext.current
    val model = remember(context) { InvitationRuntime.get(context) }
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { model.updatePermission() }
    var remove by remember { mutableStateOf<InvitationPhone?>(null) }
    var reset by remember { mutableStateOf(false) }
    var forget by remember { mutableStateOf<InvitationSavedProfile?>(null) }
    Text(if (showChoices) "Background screen invitations" else "Screen invitation recovery", style = MaterialTheme.typography.titleLarge)
    Text("Each screen has its own saved choice and one transport. Delivery and device permissions still need qualification; readiness never changes your choice.")
    Text(model.status)
    if (showChoices && model.local?.installation == null && !model.corrupt) TextButton(onClick = model::register, enabled = !model.busy) { Text("Register this phone") }
    if (showChoices) model.local?.let { local ->
        if (local.lostProof) Text("Registration could not be confirmed and its one-time proof is unavailable. Inspect and remove the home installation before registering another.")
        TextButton(onClick = model::rebind, enabled = !model.busy && local.phoneSecret != null) { Text("Rebind to this login") }
        TextButton(onClick = {
            if (Build.VERSION.SDK_INT >= 33) permission.launch(Manifest.permission.POST_NOTIFICATIONS) else model.updatePermission()
        }, enabled = !model.busy && local.phoneSecret != null) { Text("Allow notifications / refresh permission") }
        model.screenIds.forEach { receiver ->
            val choice = local.choices.firstOrNull { it.receiver == receiver }
            val consent = local.consents.firstOrNull { it.receiver_id == receiver }
            Text(remote.devices.firstOrNull { it.id == receiver }?.name ?: "Paired screen", style = MaterialTheme.typography.titleMedium)
            Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
                Switch(checked = choice?.enabled == true, onCheckedChange = { model.choose(receiver, it, choice?.transport ?: "fcm") })
                Text("Invite this phone when the screen opens")
            }
            Row {
                TextButton(onClick = { model.choose(receiver, choice?.enabled == true, "fcm") }) { Text(if (choice?.transport != "android_resident") "✓ Push" else "Push") }
                TextButton(onClick = { model.choose(receiver, choice?.enabled == true, "android_resident") }) { Text(if (choice?.transport == "android_resident") "✓ Resident" else "Resident") }
            }
            TextButton(onClick = { model.save(receiver) }, enabled = !model.busy && local.phoneSecret != null) { Text("Save this screen to home") }
            Text(consent?.readiness?.status?.replace('_', ' ') ?: "Not saved to home")
            if (choice?.transport != "android_resident") {
                Text(if (InvitationFirebase.configured) "Push configured in this build; physical delivery unverified." else "Push is unconfigured in this build. ON stays saved. Select Resident explicitly as an alternative.")
                TextButton(onClick = { model.enroll(receiver) }, enabled = !model.busy && choice?.enabled == true) { Text("Start / renew push enrollment") }
            }
        }
        Row(verticalAlignment = androidx.compose.ui.Alignment.CenterVertically) {
            Switch(local.residentChoice, model::residentChoice)
            Text("Allow user-started resident receiver")
        }
        Text(if (local.runRequested && local.phone?.resident_active == true) "Resident receiver reports active; physical background delivery remains unverified." else if (local.runRequested) "Resident start requested; awaiting Wi-Fi and home availability." else "Resident receiver is stopped locally.")
        Text("Starting and stopping this local receiver does not change any screen's invitation choice. Wi-Fi and notifications are required. It never restarts itself.")
        Row {
            TextButton(onClick = model::startResident) { Text("Start resident receiver") }
            TextButton(onClick = model::stopResident) { Text("Stop locally now") }
        }
        TextButton(onClick = model::refresh, enabled = !model.busy && local.phoneSecret != null) { Text("Refresh screen readiness") }
    }
    model.savedProfiles.forEach { saved ->
        Text("Saved profile " + saved.origin + " · account " + saved.account)
        TextButton(onClick = { forget = saved }) { Text("Forget this local saved profile") }
    }
    forget?.let { saved -> AlertDialog(onDismissRequest = { forget = null }, title = { Text("Forget local profile?") },
        text = { Text("This removes only local invitation records. Home installations may remain and require signed-in list/delete recovery at " + saved.origin + ".") },
        confirmButton = { TextButton(onClick = { model.forgetProfile(saved); forget = null }) { Text("Forget locally") } },
        dismissButton = { TextButton(onClick = { forget = null }) { Text("Cancel") } }) }
    Text("Recovery", style = MaterialTheme.typography.titleMedium)
    Text("While signed in, list this account's home installations even if this phone's proof is lost. Removing a row revokes that specific phone installation.")
    TextButton(onClick = { model.listInstallations() }, enabled = !model.busy) { Text("List home phone installations") }
    model.installations.forEach { phone ->
        Text(phone.name + " · " + phone.installation_id)
        TextButton(onClick = { remove = phone }, enabled = !model.busy) { Text("Remove this installation") }
    }
    if (model.nextCursor != null) TextButton(onClick = { model.listInstallations(model.nextCursor) }, enabled = !model.busy) { Text("Next installations") }
    TextButton(onClick = { reset = true }, enabled = !model.busy) { Text("Reset local invitation records") }
    if (model.corrupt) TextButton(onClick = model::resetIndex) { Text("Reset corrupt local profile index") }
    remove?.let { phone -> AlertDialog(onDismissRequest = { remove = null }, title = { Text("Remove " + phone.name + "?") },
        text = { Text("This revokes only installation " + phone.installation_id + ".") },
        confirmButton = { TextButton(onClick = { model.removeInstallation(phone.installation_id); remove = null }) { Text("Remove") } },
        dismissButton = { TextButton(onClick = { remove = null }) { Text("Cancel") } }) }
    if (reset) AlertDialog(onDismissRequest = { reset = false }, title = { Text("Reset local records?") },
        text = { Text("Home installations may remain. No remote revocation is claimed; use the signed-in list to remove orphan installations.") },
        confirmButton = { TextButton(onClick = { model.resetLocal(); reset = false }) { Text("Reset locally") } },
        dismissButton = { TextButton(onClick = { reset = false }) { Text("Cancel") } })
}
