package tv.plurx.app.ui

import androidx.activity.compose.BackHandler
import androidx.compose.foundation.layout.*
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch
import tv.plurx.app.data.SharedPlaybackPlan
import tv.plurx.app.data.SharedPlaybackReference
import tv.plurx.app.player.SharedPlayerController
import tv.plurx.app.remote.*
import tv.plurx.app.ui.components.TvButton
import java.util.UUID

/** The authenticated file choice is a preparation, never an implicit Start. */
@Composable
internal fun SharedRemotePreplay(vm: AppViewModel, reference: SharedPlaybackReference, fileId: String,
    onBack: () -> Unit, onPlaying: (SharedPlaybackPlan, SharedPlayerController) -> Unit) {
    val context = LocalContext.current
    val controller = remember(reference, fileId) { SharedPlayerController(context, vm) }
    val navigation = LocalRemoteNavigation.current
    val client = LocalRemoteClient.current
    val token = remember(reference, fileId) { "shared-preplay:" + UUID.randomUUID().toString() }
    val scope = rememberCoroutineScope()
    var live by remember { mutableStateOf(true) }
    var transferred by remember { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var message by remember { mutableStateOf("Press Play to start this Shared file.") }
    var attempt by remember { mutableLongStateOf(0) }
    val currentBack by rememberUpdatedState(onBack)
    val currentPlaying by rememberUpdatedState(onPlaying)
    val binding = remember(token, controller) { RemoteDeferredBinding(token, token, reference.toString() + "|" + fileId) }
    var completedPlan by remember { mutableStateOf<SharedPlaybackPlan?>(null) }
    fun owned() = live && !transferred && navigation?.scope == token
    fun exit(home: Boolean = false): RemoteOutcome {
        if (!owned()) return RemoteOutcome.StaleContext
        attempt++; controller.beginStop(); live = false
        if (home) navigation?.onHome?.invoke() else currentBack()
        return RemoteOutcome.Applied
    }
    suspend fun prepareAndStart(check: () -> Boolean, physical: Boolean): RemoteOutcome {
        if (busy || !owned() || !check()) return RemoteOutcome.Unavailable
        val operation = ++attempt
        busy = true
        try {
            val plan = vm.prepareSharedPlayback(reference, fileId) { owned() && attempt == operation && check() }
            if (!owned() || attempt != operation || !check()) return RemoteOutcome.Unavailable
            val outcome = if (physical) controller.startPreparedPhysical(plan) { owned() && attempt == operation && check() }
                else controller.startRemote(plan) { owned() && attempt == operation && check() }
            if (outcome == RemoteOutcome.Applied) completedPlan = plan
            else message = "Playback could not be confirmed. Press Play to prepare this file again."
            return outcome
        } finally { if (attempt == operation) busy = false }
    }
    fun complete(outcome: RemoteOutcome) {
        val plan = completedPlan
        if (outcome == RemoteOutcome.Applied && owned() && plan != null) {
            transferred = true; currentPlaying(plan, controller)
        }
    }
    fun deferred(action: RemoteAction): RemoteDeferredEffect? {
        if (action.type != "select" && (action.type != "set_playing" || action.boolean("playing") != true)) return null
        return RemoteDeferredEffect(binding, ::owned, { check -> prepareAndStart(check, false) }, ::complete)
    }
    DisposableEffect(navigation, token) {
        navigation?.enter(token, "details") { exit(); true }
        onDispose { live = false; attempt++; navigation?.releaseScope(token); if (!transferred) controller.close() }
    }
    SideEffect {
        client?.playback?.attach(RemotePlaybackAdapter.Owner(token, token, { setOf("set_playing", "stop", "back", "home") }, { null }, { action ->
            when (action.type) { "stop", "back" -> exit(); "home" -> exit(true); else -> RemoteOutcome.Unsupported }
        }, { if (!transferred) controller.retireNetworkOperation() }, ::owned, { action -> if (action.type == "select") null else deferred(action) }))
    }
    DisposableEffect(client, token) { onDispose { client?.playback?.detach(token) } }
    BackHandler { exit() }
    CompositionLocalProvider(LocalRemoteScope provides token) {
        RemoteOrder(listOf("shared-preplay:back", "shared-preplay:play"))
        Column(Modifier.fillMaxSize().padding(20.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
            Text("Shared file ready")
            Text(message)
            if (busy) Text("Starting Shared playback")
            TvButton(modifier = Modifier.remoteAction("shared-preplay:back", "Back") { exit() }, onClick = { exit() }) { Text("Back") }
            TvButton(modifier = Modifier.remoteAction("shared-preplay:play", "Play", deferred = ::deferred) { RemoteOutcome.Unsupported }, onClick = { scope.launch {
                client?.physicalInput()
                try { complete(prepareAndStart(::owned, true)) }
                catch (cancel: kotlinx.coroutines.CancellationException) { throw cancel }
                catch (_: Exception) { if (owned()) message = "Playback could not be confirmed. Press Play to try again." }
            } }) { Text("Play") }
        }
    }
}
