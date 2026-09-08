@file:androidx.annotation.OptIn(androidx.media3.common.util.UnstableApi::class)

package tv.plurx.app.livetv

import android.app.PictureInPictureParams
import android.content.pm.PackageManager
import android.os.Build
import android.util.Rational
import android.view.ViewGroup
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.LocalActivity
import androidx.core.util.Consumer
import androidx.core.app.PictureInPictureModeChangedInfo
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
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
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import tv.plurx.app.ui.components.TvButton as Button
import tv.plurx.app.ui.components.TvTextButton as TextButton
import tv.plurx.app.ui.components.RequestInitialFocus
import tv.plurx.app.ui.components.tvFocusRing
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.foundation.clickable
import androidx.compose.foundation.interaction.MutableInteractionSource
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableLongStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.viewinterop.AndroidView
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LifecycleEventEffect
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.media3.ui.PlayerView
import kotlinx.coroutines.launch
import tv.plurx.app.data.Session
import tv.plurx.app.data.SettingsStore
import tv.plurx.app.ui.components.safeDisplayInsets
import tv.plurx.app.ui.FormFactor
import tv.plurx.app.ui.currentFormFactor
import androidx.compose.foundation.layout.windowInsetsPadding
import kotlinx.coroutines.delay

@OptIn(androidx.compose.material3.ExperimentalMaterial3Api::class)
@Composable
fun LiveTvScreen(origin: String, onBack: () -> Unit) {
    val context = LocalContext.current
    val controller = LiveTvPlayer.get(context)
    val state by controller.state.collectAsStateWithLifecycle()
    var search by remember { mutableStateOf("") }
    var fullscreen by remember { mutableStateOf(false) }
    val settings = remember(context) { SettingsStore(context) }
    val persistedView by settings.liveTvView.collectAsStateWithLifecycle(initialValue = null)
    var browse by remember { mutableStateOf(LiveTvBrowseView.List) }
    // The stored choice arrives asynchronously; adopt it once, then let taps
    // own the value. Re-adopting on every emission would fight the viewer.
    LaunchedEffect(persistedView) {
        persistedView?.let { browse = LiveTvBrowseView.fromStorage(it) }
    }
    var favoritesOnly by remember { mutableStateOf(false) }
    var hideProtected by remember { mutableStateOf(false) }
    var detail by remember { mutableStateOf<Pair<LiveTvChannel, LiveTvProgramme>?>(null) }
    var overlayVisible by remember { mutableStateOf(true) }
    var lastInteraction by remember { mutableIntStateOf(0) }
    var isInPip by remember { mutableStateOf(false) }
    var now by remember { mutableLongStateOf(System.currentTimeMillis() / 1000) }
    val scope = rememberCoroutineScope()
    val backFocus = remember { FocusRequester() }
    RequestInitialFocus(backFocus)
    val formFactor = currentFormFactor()
    val television = formFactor == FormFactor.Television
    val activity = LocalActivity.current
    val componentActivity = activity as? ComponentActivity
    val canUsePip = Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
        context.packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)

    // Key on the token as well: `Session.token` is a plain global, not Compose
    // state, so a profile switch that keeps this screen composed would leave
    // the application-scoped controller heartbeating the previous profile's
    // capability under the new profile's session.
    val token = Session.token.orEmpty()
    LaunchedEffect(origin, token) { controller.load(origin, token) }
    // The ten-foot surface is always fullscreen. Shipping the phone layout with
    // a Fullscreen button in front of it meant the overlay rendered inside a
    // 260 dp thumbnail with a second, separately focusable channel list beneath
    // it — two D-pad targets fighting over the same presses.
    LaunchedEffect(state.playing, television) {
        if (television) fullscreen = state.playing else if (!state.playing) fullscreen = false
    }
    // The contract's four seconds. Without this the overlay never hid at all,
    // so `ten-foot · overlay · idle → hide` was a row nothing implemented.
    LaunchedEffect(lastInteraction, overlayVisible, state.playing, state.paused, isInPip) {
        if (!overlayVisible || !state.playing || state.paused || isInPip) return@LaunchedEffect
        delay(LiveTvInputPolicy.HIDE_AFTER_MS)
        overlayVisible = false
    }
    LaunchedEffect(Unit) {
        while (true) {
            delay(30_000)
            now = System.currentTimeMillis() / 1000
        }
    }

    // Entering picture-in-picture drives this activity to ON_STOP, and the old
    // rule released the tuner there — which would have killed the exact case
    // picture-in-picture exists for. The predicate lives on the controller
    // because the screen is precisely the thing that is going away when it
    // matters; an ordinary back-out, with no PiP, still releases.
    LaunchedEffect(isInPip) { controller.setRetained(isInPip) }
    DisposableEffect(controller) { onDispose { controller.stopUnlessRetained() } }
    LifecycleEventEffect(Lifecycle.Event.ON_STOP) { controller.stopUnlessRetained() }
    BackHandler(fullscreen) { fullscreen = false }

    DisposableEffect(componentActivity, canUsePip) {
        val target = componentActivity
        if (!canUsePip || target == null) {
            onDispose { }
        } else {
            val listener = Consumer<PictureInPictureModeChangedInfo> { info ->
                isInPip = info.isInPictureInPictureMode
                controller.setRetained(info.isInPictureInPictureMode)
                // Restore the chrome on the way out, exactly as the VOD screen
                // does. Clearing it with no `else` left the expanded window
                // with no title, no progress, no channel strip and no Exit,
                // permanently — the only recovery was leaving Live TV, which
                // releases the tuner.
                overlayVisible = !info.isInPictureInPictureMode
                lastInteraction += 1
            }
            target.addOnPictureInPictureModeChangedListener(listener)
            onDispose { target.removeOnPictureInPictureModeChangedListener(listener) }
        }
    }

    // Remembered on its inputs. `filter` runs `airing` per channel — a linear
    // scan of that channel's programmes — so recomputing it on every
    // recomposition made one keystroke in the search field O(channels ×
    // programmes).
    val visible = remember(state.channels, state.guide, search, favoritesOnly, hideProtected, now) {
        LiveTvGuideReducer.filter(
            channels = state.channels,
            guide = state.guide,
            options = LiveTvChannelFilter(search, favoritesOnly, hideProtected),
            now = now,
        )
    }

    fun enterPip() {
        val target = componentActivity ?: return
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        // Retain only if the system actually took us into PiP. Setting the flag
        // first and discarding the Boolean latched it permanently whenever PiP
        // was refused — which it is on any device where the user has turned
        // picture-in-picture off for this app — and every lifecycle release
        // path is gated on that flag. The result was a keepalive loop renewing
        // the household's only tuner against a screen nobody was on, with no
        // way back to the Stop button.
        val entered = target.enterPictureInPictureMode(
            PictureInPictureParams.Builder().setAspectRatio(Rational(16, 9)).build(),
        )
        controller.setRetained(entered)
        if (!entered) {
            controller.report("Picture-in-picture is turned off for plurx in Android settings.")
        }
    }

    // Every press decided by the shared table, exactly as the web page and the
    // Apple clients do. The one ruling this has to preserve: a direction on a
    // hidden overlay only reveals it — it never changes channel behind a
    // picture nobody can see.
    // Named `applyOutcome`, not `apply`: a local function called `apply`
    // shadows kotlin.apply inside this scope, and this file uses `.apply {}`
    // on the PlayerView a few lines below.
    fun applyOutcome(outcome: LiveTvInputOutcome): Boolean {
        when (outcome) {
            LiveTvInputOutcome.Reveal -> { overlayVisible = true; lastInteraction += 1 }
            LiveTvInputOutcome.Hide -> overlayVisible = false
            LiveTvInputOutcome.ToggleChrome -> { overlayVisible = !overlayVisible; lastInteraction += 1 }
            LiveTvInputOutcome.TogglePlay -> { controller.togglePause(); lastInteraction += 1 }
            LiveTvInputOutcome.Exit -> {
                if (television) return false
                fullscreen = false
            }
            LiveTvInputOutcome.ChannelUp, LiveTvInputOutcome.ChannelDown -> {
                val delta = if (outcome == LiveTvInputOutcome.ChannelUp) -1 else 1
                val next = LiveTvGuideReducer.adjacent(visible.map { it.id }, state.watching?.id, delta)
                    ?.let { id -> visible.firstOrNull { it.id == id } }
                if (next != null) controller.requestChannel(next)
                lastInteraction += 1
            }
            // Focus movement and activation belong to the focus engine and the
            // button's own onClick; the table names them so this stays
            // exhaustive and a future row cannot land silently.
            LiveTvInputOutcome.FocusRow, LiveTvInputOutcome.Activate -> lastInteraction += 1
            LiveTvInputOutcome.StripPrev, LiveTvInputOutcome.StripNext,
            LiveTvInputOutcome.Tune, LiveTvInputOutcome.Ignore -> return false
        }
        return true
    }

    val surface = if (television) LiveTvInputSurface.TenFoot else LiveTvInputSurface.Touch
    fun inputState(): LiveTvInputState = when {
        !television && !fullscreen -> LiveTvInputState.Page
        overlayVisible -> LiveTvInputState.Overlay
        else -> LiveTvInputState.Hidden
    }

    Column(
        Modifier.fillMaxSize().windowInsetsPadding(safeDisplayInsets()).padding(16.dp)
            // Key spellings and the handler that installs them both live in
            // LiveTvKeyAdapter, so this screen can only speak the contract's
            // vocabulary. A press on a hidden overlay is consumed whatever it
            // decides, so the focus engine cannot move focus behind a picture
            // that is showing no chrome.
            .liveTvInputAdapter(enabled = state.playing && !isInPip) { input ->
                applyOutcome(LiveTvInputPolicy.route(surface, inputState(), input))
            },
    ) {
        if (!fullscreen && !isInPip) {
            TextButton(onClick = onBack, modifier = Modifier.focusRequester(backFocus)) { Text("Back") }
            Text("Live TV", style = MaterialTheme.typography.headlineMedium)
        }
        if (!isInPip) {
            Text(state.title, style = MaterialTheme.typography.titleMedium)
            Text(state.message)
        }
        if (state.playing) {
            val tap = remember { MutableInteractionSource() }
            val picture = if (fullscreen || isInPip) {
                Modifier.weight(1f).fillMaxWidth().background(Color.Black)
            } else {
                Modifier.fillMaxWidth().heightIn(min = 160.dp, max = 260.dp).background(Color.Black)
            }
            Box(
                // A tap toggles the chrome, which is the touch table's whole
                // contract for this surface. Never on a television, where the
                // D-pad owns it and a tap would be a phantom press.
                if (television) picture else picture.clickable(
                    interactionSource = tap,
                    indication = null,
                ) { applyOutcome(LiveTvInputPolicy.route(surface, inputState(), LiveTvContractInput.TapSurface)) },
            ) {
                AndroidView(
                    factory = { ctx ->
                        PlayerView(ctx).apply {
                            layoutParams = ViewGroup.LayoutParams(
                                ViewGroup.LayoutParams.MATCH_PARENT,
                                ViewGroup.LayoutParams.MATCH_PARENT,
                            )
                            useController = false
                            isFocusable = false
                            descendantFocusability = ViewGroup.FOCUS_BLOCK_DESCENDANTS
                            keepScreenOn = true
                        }
                    },
                    update = { it.player = controller.player },
                    modifier = Modifier.fillMaxSize(),
                )
                if ((fullscreen || television) && overlayVisible && !isInPip) {
                    LiveTvOverlay(
                        channel = state.watching,
                        airing = state.watching?.let { controller.airing(it, now) } ?: LiveTvAiring(),
                        neighbours = visible,
                        onSelect = { controller.requestChannel(it) },
                        onExit = { fullscreen = false },
                    )
                }
            }
            if (!isInPip) {
                LiveTvNowBar(
                    channel = state.watching,
                    airing = state.watching?.let { controller.airing(it, now) } ?: LiveTvAiring(),
                    paused = state.paused,
                    muted = state.muted,
                    fullscreen = fullscreen,
                    canUsePip = canUsePip,
                    onTogglePause = controller::togglePause,
                    onToggleMute = controller::toggleMute,
                    onToggleFullscreen = { fullscreen = !fullscreen },
                    onPip = ::enterPip,
                    onStop = { controller.stop() },
                )
            }
        } else if (!isInPip) {
            TextButton(onClick = { controller.stop() }) { Text("Stop / retry cleanup") }
        }
        if (!fullscreen && !isInPip) {
            FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(enabled = !state.busy, onClick = { controller.load(origin, token) }) {
                    Text("Reload channels")
                }
                // A focus-navigable half-hour grid is a milestone of its own on
                // each ten-foot platform. Until then the television gets the
                // list, which the focus engine already handles.
                if (!television) {
                    TextButton(onClick = {
                        browse = if (browse == LiveTvBrowseView.List) {
                            LiveTvBrowseView.Guide
                        } else {
                            LiveTvBrowseView.List
                        }
                        scope.launch { settings.saveLiveTvView(browse.storage) }
                    }) { Text(if (browse == LiveTvBrowseView.List) "Guide" else "On now") }
                }
                TextButton(onClick = { favoritesOnly = !favoritesOnly }) {
                    Text(if (favoritesOnly) "All channels" else "Favorites")
                }
                TextButton(onClick = { hideProtected = !hideProtected }) {
                    Text(if (hideProtected) "Show protected" else "Hide protected")
                }
            }
            OutlinedTextField(
                search,
                onValueChange = { search = it },
                label = { Text("Number, name, or what is on") },
                modifier = Modifier.fillMaxWidth().tvFocusRing(),
                singleLine = true,
            )
            if (browse == LiveTvBrowseView.Guide && !television) {
                val window = remember(now) { LiveTvGuideReducer.window(now) }
                // The layout allocates the whole row/cell tree; without this it
                // did so on every recomposition, including every keystroke.
                val layout = remember(state.guide, visible, window, now) {
                    LiveTvGuideReducer.gridLayout(
                        guide = state.guide,
                        channels = visible,
                        window = window,
                        now = now,
                        pxPerSlot = LiveTvGridMetrics.slotWidth.value,
                    )
                }
                LiveTvGuideGrid(
                    layout = layout,
                    slots = remember(window) { LiveTvGuideReducer.gridSlots(window) },
                    playingChannelId = state.watching?.id,
                    onAiring = { controller.watch(it) },
                    onFuture = { channel, programme -> detail = channel to programme },
                    modifier = Modifier.weight(1f),
                )
            } else {
                LazyColumn(
                    verticalArrangement = Arrangement.spacedBy(8.dp),
                    modifier = Modifier.weight(1f),
                ) {
                    items(visible, key = { it.id }) { channel ->
                        LiveTvChannelRow(
                            channel = channel,
                            airing = controller.airing(channel, now),
                            selected = state.watching?.id == channel.id,
                            onWatch = { controller.watch(channel) },
                        )
                    }
                }
            }
        }
    }

    // A future programme gets details and no actions. There is no DVR behind
    // this, so offering "record" would be offering something that does not
    // exist.
    // A real sheet. Emitted bare, this drew programme text straight over the
    // channel list with no background, no scrim, no outside-tap dismiss and no
    // scroll — so a long synopsis (guide text is relayed third-party content
    // and only the 2 MiB document cap bounds it) pushed Close off the screen
    // and the only way out was system Back, which drops the tuner.
    detail?.let { (channel, programme) ->
        ModalBottomSheet(onDismissRequest = { detail = null }) {
            LiveTvProgrammeDetail(channel, programme) { detail = null }
        }
    }
}

@Composable
private fun LiveTvNowBar(
    channel: LiveTvChannel?,
    airing: LiveTvAiring,
    paused: Boolean,
    muted: Boolean,
    fullscreen: Boolean,
    canUsePip: Boolean,
    onTogglePause: () -> Unit,
    onToggleMute: () -> Unit,
    onToggleFullscreen: () -> Unit,
    onPip: () -> Unit,
    onStop: () -> Unit,
) {
    Column(Modifier.fillMaxWidth()) {
        Text(
            airing.now?.title ?: channel?.guide_name ?: "Live television",
            style = MaterialTheme.typography.titleMedium,
            maxLines = 1,
            overflow = TextOverflow.Ellipsis,
        )
        airing.now?.let { programme ->
            LinearProgressIndicator(
                progress = { airing.progress ?: 0f },
                modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp),
            )
            Text(
                "${liveTvTime(programme.start)}–${liveTvTime(programme.end)}" +
                    (airing.next?.let { " · Next: ${it.title}" } ?: ""),
                style = MaterialTheme.typography.labelSmall,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
        }
        FlowRow(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(onClick = onTogglePause) { Text(if (paused) "Play live" else "Pause") }
            TextButton(onClick = onToggleMute) { Text(if (muted) "Unmute" else "Mute") }
            if (canUsePip) TextButton(onClick = onPip) { Text("Picture-in-picture") }
            TextButton(onClick = onToggleFullscreen) {
                Text(if (fullscreen) "Exit fullscreen" else "Fullscreen")
            }
            TextButton(onClick = onStop) { Text("Stop") }
        }
    }
}

/**
 * The overlay over the picture: channel and programme, a progress bar, and a
 * strip of neighbouring channels. On a television every press reaches it
 * already decided by the shared contract table.
 */
@Composable
private fun LiveTvOverlay(
    channel: LiveTvChannel?,
    airing: LiveTvAiring,
    neighbours: List<LiveTvChannel>,
    onSelect: (LiveTvChannel) -> Unit,
    onExit: () -> Unit,
) {
    Column(
        Modifier
            .fillMaxSize()
            .background(Color(0x66000000))
            .padding(16.dp),
        verticalArrangement = Arrangement.SpaceBetween,
    ) {
        Column {
            Text(
                airing.now?.title ?: channel?.guide_name ?: "Live television",
                style = MaterialTheme.typography.titleLarge,
                color = Color.White,
            )
            Text(channel?.title.orEmpty(), style = MaterialTheme.typography.labelMedium, color = Color.White)
            airing.now?.let {
                Text(
                    "${liveTvTime(it.start)}–${liveTvTime(it.end)}" +
                        (airing.next?.let { next -> " · Next: ${next.title}" } ?: ""),
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.White,
                )
            }
        }
        Column {
            LinearProgressIndicator(
                progress = { airing.progress ?: 0f },
                modifier = Modifier.fillMaxWidth().padding(vertical = 6.dp),
            )
            Row(
                Modifier.horizontalScroll(rememberScrollState()),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                neighbours.take(7).forEach { entry ->
                    TextButton(onClick = { onSelect(entry) }, enabled = entry.watchable) {
                        Text(entry.title, style = MaterialTheme.typography.labelSmall, color = Color.White)
                    }
                }
            }
            TextButton(onClick = onExit) { Text("Exit", color = Color.White) }
        }
    }
}

@Composable
private fun LiveTvProgrammeDetail(
    channel: LiveTvChannel,
    programme: LiveTvProgramme,
    onClose: () -> Unit,
) {
    Column(Modifier.fillMaxWidth().verticalScroll(rememberScrollState()).padding(16.dp)) {
        Text(programme.title, style = MaterialTheme.typography.titleMedium)
        Text(
            "${channel.title} · ${liveTvTime(programme.start)}–${liveTvTime(programme.end)}" +
                (programme.episode?.let { " · $it" } ?: ""),
            style = MaterialTheme.typography.labelSmall,
        )
        programme.episode_title?.let { Text(it, style = MaterialTheme.typography.bodyMedium) }
        programme.synopsis?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
        if (programme.filters.isNotEmpty()) {
            Text(programme.filters.joinToString(" · "), style = MaterialTheme.typography.labelSmall)
        }
        Text("Live only — plurx does not record.", style = MaterialTheme.typography.labelSmall)
        TextButton(onClick = onClose) { Text("Close") }
    }
}
