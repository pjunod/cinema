package tv.plurx.app.remote

import android.graphics.Bitmap
import androidx.compose.foundation.Image
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.onClick
import androidx.compose.ui.semantics.role
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import com.google.mlkit.vision.barcode.common.Barcode
import com.google.mlkit.vision.codescanner.GmsBarcodeScannerOptions
import com.google.mlkit.vision.codescanner.GmsBarcodeScanning
import kotlinx.serialization.json.*
import java.net.URI
import java.net.URLDecoder

internal object RemotePairingQr {
    fun parse(payload: String, instance: String, target: RemoteTarget): Pair<String, String>? = runCatching {
        require(payload.toByteArray().size <= 2048)
        val uri = URI(payload)
        require(uri.scheme == "cinema-remote" && uri.host == "pair" && uri.userInfo == null && uri.port == -1 && uri.path.isNullOrEmpty())
        val pairs = requireNotNull(uri.rawQuery).split('&').map { piece -> val pair = piece.split('=', limit = 2); require(pair.size == 2); URLDecoder.decode(pair[0], "UTF-8") to URLDecoder.decode(pair[1], "UTF-8") }
        require(pairs.size == 5 && pairs.map { it.first }.distinct().size == 5)
        val values = pairs.toMap(); require(values.keys == setOf("server_instance_id", "owner_node_id", "session_id", "receiver_epoch", "challenge_id"))
        require(values["server_instance_id"] == instance && values["owner_node_id"] == target.owner_node_id && RemoteWire.uuid(values.getValue("session_id")) == target.session_id && RemoteWire.uuid(values.getValue("receiver_epoch")) == target.receiver_epoch)
        val fragment = requireNotNull(uri.rawFragment); require(Regex("code=[0-9]{8}").matches(fragment))
        RemoteWire.uuid(values.getValue("challenge_id")) to fragment.removePrefix("code=")
    }.getOrNull()
    fun bitmap(challenge: JsonObject): Bitmap? = runCatching {
        val rows = challenge["qr_modules"]?.takeUnless { it == JsonNull }?.jsonArray?.map { it.jsonPrimitive.content } ?: return null
        require(rows.size in 21..177 && rows.all { it.length == rows.size && it.all { ch -> ch == '0' || ch == '1' } })
        val scale = 6; val quiet = 4; val size = (rows.size + quiet * 2) * scale
        Bitmap.createBitmap(size, size, Bitmap.Config.ARGB_8888).also { bitmap ->
            bitmap.eraseColor(android.graphics.Color.WHITE)
            rows.forEachIndexed { y, row -> row.forEachIndexed { x, ch -> if (ch == '1') repeat(scale) { dy -> repeat(scale) { dx -> bitmap.setPixel((x + quiet) * scale + dx, (y + quiet) * scale + dy, android.graphics.Color.BLACK) } } } }
        }
    }.getOrNull()
}
@Composable
internal fun RemoteRootOverlay(remote: RemoteClientModel, television: Boolean) {
    if (television) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.TopEnd) {
            if (remote.enabled && remote.target != null) TextButton(onClick = remote::startPairing, modifier = Modifier.padding(12.dp)) { Text("Pair a phone") }
        }
        val pending = remote.pendingPairings.firstOrNull()
        val challenge = remote.challenge
        if (pending != null || challenge != null || remote.pairingOpening) {
            RemoteRestricted()
            AlertDialog(onDismissRequest = { if (pending == null) remote.hidePairing() },
                title = { Text(if (pending != null) "Approve this phone?" else "Pair a phone") },
                text = { Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                    if (pending != null) Text(RemoteWire.safeLabel(pending.string("controller_name"), 80, "Phone"))
                    else if (remote.pairingOpening) { CircularProgressIndicator(); Text("Getting a TV pairing code…") }
                    else if (challenge != null) {
                        Text("Choose this TV in Cinema on your phone, then enter this code or scan the QR.")
                        RemotePairingQr.bitmap(challenge)?.let { Image(it.asImageBitmap(), "TV pairing QR", Modifier.size(240.dp)) }
                        Text(challenge.string("code"), style = MaterialTheme.typography.headlineLarge)
                        Text("Expires in two minutes. Approval stays on this TV.")
                    }
                } },
                confirmButton = { TextButton(onClick = { if (pending != null) remote.approve(pending, true) else remote.hidePairing() }) { Text(if (pending != null) "Approve" else "Close") } },
                dismissButton = { if (pending != null) TextButton(onClick = { remote.approve(pending, false) }) { Text("Decline") } })
        }
    } else if (remote.remotePresented) {
        RemoteRestricted()
        Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) { RemoteCompanion(remote) }
    } else if (remote.suggestions.isNotEmpty()) {
        Box(Modifier.fillMaxSize(), contentAlignment = Alignment.BottomCenter) {
            Card(Modifier.widthIn(max = 600.dp).fillMaxWidth().padding(18.dp)) {
                Column(Modifier.padding(18.dp), verticalArrangement = Arrangement.spacedBy(10.dp)) {
                    val suggestions = remote.suggestions
                    Text(if (suggestions.size == 1) "Use this phone with " + suggestions.first().name else "Paired screens are available")
                    Button(onClick = { if (suggestions.size == 1) remote.select(suggestions.first()) else { remote.closeController(); remote.remotePresented = true } }) { Text(if (suggestions.size == 1) "Open remote" else "Choose a screen") }
                    TextButton(onClick = remote::dismissSuggestion) { Text("Dismiss for this session") }
                    if (suggestions.size == 1) Row(verticalAlignment = Alignment.CenterVertically) {
                        val device = suggestions.first(); var enabled by remember(device.id) { mutableStateOf(remote.suggestionsEnabled(device.id)) }
                        Checkbox(checked = enabled, onCheckedChange = { enabled = it; remote.setSuggestions(device.id, it) })
                        Text("Suggest this screen")
                    }
                }
            }
        }
    }
}
@Composable
private fun RemoteCompanion(remote: RemoteClientModel) {
    val context = LocalContext.current
    var code by remember { mutableStateOf("") }
    var challenge by remember { mutableStateOf<String?>(null) }
    var search by remember { mutableStateOf("") }
    var scannerMessage by remember { mutableStateOf("") }
    val selected = remote.selected
    val nonce = remote.state?.get("text_nonce")?.takeUnless { it == JsonNull }?.jsonPrimitive?.contentOrNull
    LaunchedEffect(selected?.id, selected?.target) { code = ""; challenge = null }
    LaunchedEffect(remote.scanRevision, selected?.id, selected?.target) {
        remote.scannedPairing?.let { parsed -> challenge = parsed.first; code = parsed.second }
    }
    LaunchedEffect(nonce) { search = "" }
    DisposableEffect(remote) { onDispose { remote.stopHolding(); remote.closeController() } }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(16.dp)) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
            Text("Cinema remote", style = MaterialTheme.typography.headlineMedium)
            TextButton(onClick = { remote.releaseControl(); remote.remotePresented = false }) { Text("Close") }
        }
        if (selected == null) {
            Text(remote.status)
            remote.devices.forEach { device -> Button(enabled = device.available, onClick = { remote.select(device) }) { Text(device.name + if (!device.available) " · Offline" else if (device.busy) " · In use" else "") } }
            if (remote.devices.isEmpty()) Text("Enable Cinema remotes in Developer settings on both devices and on the server.")
            if (remote.unavailableNodes.isNotEmpty()) Text("Some server nodes are unavailable.")
        } else {
            Text(selected.name, style = MaterialTheme.typography.titleLarge)
            TextButton(onClick = { remote.closeController() }) { Text("Choose another screen") }
            if (!remote.isPaired) {
                Text("Choose Pair a phone on the TV. Enter its eight-digit code or scan the QR.")
                OutlinedTextField(code, onValueChange = { value -> if (value.all { it in '0'..'9' } && value.length <= 8) { code = value; challenge = null } }, label = { Text("TV code") })
                TextButton(onClick = {
                    val ticket = remote.beginQrScan()
                    if (ticket != null) {
                        val options = GmsBarcodeScannerOptions.Builder().setBarcodeFormats(Barcode.FORMAT_QR_CODE).enableAutoZoom().build()
                        GmsBarcodeScanning.getClient(context, options).startScan().addOnSuccessListener { barcode ->
                            if (remote.finishQrScan(ticket, barcode.rawValue.orEmpty())) { scannerMessage = "TV code scanned. Request pairing after returning." }
                            else scannerMessage = "Choose the same authenticated TV and scan its current code again."
                        }.addOnFailureListener { scannerMessage = "Scanner unavailable. Enter the TV code instead." }
                    }
                }) { Text("Scan TV QR") }
                Button(enabled = code.length == 8, onClick = { remote.pair(code, challenge) }) { Text("Request pairing") }
                Text(scannerMessage)
            } else {
                if (!remote.controlling) {
                    Text(if (remote.controlledByOther) (remote.control?.name ?: "Another phone") + " is controlling this screen." else "Tap Use as remote to request or resume control.")
                    Button(onClick = { remote.acquire(remote.controlledByOther) }) { Text(if (remote.controlledByOther) "Take over" else "Use as remote") }
                }
                Text(remote.state?.get("focused_label")?.takeUnless { it == JsonNull }?.jsonPrimitive?.contentOrNull ?: "Waiting for safe TV focus")
                RemoteDirectionPad(remote)
                Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                    listOf("back" to "Back", "home" to "Home", "stop" to "Stop").forEach { (action, label) -> if (remote.supports(action)) Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction(action)) }) { Text(label) } }
                }
                val playback = remote.state?.get("playback")?.takeUnless { it == JsonNull }?.jsonObject
                if (playback != null) {
                    Text(playback.string("title"), style = MaterialTheme.typography.titleLarge)
                    Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
                        if (remote.supports("seek_relative")) Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction("seek_relative", buildJsonObject { put("seconds", -10) })) }) { Text("−10 s") }
                        if (remote.supports("set_playing")) Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction("set_playing", buildJsonObject { put("playing", !playback.getValue("playing").jsonPrimitive.boolean) })) }) { Text(if (playback.getValue("playing").jsonPrimitive.boolean) "Pause" else "Play") }
                        if (remote.supports("seek_relative")) Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction("seek_relative", buildJsonObject { put("seconds", 10) })) }) { Text("+10 s") }
                    }
                    val options = playback.getValue("tracks").jsonArray.map { it.jsonObject }
                    options.map { it.string("kind") }.distinct().forEach { kind ->
                        if (remote.supports("open_tracks")) Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction("open_tracks", buildJsonObject { put("kind", kind) })) }) { Text("Choose " + kind) }
                        if (remote.state?.get("route")?.jsonPrimitive?.content == "tracks" && remote.supports("choose_track")) options.filter { it.string("kind") == kind }.forEach { option ->
                            Button(enabled = remote.controlling, onClick = { remote.send(RemoteAction("choose_track", buildJsonObject { put("kind", kind); put("option_id", option.string("option_id")) })) }) { Text(option.string("label")) }
                        }
                    }
                }
                if (nonce != null && remote.supports("text_replace")) {
                    OutlinedTextField(search, onValueChange = { if (it.toByteArray().size <= 512) search = it }, label = { Text("Text for TV search") })
                    Button(enabled = remote.controlling, onClick = { if (remote.state?.get("text_nonce")?.jsonPrimitive?.contentOrNull == nonce) remote.send(RemoteAction("text_replace", buildJsonObject { put("text_nonce", nonce); put("text", search) })) }) { Text("Send search text") }
                }
            }
        }
        Text(remote.commandStatus)
    }
}
@Composable
private fun RemoteDirectionPad(remote: RemoteClientModel) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, verticalArrangement = Arrangement.spacedBy(12.dp)) {
        RemoteDirection(remote, "up", "Up")
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp), verticalAlignment = Alignment.CenterVertically) {
            RemoteDirection(remote, "left", "Left")
            Button(enabled = remote.controlling && remote.supports("select"), onClick = { remote.send(RemoteAction("select")) }, modifier = Modifier.size(92.dp, 72.dp)) { Text("Select") }
            RemoteDirection(remote, "right", "Right")
        }
        RemoteDirection(remote, "down", "Down")
    }
}
@Composable
private fun RemoteDirection(remote: RemoteClientModel, direction: String, label: String) {
    val enabled = remote.controlling && remote.supports("navigate")
    val send = { remote.send(RemoteAction("navigate", buildJsonObject { put("direction", direction) })) }
    Surface(Modifier.size(72.dp).pointerInput(direction, enabled) {
        if (enabled) detectTapGestures(onPress = { remote.beginHolding(direction); try { tryAwaitRelease() } finally { remote.stopHolding() } })
    }.semantics { contentDescription = label; role = Role.Button; onClick { if (enabled) send(); enabled } }, shape = MaterialTheme.shapes.large, color = MaterialTheme.colorScheme.secondaryContainer) {
        Box(contentAlignment = Alignment.Center) { Text(label) }
    }
}
@Composable
internal fun RemoteDeveloperCard() {
    val remote = LocalRemoteClient.current ?: return
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text("Cinema remotes", style = MaterialTheme.typography.titleLarge)
        Row(verticalAlignment = Alignment.CenterVertically) { Checkbox(remote.enabled, onCheckedChange = remote::saveEnabled); Text("Enable foreground receiver and companion") }
        Text("Waiting on real TV/phone focus, menus, lazy scrolling, mixed physical input and cross-platform HTTP acceptance. Background invitations are a separate future opt-in. This switch always saves; readiness stays advisory.")
        Text("Graduates to Remotes & devices after acceptance.")
    }
}
@Composable
internal fun RemoteDeviceSettings(remote: RemoteClientModel, onBack: () -> Unit) {
    RemoteRestricted()
    var revision by remember { mutableIntStateOf(0) }
    LaunchedEffect(remote, revision) { remote.refreshGrants() }
    Column(Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(24.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        TextButton(onClick = onBack) { Text("Back") }; Text("Remotes & devices", style = MaterialTheme.typography.headlineMedium)
        TextButton(onClick = { remote.closeController(); remote.remotePresented = true }) { Text("Open Cinema remote") }
        remote.localGrants.forEach { grant ->
            Text(remote.devices.firstOrNull { it.id == grant.receiverId }?.name ?: "Paired screen")
            TextButton(onClick = { remote.revokeGrant(grant.id); revision++ }) { Text("Revoke this phone's pairing") }
            TextButton(onClick = { remote.forgetGrant(grant.id); revision++ }) { Text("Forget saved pairing") }
        }
        remote.grantList.forEach { grant -> Text(grant.string("name")); TextButton(onClick = { remote.revokeGrant(grant.string("id")); revision++ }) { Text("Revoke pairing") } }
        TextButton(onClick = remote::refreshGrants) { Text("Refresh pairings") }
        if (remote.localReceiverId != null) { Text("Reset revokes this TV installation and all paired phones."); TextButton(onClick = { remote.resetReceiver(); revision++ }) { Text("Reset TV remote registration") } }
        Text(remote.managementStatus)
    }
}
