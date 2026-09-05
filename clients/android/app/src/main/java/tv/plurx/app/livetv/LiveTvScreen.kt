@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import android.view.ViewGroup
import androidx.activity.compose.BackHandler
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
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
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import tv.plurx.app.data.Session
import tv.plurx.app.ui.components.safeDisplayInsets
import androidx.compose.foundation.layout.windowInsetsPadding

@Composable
fun LiveTvScreen(origin: String, onBack: () -> Unit) {
    val controller = LiveTvPlayer.get(LocalContext.current)
    val state by controller.state.collectAsStateWithLifecycle()
    var search by remember { mutableStateOf("") }
    var fullscreen by remember { mutableStateOf(false) }
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    LaunchedEffect(origin) { controller.load(origin, Session.token.orEmpty()) }
    LaunchedEffect(state.playing) { if (!state.playing) fullscreen = false }
    DisposableEffect(controller) { onDispose { controller.stop() } }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { controller.stop() }
    BackHandler(fullscreen) { fullscreen = false }

    Column(Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(16.dp)) {
        if (!fullscreen) {
            TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text("Live TV", style = MaterialTheme.typography.headlineMedium)
        }
        Text(state.title, style = MaterialTheme.typography.titleMedium)
        Text(state.message)
        if (state.playing) {
            Box(if (fullscreen) Modifier.weight(1f).fillMaxWidth().background(Color.Black)
                else Modifier.fillMaxWidth().heightIn(min = 160.dp, max = 260.dp).background(Color.Black)) {
                AndroidView(factory = { context -> PlayerView(context).apply {
                    layoutParams = ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT)
                    useController = false
                    isFocusable = false
                    descendantFocusability = ViewGroup.FOCUS_BLOCK_DESCENDANTS
                    keepScreenOn = true
                } }, update = { it.player = controller.player }, modifier = Modifier.fillMaxSize())
            }
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                Button(onClick = controller::togglePause) { Text(if (state.paused) "Play live" else "Pause") }
                TextButton(onClick = controller::toggleMute) { Text(if (state.muted) "Unmute" else "Mute") }
                TextButton(onClick = { fullscreen = !fullscreen }) { Text(if (fullscreen) "Exit fullscreen" else "Fullscreen") }
                TextButton(onClick = { controller.stop() }) { Text("Stop") }
            }
        } else TextButton(onClick = { controller.stop() }) { Text("Stop / retry cleanup") }
        if (!fullscreen) {
            FlowRow {
                TextButton(enabled = !state.busy, onClick = { controller.load(origin, Session.token.orEmpty()) }) { Text("Reload channels") }
            }
            OutlinedTextField(search, onValueChange = { search = it }, label = { Text("Find a channel") }, modifier = Modifier.fillMaxWidth().tvFocusRing(), singleLine = true)
            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp), modifier = Modifier.weight(1f)) {
                items(state.channels.filter { it.title.contains(search, ignoreCase = true) }, key = { it.id }) { channel ->
                    Column(Modifier.fillMaxWidth().padding(vertical = 8.dp)) {
                        Text(channel.title)
                        if (channel.favorite) Text("Favorite", style = MaterialTheme.typography.labelSmall)
                        Button(onClick = { controller.watch(channel) }, enabled = channel.watchable) {
                            Text(if (channel.watchable) "Watch live" else "DRM unsupported")
                        }
                    }
                }
            }
        }
    }
}
